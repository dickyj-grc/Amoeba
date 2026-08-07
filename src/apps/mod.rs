//! Amoeba App Package support.
//!
//! An App Package is a self-contained bundle (zip/folder) that describes a
//! deployable service: metadata, a Docker Compose file or container image,
//! permissions, resource limits, and a schema for environment variables and
//! secrets. The package manager in this module installs and uninstalls apps
//! by materializing the package onto disk and updating Amoeba's service
//! catalog (`services.json`), which Amoeba hot-reloads without a restart.

pub mod manager;
pub mod schema;
