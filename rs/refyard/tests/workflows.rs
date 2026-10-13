//! End-to-end filesystem workflows and deletion safety regression tests.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    thread,
};

use refyard::{
    cleanup::cleanup,
    discovery::{ScanOptions, register, scan},
    graph::{references, validate},
    manifest::{Artifact, Manifest, Registry, ResourceKey, load_manifest, load_registry},
    storage::{
        Repositories, create_yaml_if_missing, load_index, load_repositories, save_repositories,
        write_yaml,
    },
};
use tempfile::{TempDir, tempdir};

struct Fixture {
    _directory: TempDir,
    roots: PathBuf,
    state: PathBuf,
    registry: PathBuf,
    resources: PathBuf,
}

fn fixture() -> Fixture {
    let directory = tempdir().unwrap();
    let roots = directory.path().join("projects");
    let state = directory.path().join("state");
    let registry = directory.path().join("refyard.registry.yaml");
    let resources = directory.path().join("resources");
    fs::create_dir(&roots).unwrap();
    fs::create_dir(&resources).unwrap();
    write_yaml(&registry, &Registry::default()).unwrap();
    Fixture {
        _directory: directory,
        roots,
        state,
        registry,
        resources,
    }
}

fn resource(id: &str, revision: Option<&str>) -> ResourceKey {
    ResourceKey {
        id: id.into(),
        revision: revision.map(String::from),
    }
}

fn project(path: &Path, requires: Vec<ResourceKey>) {
    fs::create_dir_all(path).unwrap();
    write_yaml(&path.join("refyard.yaml"), &Manifest { requires }).unwrap();
}

fn artifact(key: ResourceKey, path: &Path) -> Artifact {
    Artifact {
        resource: key,
        path: path.to_path_buf(),
        notes: None,
    }
}

fn full_scan(fixture: &Fixture) {
    let mut repositories = load_repositories(&fixture.state).unwrap();
    let index = scan(
        &fixture.state,
        &mut repositories,
        std::slice::from_ref(&fixture.roots),
        ScanOptions {
            full: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(index.full_scan, "{:?}", index.issues);
}

#[test]
fn concurrent_registry_creation_preserves_one_complete_document() {
    let directory = tempdir().unwrap();
    let registry_path = directory.path().join("config/registry.yaml");
    let barrier = Arc::new(Barrier::new(8));
    let creators: Vec<_> = (0..8)
        .map(|creator| {
            let registry_path = registry_path.clone();
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                let registry = Registry {
                    artifacts: vec![artifact(
                        resource(&format!("creator-{creator}"), None),
                        Path::new("external-resource"),
                    )],
                };
                barrier.wait();
                create_yaml_if_missing(&registry_path, &registry).unwrap();
            })
        })
        .collect();
    for creator in creators {
        creator.join().unwrap();
    }
    let registry = load_registry(&registry_path).unwrap();
    assert_eq!(registry.artifacts.len(), 1);
    let original = fs::read(&registry_path).unwrap();
    create_yaml_if_missing(&registry_path, &Registry::default()).unwrap();
    assert_eq!(fs::read(&registry_path).unwrap(), original);
}

#[test]
fn supplied_yaml_schemas_round_trip_and_reject_mistakes() {
    let fixture = fixture();
    let manifest = fixture.roots.join("refyard.yaml");
    fs::write(&manifest, "requires:\n  - id: hf:sentence-transformers/all-MiniLM-L6-v2\n    revision: abc123\n  - id: hf:Qwen/Qwen2.5-7B-Instruct\n    revision: def456\n").unwrap();
    assert_eq!(load_manifest(&manifest).unwrap().requires.len(), 2);
    fs::write(&fixture.registry, "artifacts:\n  - id: hf:sentence-transformers/all-MiniLM-L6-v2\n    revision: abc123\n    path: /shared/models/minilm\n    notes: Downloaded from Hugging Face\n").unwrap();
    let registry = load_registry(&fixture.registry).unwrap();
    write_yaml(&fixture.registry, &registry).unwrap();
    assert_eq!(load_registry(&fixture.registry).unwrap(), registry);
    for bad in [
        "",
        "{}",
        "requires: null",
        "require: []",
        "requires:\n  - id: ' '",
        "requires:\n  - id: model\n    revision: ''",
        "requires:\n  - id: model\n    revison: abc",
        "requires:\n  - id: model\n  - id: model",
    ] {
        fs::write(&manifest, bad).unwrap();
        assert!(load_manifest(&manifest).is_err(), "accepted {bad}");
    }
    for bad in [
        "artifacts:\n  - id: model\n    path: ''",
        "artifacts:\n  - id: model\n    path: /tmp/model\n    typo: true",
        "artifacts:\n  - id: model\n    path: /tmp/one\n  - id: model\n    path: /tmp/two",
    ] {
        fs::write(&fixture.registry, bad).unwrap();
        assert!(load_registry(&fixture.registry).is_err(), "accepted {bad}");
    }
}

#[test]
fn full_discovery_includes_hidden_and_gitignored_projects() {
    let fixture = fixture();
    for name in ["visible", "ignored", ".hidden"] {
        project(&fixture.roots.join(name), vec![]);
    }
    fs::write(fixture.roots.join(".gitignore"), "ignored/\n").unwrap();
    let mut repositories = Repositories::default();
    let filtered = scan(
        &fixture.state,
        &mut repositories,
        std::slice::from_ref(&fixture.roots),
        ScanOptions {
            respect_gitignore: true,
            ignore_hidden: true,
            full: false,
        },
    )
    .unwrap();
    assert_eq!(filtered.projects.len(), 1);
    assert!(!filtered.full_scan);
    let complete = scan(
        &fixture.state,
        &mut repositories,
        &[],
        ScanOptions {
            full: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(complete.projects.len(), 3);
    assert!(complete.full_scan);
}

#[test]
fn manual_registration_and_index_rebuild_preserve_discovery_inputs() {
    let fixture = fixture();
    let directory = fixture.roots.join("project");
    project(&directory, vec![resource("model", Some("abc"))]);
    let mut repositories = Repositories::default();
    register(&mut repositories, &directory.join("refyard.yaml")).unwrap();
    register(&mut repositories, &directory).unwrap();
    assert_eq!(repositories.repositories.len(), 1);
    save_repositories(&fixture.state, &repositories).unwrap();
    let original = scan(
        &fixture.state,
        &mut repositories,
        &[],
        ScanOptions::default(),
    )
    .unwrap();
    fs::remove_file(fixture.state.join("index.yaml")).unwrap();
    assert!(load_index(&fixture.state, &repositories).is_err());
    let mut durable = load_repositories(&fixture.state).unwrap();
    let rebuilt = scan(&fixture.state, &mut durable, &[], ScanOptions::default()).unwrap();
    assert_eq!(original.projects, rebuilt.projects);
}

#[test]
fn reverse_lookup_distinguishes_revisions_and_includes_unregistered_ids() {
    let fixture = fixture();
    project(
        &fixture.roots.join("one"),
        vec![resource("model", Some("abc")), resource("unknown", None)],
    );
    project(
        &fixture.roots.join("two"),
        vec![resource("model", Some("def"))],
    );
    full_scan(&fixture);
    let repositories = load_repositories(&fixture.state).unwrap();
    let index = load_index(&fixture.state, &repositories).unwrap();
    let registry = Registry {
        artifacts: vec![artifact(resource("model", None), &fixture.resources)],
    };
    assert_eq!(references(&index, &registry, "model", None).len(), 3);
    let filtered = references(&index, &registry, "model", Some("abc"));
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].projects, vec![fixture.roots.join("one")]);
    assert_eq!(
        references(&index, &registry, "unknown", None)[0]
            .projects
            .len(),
        1
    );
    assert!(references(&index, &registry, "absent", None).is_empty());
}

#[test]
fn validation_reads_live_manifests_and_reports_all_warning_classes() {
    let fixture = fixture();
    let directory = fixture.roots.join("project");
    project(&directory, vec![]);
    full_scan(&fixture);
    project(&directory, vec![resource("unknown", None)]);
    let registry = Registry {
        artifacts: vec![artifact(
            resource("missing", None),
            Path::new("resources/missing"),
        )],
    };
    let mut repositories = load_repositories(&fixture.state).unwrap();
    repositories.repositories.push(fixture.roots.join("gone"));
    let report = validate(&repositories, &fixture.registry, &registry);
    for kind in [
        "unavailable_project",
        "unregistered_resource",
        "unavailable_resource",
        "unreferenced_resource",
    ] {
        assert!(
            report.issues.iter().any(|issue| issue.kind == kind),
            "missing {kind}"
        );
    }
}

#[test]
fn dry_run_preserves_resources_and_deletion_requires_a_full_scan() {
    let fixture = fixture();
    let file = fixture.resources.join("weights.bin");
    fs::write(&file, "weights").unwrap();
    write_yaml(
        &fixture.registry,
        &Registry {
            artifacts: vec![artifact(
                resource("weights", None),
                Path::new("resources/weights.bin"),
            )],
        },
    )
    .unwrap();
    let mut repositories = Repositories::default();
    scan(
        &fixture.state,
        &mut repositories,
        std::slice::from_ref(&fixture.roots),
        ScanOptions::default(),
    )
    .unwrap();
    let report = cleanup(&fixture.state, &fixture.registry, false).unwrap();
    assert_eq!(report.candidates[0].path, file);
    assert!(report.dry_run);
    assert!(file.exists());
    assert!(cleanup(&fixture.state, &fixture.registry, true).is_err());
    assert!(file.exists());
}

#[test]
fn authorized_cleanup_deletes_files_and_directories_and_consumes_authorization() {
    let fixture = fixture();
    let file = fixture.resources.join("file");
    let directory = fixture.resources.join("directory");
    fs::write(&file, "file").unwrap();
    fs::create_dir(&directory).unwrap();
    fs::write(directory.join("data"), "directory contents").unwrap();
    let registry = Registry {
        artifacts: vec![
            artifact(resource("file", None), &file),
            artifact(resource("directory", None), &directory),
        ],
    };
    write_yaml(&fixture.registry, &registry).unwrap();
    full_scan(&fixture);
    let report = cleanup(&fixture.state, &fixture.registry, true).unwrap();
    assert!(!report.dry_run);
    assert!(report.candidates.iter().all(|candidate| candidate.deleted));
    assert!(!file.exists());
    assert!(!directory.exists());
    assert_eq!(load_registry(&fixture.registry).unwrap(), registry);
    assert!(cleanup(&fixture.state, &fixture.registry, true).is_err());
}

#[test]
fn cleanup_finds_new_references_created_after_the_authorizing_scan() {
    let fixture = fixture();
    let weights = fixture.resources.join("weights");
    fs::write(&weights, "weights").unwrap();
    write_yaml(
        &fixture.registry,
        &Registry {
            artifacts: vec![artifact(resource("model", Some("abc")), &weights)],
        },
    )
    .unwrap();
    full_scan(&fixture);
    project(
        &fixture.roots.join("new"),
        vec![resource("model", Some("abc"))],
    );
    let report = cleanup(&fixture.state, &fixture.registry, true).unwrap();
    assert!(report.candidates.is_empty());
    assert!(weights.exists());
}

#[test]
fn cleanup_aborts_when_a_manifest_becomes_unreadable_or_invalid() {
    let fixture = fixture();
    let directory = fixture.roots.join("project");
    project(&directory, vec![]);
    let weights = fixture.resources.join("weights");
    fs::write(&weights, "weights").unwrap();
    write_yaml(
        &fixture.registry,
        &Registry {
            artifacts: vec![artifact(resource("model", None), &weights)],
        },
    )
    .unwrap();
    full_scan(&fixture);
    fs::write(directory.join("refyard.yaml"), "requires: [broken").unwrap();
    assert!(cleanup(&fixture.state, &fixture.registry, true).is_err());
    assert!(weights.exists());
    let repositories = load_repositories(&fixture.state).unwrap();
    assert!(!load_index(&fixture.state, &repositories).unwrap().full_scan);
}

#[test]
fn missing_known_repositories_are_retained_and_prevent_full_scan_success() {
    let fixture = fixture();
    let directory = fixture.roots.join("project");
    project(&directory, vec![]);
    full_scan(&fixture);
    fs::remove_dir_all(&directory).unwrap();
    let mut repositories = load_repositories(&fixture.state).unwrap();
    let index = scan(
        &fixture.state,
        &mut repositories,
        &[],
        ScanOptions {
            full: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!index.full_scan);
    assert_eq!(index.issues.len(), 1);
    assert_eq!(repositories.repositories, vec![directory]);
}

#[test]
fn failed_or_filtered_rescans_revoke_prior_full_scan_authorization() {
    let fixture = fixture();
    full_scan(&fixture);
    let mut repositories = load_repositories(&fixture.state).unwrap();
    assert!(
        scan(
            &fixture.state,
            &mut repositories,
            &[],
            ScanOptions {
                full: true,
                ignore_hidden: true,
                respect_gitignore: false
            }
        )
        .is_err()
    );
    assert!(load_index(&fixture.state, &repositories).is_err());
    full_scan(&fixture);
    let index = scan(
        &fixture.state,
        &mut repositories,
        &[],
        ScanOptions {
            ignore_hidden: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!index.full_scan);
}

#[test]
fn cleanup_protects_aliases_and_both_directions_of_directory_overlap() {
    let fixture = fixture();
    let shared = fixture.resources.join("shared");
    fs::create_dir(&shared).unwrap();
    let file = shared.join("weights");
    fs::write(&file, "weights").unwrap();
    let used = resource("used", None);
    project(&fixture.roots.join("project"), vec![used.clone()]);
    for referenced in [&file, &shared] {
        let registry = Registry {
            artifacts: vec![
                artifact(used.clone(), referenced),
                artifact(resource("unused-parent", None), &shared),
                artifact(resource("unused-child", None), &file),
            ],
        };
        write_yaml(&fixture.registry, &registry).unwrap();
        full_scan(&fixture);
        let report = cleanup(&fixture.state, &fixture.registry, true).unwrap();
        assert_eq!(report.candidates.len(), 2);
        assert!(
            report
                .candidates
                .iter()
                .all(|candidate| candidate.blocked_reason.is_some() && !candidate.deleted)
        );
        assert!(file.exists());
    }
}

#[test]
fn cleanup_protects_projects_state_registry_and_scan_roots() {
    let fixture = fixture();
    let directory = fixture.roots.join("project");
    project(&directory, vec![]);
    let registry = Registry {
        artifacts: vec![
            artifact(resource("project", None), &directory),
            artifact(resource("registry", None), &fixture.registry),
            artifact(resource("state", None), &fixture.state),
            artifact(resource("root", None), &fixture.roots),
        ],
    };
    write_yaml(&fixture.registry, &registry).unwrap();
    full_scan(&fixture);
    let report = cleanup(&fixture.state, &fixture.registry, true).unwrap();
    assert_eq!(report.candidates.len(), 4);
    assert!(
        report
            .candidates
            .iter()
            .all(|candidate| candidate.blocked_reason.is_some())
    );
    assert!(fixture.registry.exists());
    assert!(directory.join("refyard.yaml").exists());
}

#[test]
fn nested_unused_locations_are_deleted_once_and_missing_locations_are_reported() {
    let fixture = fixture();
    let child = fixture.resources.join("child");
    fs::write(&child, "data").unwrap();
    write_yaml(
        &fixture.registry,
        &Registry {
            artifacts: vec![
                artifact(resource("parent", None), &fixture.resources),
                artifact(resource("child", None), &child),
                artifact(resource("alias", None), &child),
                artifact(
                    resource("missing", None),
                    &fixture.resources.join("missing"),
                ),
            ],
        },
    )
    .unwrap();
    full_scan(&fixture);
    let report = cleanup(&fixture.state, &fixture.registry, true).unwrap();
    assert_eq!(report.candidates.len(), 3);
    assert_eq!(report.missing.len(), 1);
    assert!(report.candidates.iter().all(|candidate| candidate.deleted));
    assert!(!fixture.resources.exists());
}

#[cfg(unix)]
#[test]
fn symlink_targets_and_the_links_needed_to_reach_them_remain_available() {
    use std::os::unix::fs::symlink;
    let fixture = fixture();
    let unused = fixture.resources.join("unused");
    let target = fixture.resources.join("target");
    fs::create_dir(&unused).unwrap();
    fs::create_dir(&target).unwrap();
    fs::write(target.join("weights"), "weights").unwrap();
    symlink(&target, unused.join("link")).unwrap();
    let used = resource("used", None);
    project(&fixture.roots.join("project"), vec![used.clone()]);
    write_yaml(
        &fixture.registry,
        &Registry {
            artifacts: vec![
                artifact(used, &unused.join("link/weights")),
                artifact(resource("unused", None), &unused),
                artifact(resource("target", None), &target),
                artifact(resource("link", None), &unused.join("link")),
            ],
        },
    )
    .unwrap();
    full_scan(&fixture);
    let report = cleanup(&fixture.state, &fixture.registry, true).unwrap();
    assert!(
        report
            .candidates
            .iter()
            .all(|candidate| candidate.blocked_reason.is_some())
    );
    assert!(unused.join("link/weights").exists());
}
