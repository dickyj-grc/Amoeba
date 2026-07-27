//! Container lifecycle management: cold-boot, cooldown, scale-to-zero.

pub mod compose;
pub mod container;
pub mod docker_container;
pub mod driver;
pub mod reaper;
