//! CLI contract tests: arguments, system locations, outputs, and exit statuses.

use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

use serde_json::Value;
use tempfile::tempdir;

fn invoke(directory: &Path, arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_refyard"))
        .current_dir(directory)
        .args([
            "--registry",
            "refyard.registry.yaml",
            "--state-dir",
            ".refyard",
        ])
        .args(arguments)
        .output()
        .unwrap()
}

fn success(directory: &Path, arguments: &[&str]) -> Value {
    let output = invoke(directory, arguments);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn complete_cli_workflow_tracks_and_cleans_resources() {
    let directory = tempdir().unwrap();
    let root = directory.path();
    fs::create_dir(root.join("repos")).unwrap();
    fs::create_dir(root.join("repos/project")).unwrap();
    fs::write(
        root.join("repos/project/refyard.yaml"),
        "requires:\n  - id: hf:model\n    revision: abc\n",
    )
    .unwrap();
    fs::write(root.join("used"), "weights").unwrap();
    fs::write(root.join("unused"), "unused weights").unwrap();
    success(
        root,
        &[
            "--json",
            "registry",
            "add",
            "hf:model",
            "--revision",
            "abc",
            "--path",
            "used",
            "--notes",
            "Managed externally",
        ],
    );
    success(
        root,
        &["--json", "registry", "add", "unused", "--path", "unused"],
    );
    success(
        root,
        &["--json", "repo", "add", "repos/project/refyard.yaml"],
    );
    let scan = success(root, &["--json", "scan", "--full", "repos"]);
    assert_eq!(scan["full_scan"], true);
    let refs = success(root, &["--json", "refs", "hf:model", "--revision", "abc"]);
    assert_eq!(refs[0]["projects"].as_array().unwrap().len(), 1);
    assert_eq!(
        success(root, &["--json", "projects"])[0]["requires"][0]["id"],
        "hf:model"
    );
    assert_eq!(
        success(root, &["--json", "resources"])
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        success(root, &["--json", "graph"])["projects"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let preview = success(root, &["--json", "cleanup"]);
    assert_eq!(preview["dry_run"], true);
    assert!(root.join("unused").exists());
    let deleted = success(root, &["--json", "cleanup", "--delete"]);
    assert_eq!(deleted["candidates"][0]["deleted"], true);
    assert!(!root.join("unused").exists());
    assert!(root.join("used").exists());
    success(root, &["--json", "registry", "remove", "unused"]);
    assert!(invoke(root, &["--json", "validate"]).status.success());
    success(root, &["--json", "scan"]);
    let duplicate = invoke(
        root,
        &[
            "registry",
            "add",
            "hf:model",
            "--revision",
            "abc",
            "--path",
            "used",
        ],
    );
    assert!(!duplicate.status.success());
    assert_eq!(
        success(root, &["--json", "registry", "list"])["artifacts"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn validation_and_incomplete_scans_have_nonzero_status_with_structured_diagnostics() {
    let directory = tempdir().unwrap();
    let root = directory.path();
    fs::create_dir(root.join("project")).unwrap();
    fs::write(
        root.join("project/refyard.yaml"),
        "requires:\n  - id: unknown\n",
    )
    .unwrap();
    success(root, &["--json", "repo", "add", "project"]);
    let validation = invoke(root, &["--json", "validate"]);
    assert_eq!(validation.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&validation.stdout).unwrap();
    assert_eq!(report["issues"][0]["kind"], "unregistered_resource");
    fs::remove_dir_all(root.join("project")).unwrap();
    let scan = invoke(root, &["--json", "scan"]);
    assert_eq!(scan.status.code(), Some(1));
    let report: Value = serde_json::from_slice(&scan.stdout).unwrap();
    assert_eq!(report["issues"].as_array().unwrap().len(), 1);
    success(root, &["--json", "repo", "remove", "project"]);
    assert_eq!(
        success(root, &["--json", "repo", "list"])["repositories"],
        serde_json::json!([])
    );
}

#[test]
fn cli_rejects_unsafe_full_scan_options_and_removed_init_command() {
    let directory = tempdir().unwrap();
    let root = directory.path();
    assert_eq!(
        invoke(root, &["scan", "--full", "--ignore-hidden", "."])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(
        invoke(root, &["scan", "--full", "--respect-gitignore", "."])
            .status
            .code(),
        Some(2)
    );
    assert_eq!(invoke(root, &["init"]).status.code(), Some(2));
    assert!(invoke(root, &["--help"]).status.success());
    assert!(invoke(root, &["cleanup", "--help"]).status.success());
}

#[test]
#[cfg(target_os = "linux")]
fn machine_configuration_uses_system_locations_across_working_directories() {
    let directory = tempdir().unwrap();
    let root = directory.path();
    let output = Command::new(env!("CARGO_BIN_EXE_refyard"))
        .current_dir(root)
        .env("HOME", root.join("home"))
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME")
        .env_remove("REFYARD_REGISTRY")
        .env_remove("REFYARD_STATE_DIR")
        .args(["--json", "registry", "list"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        root.join("home/.config/refyard/refyard.registry.yaml")
            .exists()
    );
    assert!(root.join("home/.local/state/refyard/state.lock").exists());
    assert!(!root.join("refyard.registry.yaml").exists());
    assert!(!root.join(".refyard").exists());
    let another_repository = root.join("another-repository");
    fs::create_dir(&another_repository).unwrap();
    let lookup = Command::new(env!("CARGO_BIN_EXE_refyard"))
        .current_dir(&another_repository)
        .env("HOME", root.join("home"))
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_STATE_HOME")
        .env_remove("REFYARD_REGISTRY")
        .env_remove("REFYARD_STATE_DIR")
        .args(["--json", "registry", "list"])
        .output()
        .unwrap();
    assert!(lookup.status.success());
}

#[test]
fn explicit_flags_select_registry_and_state_locations() {
    let directory = tempdir().unwrap();
    let root = directory.path();
    let overridden = Command::new(env!("CARGO_BIN_EXE_refyard"))
        .current_dir(root)
        .env("REFYARD_REGISTRY", root.join("ignored/registry.yaml"))
        .env("REFYARD_STATE_DIR", root.join("ignored/state"))
        .args([
            "--registry",
            "override.yaml",
            "--state-dir",
            "override-state",
            "registry",
            "list",
        ])
        .output()
        .unwrap();
    assert!(overridden.status.success());
    assert!(root.join("override.yaml").exists());
    assert!(root.join("override-state/state.lock").exists());
}

#[test]
fn environment_overrides_system_locations_and_flags_override_environment() {
    let directory = tempdir().unwrap();
    let root = directory.path();
    let command_with_overrides = || {
        let mut command = Command::new(env!("CARGO_BIN_EXE_refyard"));
        command
            .current_dir(root)
            .env("REFYARD_REGISTRY", root.join("machine/registry.yaml"))
            .env("REFYARD_STATE_DIR", root.join("machine/state"));
        command
    };
    let automatic = command_with_overrides()
        .args(["--json", "registry", "list"])
        .output()
        .unwrap();
    assert!(
        automatic.status.success(),
        "{}",
        String::from_utf8_lossy(&automatic.stderr)
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&automatic.stdout).unwrap()["artifacts"],
        serde_json::json!([])
    );
    assert!(root.join("machine/registry.yaml").exists());
    assert!(root.join("machine/state/state.lock").exists());
    let registry_override = command_with_overrides()
        .args(["--registry", "alternate.yaml", "registry", "list"])
        .output()
        .unwrap();
    assert!(registry_override.status.success());
    assert!(root.join("alternate.yaml").exists());
    let state_override = command_with_overrides()
        .args(["--state-dir", "alternate-state", "registry", "list"])
        .output()
        .unwrap();
    assert!(state_override.status.success());
    assert!(root.join("alternate-state/state.lock").exists());
}

#[test]
fn automatic_registry_creation_preserves_existing_valid_and_invalid_contents() {
    let directory = tempdir().unwrap();
    let root = directory.path();
    assert_eq!(
        success(root, &["--json", "registry", "list"])["artifacts"],
        serde_json::json!([])
    );
    let registry = root.join("refyard.registry.yaml");
    let existing = "# Keep my notes\nartifacts:\n  - id: model\n    path: /external/model\n";
    fs::write(&registry, existing).unwrap();
    assert_eq!(
        success(root, &["--json", "registry", "list"])["artifacts"][0]["id"],
        "model"
    );
    assert_eq!(fs::read_to_string(&registry).unwrap(), existing);
    let invalid = "artifacts: [invalid";
    fs::write(&registry, invalid).unwrap();
    let output = invoke(root, &["registry", "list"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Invalid YAML"));
    assert_eq!(fs::read_to_string(&registry).unwrap(), invalid);
}

#[test]
fn registry_creation_failure_reports_the_location_without_replacing_existing_files() {
    let directory = tempdir().unwrap();
    let root = directory.path();
    fs::write(root.join("not-a-directory"), "existing contents").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_refyard"))
        .current_dir(root)
        .args([
            "--registry",
            "not-a-directory/registry.yaml",
            "--state-dir",
            "state",
            "registry",
            "list",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("Could not access the registry"));
    assert_eq!(
        fs::read_to_string(root.join("not-a-directory")).unwrap(),
        "existing contents"
    );
}

#[test]
#[cfg(unix)]
fn automatic_registry_creation_does_not_replace_a_dangling_symlink() {
    use std::os::unix::fs::symlink;
    let directory = tempdir().unwrap();
    let root = directory.path();
    symlink(
        root.join("missing-target"),
        root.join("refyard.registry.yaml"),
    )
    .unwrap();
    assert_eq!(invoke(root, &["registry", "list"]).status.code(), Some(1));
    assert!(
        fs::symlink_metadata(root.join("refyard.registry.yaml"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(!root.join("missing-target").exists());
}

#[test]
#[cfg(target_os = "linux")]
fn system_xdg_locations_and_individual_overrides_are_resolved_independently() {
    let directory = tempdir().unwrap();
    let root = directory.path();
    let invoke_with_system_directories = |arguments: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_refyard"))
            .current_dir(root)
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("system-config"))
            .env("XDG_STATE_HOME", root.join("system-state"))
            .env_remove("REFYARD_REGISTRY")
            .env_remove("REFYARD_STATE_DIR")
            .args(arguments)
            .output()
            .unwrap()
    };
    let initialized = invoke_with_system_directories(&["--json", "registry", "list"]);
    assert!(initialized.status.success());
    assert!(
        root.join("system-config/refyard/refyard.registry.yaml")
            .exists()
    );
    assert!(root.join("system-state/refyard/state.lock").exists());
    let registry_override =
        invoke_with_system_directories(&["--registry", "alternate.yaml", "registry", "list"]);
    assert!(registry_override.status.success());
    assert!(root.join("alternate.yaml").exists());
    let state_override =
        invoke_with_system_directories(&["--state-dir", "alternate-state", "registry", "list"]);
    assert!(state_override.status.success());
    assert!(root.join("alternate-state/state.lock").exists());
    assert!(!root.join("home").exists());
}

#[test]
fn errors_are_actionable_and_internal_diagnostics_require_an_explicit_flag() {
    let directory = tempdir().unwrap();
    let root = directory.path();
    let normal = Command::new(env!("CARGO_BIN_EXE_refyard"))
        .current_dir(root)
        .args([
            "--registry",
            "refyard.registry.yaml",
            "--state-dir",
            ".refyard",
        ])
        .env("RUST_BACKTRACE", "1")
        .args(["refs", "model"])
        .output()
        .unwrap();
    assert_eq!(normal.status.code(), Some(1));
    assert!(normal.stdout.is_empty());
    let message = String::from_utf8(normal.stderr).unwrap();
    assert!(message.contains("Run `refyard scan`"));
    for internal in [
        "Context (",
        "Backtrace",
        "execute Refyard command",
        "diagnostics:",
        "os error",
    ] {
        assert!(!message.contains(internal), "exposed {internal}");
    }
    let debug = invoke(root, &["--debug-errors", "refs", "model"]);
    let message = String::from_utf8(debug.stderr).unwrap();
    assert!(message.contains("diagnostics:"));
    assert!(message.contains("execute Refyard command"));
    let missing = invoke(root, &["registry", "add", "model", "--path", "absent"]);
    assert!(
        String::from_utf8(missing.stderr)
            .unwrap()
            .contains("Supply an existing, accessible location")
    );
}

#[test]
fn scan_and_validation_reports_do_not_leak_parser_payloads_or_debug_context() {
    let directory = tempdir().unwrap();
    let root = directory.path();
    fs::create_dir(root.join("project")).unwrap();
    fs::write(root.join("project/refyard.yaml"), "requires: []\n").unwrap();
    success(root, &["--json", "repo", "add", "project"]);
    fs::write(
        root.join("project/refyard.yaml"),
        "requires: []\nsuper_secret_field: private-value\n",
    )
    .unwrap();
    for arguments in [&["--json", "scan"][..], &["--json", "validate"][..]] {
        let output = invoke(root, arguments);
        assert_eq!(output.status.code(), Some(1));
        let report = String::from_utf8(output.stdout).unwrap();
        assert!(report.contains("Invalid YAML"));
        assert!(!report.contains("super_secret_field"));
        assert!(!report.contains("private-value"));
        assert!(!report.contains("Context ("));
    }
}
