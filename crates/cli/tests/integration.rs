//! Cross-crate end-to-end tests for the cli crate.
//!
//! Exercises the config→ModelConfig→completions_url seam and the
//! /project detection against fixture directories.

use std::io::Write;

fn cargo_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"fixture\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    let mut f = std::fs::File::create(dir.path().join("src").join("main.rs")).unwrap();
    f.write_all(b"fn main() {}\n").unwrap();
    dir
}

fn node_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("package.json"), "{}\n").unwrap();
    let mut f = std::fs::File::create(dir.path().join("index.js")).unwrap();
    f.write_all(b"console.log('hi');\n").unwrap();
    dir
}

fn python_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("pyproject.toml"), "").unwrap();
    let mut f = std::fs::File::create(dir.path().join("main.py")).unwrap();
    f.write_all(b"print('hi')\n").unwrap();
    dir
}

fn cmake_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("CMakeLists.txt"),
        "cmake_minimum_required(VERSION 3.10)\n",
    )
    .unwrap();
    dir
}

fn go_fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("go.mod"), "module example\n").unwrap();
    let mut f = std::fs::File::create(dir.path().join("main.go")).unwrap();
    f.write_all(b"package main\n").unwrap();
    dir
}

fn empty_fixture() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// ───────────────────────────────────────────────────────────────────
// 1. Config → ModelConfig → completions_url
// ───────────────────────────────────────────────────────────────────

#[test]
fn config_to_model_config_produces_correct_completions_url() {
    let raw = r#"
model = "test-model"
endpoint = "http://localhost:11434/v1"
max_concurrency = 2
timeout_ms = 5000
max_retries = 1
max_context_tokens = 8192
"#;
    let config = requiem_cli::config::Config::parse(raw).unwrap();
    let mc = config.to_model_config();
    assert_eq!(mc.endpoint, "http://localhost:11434/v1");
    assert_eq!(mc.model, "test-model");
    // endpoint already ends with /v1 → URL is {endpoint}/chat/completions
    assert_eq!(
        mc.completions_url(),
        "http://localhost:11434/v1/chat/completions"
    );
}

#[test]
fn config_endpoint_without_v1_gets_v1_appended() {
    let raw = r#"
model = "test-model"
endpoint = "http://localhost:8080"
"#;
    let config = requiem_cli::config::Config::parse(raw).unwrap();
    let mc = config.to_model_config();
    assert_eq!(
        mc.completions_url(),
        "http://localhost:8080/v1/chat/completions"
    );
}

#[test]
fn config_endpoint_with_trailing_slash_normalised() {
    let raw = r#"
model = "test-model"
endpoint = "http://localhost:11434/v1/"
"#;
    let config = requiem_cli::config::Config::parse(raw).unwrap();
    let mc = config.to_model_config();
    assert_eq!(
        mc.completions_url(),
        "http://localhost:11434/v1/chat/completions"
    );
}

// ───────────────────────────────────────────────────────────────────
// 2. /project detection against fixture directories
// ───────────────────────────────────────────────────────────────────

#[test]
fn detect_cargo_project_from_fixture() {
    let dir = cargo_fixture();
    let profile = requiem_project::detect(dir.path()).unwrap();
    assert_eq!(profile.ecosystem, requiem_project::Ecosystem::Cargo);
    assert_eq!(profile.build_command.as_deref(), Some("cargo build"));
    assert!(profile.languages.contains(&"Rust".to_string()));
}

#[test]
fn detect_node_project_from_fixture() {
    let dir = node_fixture();
    let profile = requiem_project::detect(dir.path()).unwrap();
    assert_eq!(profile.ecosystem, requiem_project::Ecosystem::Node);
    assert_eq!(profile.test_command.as_deref(), Some("npm test"));
}

#[test]
fn detect_python_project_from_fixture() {
    let dir = python_fixture();
    let profile = requiem_project::detect(dir.path()).unwrap();
    assert_eq!(profile.ecosystem, requiem_project::Ecosystem::Python);
    assert!(profile.languages.contains(&"Python".to_string()));
}

#[test]
fn detect_cmake_project_from_fixture() {
    let dir = cmake_fixture();
    let profile = requiem_project::detect(dir.path()).unwrap();
    assert_eq!(profile.ecosystem, requiem_project::Ecosystem::Cmake);
}

#[test]
fn detect_go_project_from_fixture() {
    let dir = go_fixture();
    let profile = requiem_project::detect(dir.path()).unwrap();
    assert_eq!(profile.ecosystem, requiem_project::Ecosystem::Go);
}

#[test]
fn detect_empty_dir_is_unknown() {
    let dir = empty_fixture();
    let profile = requiem_project::detect(dir.path()).unwrap();
    assert_eq!(profile.ecosystem, requiem_project::Ecosystem::Unknown);
}

#[test]
fn detect_git_repo_flag() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]\n").unwrap();
    std::fs::create_dir_all(dir.path().join(".git")).unwrap();
    let profile = requiem_project::detect(dir.path()).unwrap();
    assert!(profile.has_git);
}
