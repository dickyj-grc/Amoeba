//! Builds a container image on this host from a Git repository or a pasted
//! Dockerfile, for deploys that start from source (the web console's Git and
//! Dockerfile tabs). The resulting tag is what the app package then runs.

use std::collections::VecDeque;
use std::path::Path;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::Notify;

use crate::apps::deploy::{image_reference, nixpacks_args};

const CLONE_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_BUILD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
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

/// What a build is doing, for the status endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildState {
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

/// A snapshot of one build for `GET /admin/apps/<name>/build`.
#[derive(Debug, Clone, Serialize)]
pub struct BuildStatus {
    pub state: BuildState,
    /// The command currently running, e.g. `docker build -t amoeba/x:latest .`.
    pub step: String,
    pub elapsed_seconds: u64,
    /// The newest log lines, oldest first.
    pub log: Vec<String>,
}

struct ProgressInner {
    log: VecDeque<String>,
    step: String,
    state: BuildState,
    finished: Option<Instant>,
}

/// Live state of one build, shared between the request running it and the
/// status endpoint the console polls. Command output is appended line by
/// line as it arrives, keeping the last `MAX_LOG_LINES` lines.
pub struct BuildProgress {
    started: Instant,
    inner: Mutex<ProgressInner>,
    cancel_requested: AtomicBool,
    cancel: Notify,
}

impl BuildProgress {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            started: Instant::now(),
            inner: Mutex::new(ProgressInner {
                log: VecDeque::new(),
                step: String::new(),
                state: BuildState::Running,
                finished: None,
            }),
            cancel_requested: AtomicBool::new(false),
            cancel: Notify::new(),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ProgressInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn push(&self, line: impl Into<String>) {
        let mut inner = self.lock();
        inner.log.push_back(line.into());
        while inner.log.len() > MAX_LOG_LINES {
            inner.log.pop_front();
        }
    }

    fn set_step(&self, step: String) {
        self.push(format!("$ {step}"));
        self.lock().step = step;
    }

    /// Records the final state; later calls are ignored.
    pub fn finish(&self, state: BuildState) {
        let mut inner = self.lock();
        if inner.state == BuildState::Running {
            inner.state = state;
            inner.finished = Some(Instant::now());
        }
    }

    pub fn is_running(&self) -> bool {
        self.lock().state == BuildState::Running
    }

    /// Asks the running command to stop; it is killed and the build fails
    /// as cancelled.
    pub fn cancel(&self) {
        self.cancel_requested.store(true, Ordering::SeqCst);
        self.cancel.notify_waiters();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel_requested.load(Ordering::SeqCst)
    }

    async fn cancelled(&self) {
        loop {
            let notified = self.cancel.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_cancelled() {
                return;
            }
            notified.await;
        }
    }

    /// The state plus at most the last `tail` log lines.
    pub fn status(&self, tail: usize) -> BuildStatus {
        let inner = self.lock();
        let end = inner.finished.unwrap_or_else(Instant::now);
        let skip = inner.log.len().saturating_sub(tail);
        BuildStatus {
            state: inner.state,
            step: inner.step.clone(),
            elapsed_seconds: end.duration_since(self.started).as_secs(),
            log: inner.log.iter().skip(skip).cloned().collect(),
        }
    }

    fn log(&self) -> Vec<String> {
        self.lock().log.iter().cloned().collect()
    }

    fn fail(&self, message: impl Into<String>) -> BuildFailure {
        BuildFailure {
            message: message.into(),
            log: self.log(),
        }
    }
}

/// Appends each line from `pipe` to the build log as it arrives.
fn stream_lines(
    pipe: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    progress: Arc<BuildProgress>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(pipe).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            progress.push(line);
        }
    })
}

async fn run(
    program: &str,
    args: &[String],
    cwd: &Path,
    timeout: Duration,
    progress: &Arc<BuildProgress>,
) -> Result<(), String> {
    progress.set_step(format!("{program} {}", args.join(" ")));
    let mut command = Command::new(program);
    // Own process group, so a cancel or timeout can stop the children it
    // starts too (docker's build helpers, a shell's commands).
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command
        .args(args)
        .current_dir(cwd)
        .env("GIT_TERMINAL_PROMPT", "0")
        // Line-by-line BuildKit output instead of a redrawn terminal view.
        .env("BUILDKIT_PROGRESS", "plain")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("{program} failed to start: {e}"))?;
    let readers = [
        child
            .stdout
            .take()
            .map(|p| stream_lines(p, progress.clone())),
        child
            .stderr
            .take()
            .map(|p| stream_lines(p, progress.clone())),
    ];

    let outcome = tokio::select! {
        status = child.wait() => status.map_err(|e| format!("{program} failed: {e}")),
        _ = tokio::time::sleep(timeout) => Err(format!("{program} timed out after {}s", timeout.as_secs())),
        _ = progress.cancelled() => Err("cancelled".to_string()),
    };
    if outcome.is_err() {
        kill_group(&mut child).await;
    }
    for reader in readers.into_iter().flatten() {
        // A process outside the group could still hold the pipe open; after a
        // kill, stop reading rather than wait for it.
        if outcome.is_err() {
            let abort = reader.abort_handle();
            if tokio::time::timeout(Duration::from_secs(1), reader)
                .await
                .is_err()
            {
                abort.abort();
            }
        } else {
            let _ = reader.await;
        }
    }

    let status = outcome?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "{program} exited with code {}",
            status.code().unwrap_or(-1)
        ))
    }
}

/// Kills the command and everything in its process group.
async fn kill_group(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        let _ = Command::new("kill")
            .args(["-KILL", &format!("-{pid}")])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
    let _ = child.kill().await;
}

/// Build timeout: `AMOEBA_BUILD_TIMEOUT_SECS`, or 15 minutes. Large projects
/// (a browser engine, say) can need longer.
pub fn build_timeout() -> Duration {
    std::env::var("AMOEBA_BUILD_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|&secs| secs > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_BUILD_TIMEOUT)
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
    progress: &Arc<BuildProgress>,
) -> Result<BuildOutcome, BuildFailure> {
    let log = progress;
    let build_timeout = build_timeout();
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
                log,
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
                    if let Err(e) = run("git", &args, temp.path(), CLONE_TIMEOUT, log).await {
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
    if let Err(e) = run(program, &args, &ctx, build_timeout, log).await {
        return Err(log.fail(format!("build failed: {e}")));
    }

    if registry.is_some_and(|r| !r.trim().is_empty()) {
        let push = vec!["push".to_string(), tag.clone()];
        if let Err(e) = run("docker", &push, &ctx, build_timeout, log).await {
            return Err(log.fail(format!("push failed: {e}")));
        }
    }

    log.push(format!("Built {tag}"));
    Ok(BuildOutcome {
        image: tag,
        log: log.log(),
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
        let progress = BuildProgress::new();
        for i in 0..(MAX_LOG_LINES + 10) {
            progress.push(i.to_string());
        }
        let log = progress.log();
        assert_eq!(log.len(), MAX_LOG_LINES);
        assert_eq!(log[0], "10");
        assert_eq!(progress.status(3).log, ["507", "508", "509"]);
    }

    // @lat: [[deploy#Source builds#Live progress]]
    #[tokio::test]
    async fn output_is_visible_while_running_and_cancel_stops_the_command() {
        let progress = BuildProgress::new();
        let args: Vec<String> = vec!["-c".into(), "echo cloning; sleep 30".into()];
        let running = {
            let progress = progress.clone();
            tokio::spawn(async move {
                run(
                    "sh",
                    &args,
                    Path::new("."),
                    Duration::from_secs(60),
                    &progress,
                )
                .await
            })
        };

        // The first line shows up before the command finishes.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !progress.log().iter().any(|l| l == "cloning") {
            assert!(
                Instant::now() < deadline,
                "no live output: {:?}",
                progress.log()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let status = progress.status(10);
        assert_eq!(status.state, BuildState::Running);
        assert!(status.step.starts_with("sh -c"));

        let started = Instant::now();
        progress.cancel();
        let result = running.await.unwrap();
        assert_eq!(result, Err("cancelled".to_string()));
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "sleep was not killed"
        );
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
        let outcome = build_image("amoeba-build-test", &request, None, &BuildProgress::new())
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
        let outcome = build_image(
            "amoeba-git-build-test",
            &request,
            None,
            &BuildProgress::new(),
        )
        .await
        .unwrap_or_else(|f| panic!("{}\n{}", f.message, f.log.join("\n")));
        assert_eq!(outcome.image, "amoeba/amoeba-git-build-test:latest");
        assert!(outcome.log.iter().any(|l| l.starts_with("$ git clone")));
    }
}
