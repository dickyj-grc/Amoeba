//! Builds a container image on this host from a Git repository or a pasted
//! Dockerfile, for deploys that start from source (the web console's Git and
//! Dockerfile tabs). The resulting tag is what the app package then runs.

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use tokio::process::Command;

use crate::apps::deploy::{image_reference, nixpacks_args};

const CLONE_TIMEOUT: Duration = Duration::from_secs(120);
const BUILD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const MAX_DOCKERFILE_BYTES: usize = 64 * 1024;
/// Only the tail of a long build is returned; the error is almost always there.
const MAX_LOG_LINES: usize = 500;

/// What to build. Sent as JSON with a `source` tag.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum BuildRequest {
    /// Clone the repository and build it with its own Dockerfile, or with
    /// nixpacks when it has none and nixpacks is installed.
    Git {
        url: String,
        #[serde(default)]
        r#ref: Option<String>,
    },
    /// Build the given Dockerfile. `context_url` is cloned first and used as
    /// the build context, so `COPY` has files to copy; its own Dockerfile, if
    /// any, is replaced.
    Dockerfile {
        dockerfile: String,
        #[serde(default)]
        context_url: Option<String>,
        #[serde(default)]
        context_ref: Option<String>,
    },
}

impl BuildRequest {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            BuildRequest::Git { url, r#ref } => {
                validate_git_url(url)?;
                if let Some(r) = r#ref {
                    validate_git_ref(r)?;
                }
            }
            BuildRequest::Dockerfile {
                dockerfile,
                context_url,
                context_ref,
            } => {
                if dockerfile.trim().is_empty() {
                    return Err("dockerfile is empty".into());
                }
                if dockerfile.len() > MAX_DOCKERFILE_BYTES {
                    return Err("dockerfile is larger than 64 KiB".into());
                }
                if let Some(url) = context_url {
                    validate_git_url(url)?;
                }
                if let Some(r) = context_ref {
                    if context_url.is_none() {
                        return Err("context_ref needs context_url".into());
                    }
                    validate_git_ref(r)?;
                }
            }
        }
        Ok(())
    }
}

/// The name becomes the image tag (`amoeba/<name>:latest`), and Docker
/// repository names must be lowercase.
pub fn validate_image_name(name: &str) -> Result<(), String> {
    let mut chars = name.chars();
    let ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
        && name.len() <= 128;
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid service name {name:?}: use lowercase letters, digits, '-', '_' or '.'"
        ))
    }
}

/// Only `https://` URLs. That rules out `file://` (reading the host's own
/// repositories), `ext::` (running a command), and anything git would read
/// as an option.
pub fn validate_git_url(url: &str) -> Result<(), String> {
    let rest = url
        .strip_prefix("https://")
        .ok_or_else(|| format!("git URL must start with https:// (got {url:?})"))?;
    if rest.is_empty() || url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(format!("invalid git URL {url:?}"));
    }
    Ok(())
}

pub fn validate_git_ref(r: &str) -> Result<(), String> {
    let ok = !r.is_empty()
        && !r.starts_with('-')
        && !r.contains("..")
        && r.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
    if ok {
        Ok(())
    } else {
        Err(format!("invalid git ref {r:?}"))
    }
}

/// Shallow clone of one branch. `--` keeps the URL from being read as an option.
pub fn git_clone_args(url: &str, r#ref: Option<&str>, dest: &Path) -> Vec<String> {
    let mut args: Vec<String> = vec!["clone".into(), "--depth".into(), "1".into()];
    if let Some(r) = r#ref {
        args.extend(["--branch".into(), r.into()]);
    }
    args.extend(["--".into(), url.into(), dest.to_string_lossy().into_owned()]);
    args
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Builder {
    Docker,
    Nixpacks,
}

/// A repository's own Dockerfile wins; nixpacks is the fallback.
pub fn choose_builder(has_dockerfile: bool, nixpacks_available: bool) -> Result<Builder, String> {
    match (has_dockerfile, nixpacks_available) {
        (true, _) => Ok(Builder::Docker),
        (false, true) => Ok(Builder::Nixpacks),
        (false, false) => {
            Err("the repository has no Dockerfile at its root and nixpacks is not installed".into())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildOutcome {
    pub image: String,
    pub log: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildFailure {
    pub message: String,
    pub log: Vec<String>,
}

/// Collects command output, keeping only the last `MAX_LOG_LINES` lines.
#[derive(Default)]
struct BuildLog(Vec<String>);

impl BuildLog {
    fn push(&mut self, line: impl Into<String>) {
        self.0.push(line.into());
        if self.0.len() > MAX_LOG_LINES {
            let excess = self.0.len() - MAX_LOG_LINES;
            self.0.drain(..excess);
        }
    }

    fn fail(self, message: impl Into<String>) -> BuildFailure {
        BuildFailure {
            message: message.into(),
            log: self.0,
        }
    }
}

async fn run(
    program: &str,
    args: &[String],
    cwd: &Path,
    timeout: Duration,
    log: &mut BuildLog,
) -> Result<(), String> {
    log.push(format!("$ {program} {}", args.join(" ")));
    let child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(timeout, child)
        .await
        .map_err(|_| format!("{program} timed out after {}s", timeout.as_secs()))?
        .map_err(|e| format!("{program} failed to start: {e}"))?;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        log.push(line);
    }
    for line in String::from_utf8_lossy(&output.stderr).lines() {
        log.push(line);
    }
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{program} exited with code {}",
            output.status.code().unwrap_or(-1)
        ))
    }
}

async fn nixpacks_available() -> bool {
    Command::new("nixpacks")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .is_ok_and(|s| s.success())
}

/// Builds the image for `name` and pushes it when `registry` is set. Each
/// build runs in its own temporary directory, removed afterwards.
pub async fn build_image(
    name: &str,
    request: &BuildRequest,
    registry: Option<&str>,
) -> Result<BuildOutcome, BuildFailure> {
    let mut log = BuildLog::default();
    let temp = match tempfile::tempdir() {
        Ok(dir) => dir,
        Err(e) => return Err(log.fail(format!("failed to create build dir: {e}"))),
    };
    let ctx = temp.path().join("src");

    let clone = |url: &str, r: Option<&str>| git_clone_args(url, r, &ctx);
    let builder = match request {
        BuildRequest::Git { url, r#ref } => {
            if let Err(e) = run(
                "git",
                &clone(url, r#ref.as_deref()),
                temp.path(),
                CLONE_TIMEOUT,
                &mut log,
            )
            .await
            {
                return Err(log.fail(format!("clone failed: {e}")));
            }
            match choose_builder(ctx.join("Dockerfile").is_file(), nixpacks_available().await) {
                Ok(builder) => builder,
                Err(e) => return Err(log.fail(e)),
            }
        }
        BuildRequest::Dockerfile {
            dockerfile,
            context_url,
            context_ref,
        } => {
            match context_url {
                Some(url) => {
                    let args = clone(url, context_ref.as_deref());
                    if let Err(e) = run("git", &args, temp.path(), CLONE_TIMEOUT, &mut log).await {
                        return Err(log.fail(format!("clone failed: {e}")));
                    }
                }
                None => {
                    if let Err(e) = std::fs::create_dir_all(&ctx) {
                        return Err(log.fail(format!("failed to create build dir: {e}")));
                    }
                }
            }
            if let Err(e) = std::fs::write(ctx.join("Dockerfile"), dockerfile) {
                return Err(log.fail(format!("failed to write Dockerfile: {e}")));
            }
            Builder::Docker
        }
    };

    let tag = image_reference(registry, name);
    let (program, args) = match builder {
        Builder::Docker => ("docker", docker_build_args(&tag)),
        Builder::Nixpacks => ("nixpacks", nixpacks_args(&tag)),
    };
    if let Err(e) = run(program, &args, &ctx, BUILD_TIMEOUT, &mut log).await {
        return Err(log.fail(format!("build failed: {e}")));
    }

    if registry.is_some_and(|r| !r.trim().is_empty()) {
        let push = vec!["push".to_string(), tag.clone()];
        if let Err(e) = run("docker", &push, &ctx, BUILD_TIMEOUT, &mut log).await {
            return Err(log.fail(format!("push failed: {e}")));
        }
    }

    log.push(format!("Built {tag}"));
    Ok(BuildOutcome {
        image: tag,
        log: log.0,
    })
}

pub fn docker_build_args(tag: &str) -> Vec<String> {
    vec!["build".into(), "-t".into(), tag.into(), ".".into()]
}

#[cfg(test)]
mod tests {
    use super::*;

    // @lat: [[deploy#Source builds#Input checks]]
    #[test]
    fn rejects_unsafe_sources() {
        assert!(validate_git_url("https://github.com/acme/api.git").is_ok());
        for bad in [
            "file:///etc",
            "ext::sh -c touch% /tmp/pwned",
            "--upload-pack=touch /tmp/x",
            "git@github.com:acme/api.git",
            "http://github.com/acme/api.git",
            "https://",
            "https://github.com/acme/api.git x",
        ] {
            assert!(validate_git_url(bad).is_err(), "{bad}");
        }

        assert!(validate_git_ref("main").is_ok());
        assert!(validate_git_ref("release/v1.2").is_ok());
        for bad in ["", "-b", "a..b", "x y", "a;b"] {
            assert!(validate_git_ref(bad).is_err(), "{bad}");
        }

        assert!(validate_image_name("pdf-parser").is_ok());
        for bad in ["", "PdfParser", "a/b", "-x", "../x"] {
            assert!(validate_image_name(bad).is_err(), "{bad}");
        }

        let empty = BuildRequest::Dockerfile {
            dockerfile: " ".into(),
            context_url: None,
            context_ref: None,
        };
        assert!(empty.validate().is_err());
        let orphan_ref = BuildRequest::Dockerfile {
            dockerfile: "FROM scratch".into(),
            context_url: None,
            context_ref: Some("main".into()),
        };
        assert!(orphan_ref.validate().is_err());
    }

    #[test]
    fn clone_puts_the_url_after_double_dash() {
        let args = git_clone_args("https://x/y.git", Some("main"), Path::new("/tmp/b/src"));
        assert_eq!(
            args,
            [
                "clone",
                "--depth",
                "1",
                "--branch",
                "main",
                "--",
                "https://x/y.git",
                "/tmp/b/src"
            ]
        );
    }

    #[test]
    fn repository_dockerfile_wins_over_nixpacks() {
        assert_eq!(choose_builder(true, true), Ok(Builder::Docker));
        assert_eq!(choose_builder(true, false), Ok(Builder::Docker));
        assert_eq!(choose_builder(false, true), Ok(Builder::Nixpacks));
        assert!(choose_builder(false, false).is_err());
    }

    #[test]
    fn parses_tagged_requests() {
        let git: BuildRequest =
            serde_json::from_str(r#"{"source":"git","url":"https://x/y.git","ref":"dev"}"#)
                .unwrap();
        assert_eq!(
            git,
            BuildRequest::Git {
                url: "https://x/y.git".into(),
                r#ref: Some("dev".into())
            }
        );
        let df: BuildRequest =
            serde_json::from_str(r#"{"source":"dockerfile","dockerfile":"FROM scratch"}"#).unwrap();
        assert!(matches!(
            df,
            BuildRequest::Dockerfile {
                context_url: None,
                ..
            }
        ));
    }

    #[test]
    fn log_keeps_the_tail() {
        let mut log = BuildLog::default();
        for i in 0..(MAX_LOG_LINES + 10) {
            log.push(i.to_string());
        }
        assert_eq!(log.0.len(), MAX_LOG_LINES);
        assert_eq!(log.0[0], "10");
    }

    /// Needs a running Docker daemon: `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore]
    async fn builds_a_pasted_dockerfile() {
        let request = BuildRequest::Dockerfile {
            dockerfile: "FROM busybox:1.36\nCMD [\"true\"]\n".into(),
            context_url: None,
            context_ref: None,
        };
        let outcome = build_image("amoeba-build-test", &request, None)
            .await
            .unwrap();
        assert_eq!(outcome.image, "amoeba/amoeba-build-test:latest");
    }

    /// Needs Docker and network access: `cargo test -- --ignored`. The repo's
    /// Dockerfile `ADD`s a file, so this also checks the clone is the context.
    #[tokio::test]
    #[ignore]
    async fn builds_a_git_repository() {
        let request = BuildRequest::Git {
            url: "https://github.com/crccheck/docker-hello-world".into(),
            r#ref: None,
        };
        let outcome = build_image("amoeba-git-build-test", &request, None)
            .await
            .unwrap_or_else(|f| panic!("{}\n{}", f.message, f.log.join("\n")));
        assert_eq!(outcome.image, "amoeba/amoeba-git-build-test:latest");
        assert!(outcome.log.iter().any(|l| l.starts_with("$ git clone")));
    }
}
