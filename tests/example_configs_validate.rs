//! Confirms every shipped example config actually parses and passes
//! Amoeba's own schema validation (not just JSON syntax) against the new
//! nested placement/container/stack_spec shape.

use amoeba::config::watcher::load_catalog;

fn assert_valid(path: &str) {
    let result = load_catalog(path);
    assert!(result.is_ok(), "{path} failed to load: {:?}", result.err());
}

#[test]
fn services_example_json_is_valid() {
    assert_valid("config/services.example.json");
}

#[test]
fn services_docker_example_json_is_valid() {
    assert_valid("config/services.docker.example.json");
}

#[test]
fn services_local_example_json_is_valid() {
    assert_valid("config/services.local.example.json");
}

#[test]
fn services_cloud_example_json_is_valid() {
    assert_valid("config/services.cloud.example.json");
}

#[test]
fn services_capacity_example_json_is_valid() {
    assert_valid("config/services.capacity.example.json");
}
