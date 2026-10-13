//! Forward and reverse dependency queries, and validation against live source files.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

use eros::ErrorUnion;
use serde::Serialize;

use crate::{
    errors::user_message,
    manifest::{Artifact, Registry, ResourceKey, load_manifest},
    storage::{Index, Project, Repositories},
};

// Data
//------------------------------------------------------------------------------

/// All known projects that declare one exact resource identity.
#[derive(Debug, Serialize)]
pub struct References {
    pub resource: ResourceKey,
    pub projects: Vec<PathBuf>,
}

/// A registry entry enriched with dependency and local availability information.
#[derive(Debug, Serialize)]
pub struct ResourceStatus {
    pub artifact: Artifact,
    pub local_path: PathBuf,
    pub available: bool,
    pub projects: Vec<PathBuf>,
}

/// A problem in a manifest, repository, or registry entry.
#[derive(Debug, Serialize)]
pub struct ValidationIssue {
    pub kind: &'static str,
    pub location: String,
    pub message: String,
}

/// Validation warnings based on live manifests and registry paths.
#[derive(Debug, Serialize)]
pub struct ValidationReport {
    pub issues: Vec<ValidationIssue>,
}

// Functions
//------------------------------------------------------------------------------

/// Group indexed project declarations by exact resource identity, including unregistered resources.
pub fn reverse_dependencies(projects: &[Project]) -> BTreeMap<ResourceKey, Vec<PathBuf>> {
    let mut references = BTreeMap::<ResourceKey, Vec<PathBuf>>::new();
    for project in projects {
        for resource in &project.requires {
            references
                .entry(resource.clone())
                .or_default()
                .push(project.path.clone());
        }
    }
    references
}

/// Find references to an ID. An omitted revision filter returns all revisions, including unversioned.
pub fn references(
    index: &Index,
    registry: &Registry,
    id: &str,
    revision: Option<&str>,
) -> Vec<References> {
    let mut reverse = reverse_dependencies(&index.projects);
    for artifact in &registry.artifacts {
        reverse.entry(artifact.resource.clone()).or_default();
    }
    reverse
        .into_iter()
        .filter(|(resource, _)| {
            resource.id == id
                && revision.is_none_or(|value| resource.revision.as_deref() == Some(value))
        })
        .map(|(resource, projects)| References { resource, projects })
        .collect()
}

/// Resolve registry paths independently of the invoking process's working directory.
pub fn artifact_path(registry_path: &Path, artifact: &Artifact) -> PathBuf {
    if artifact.path.is_absolute() {
        artifact.path.clone()
    } else {
        registry_path
            .parent()
            .expect("registry path must have a parent")
            .join(&artifact.path)
    }
}

/// Enrich registry entries with current availability and indexed reverse references.
pub fn resource_statuses(
    registry_path: &Path,
    registry: &Registry,
    projects: &[Project],
) -> Vec<ResourceStatus> {
    let reverse = reverse_dependencies(projects);
    registry
        .artifacts
        .iter()
        .map(|artifact| {
            let local_path = artifact_path(registry_path, artifact);
            ResourceStatus {
                artifact: artifact.clone(),
                available: local_path.exists(),
                local_path,
                projects: reverse.get(&artifact.resource).cloned().unwrap_or_default(),
            }
        })
        .collect()
}

/// Validate live manifests, registered locations, unavailable scan roots, and unused resources.
/// This does not discover new projects or grant cleanup authorization.
pub fn validate(
    repositories: &Repositories,
    registry_path: &Path,
    registry: &Registry,
) -> ValidationReport {
    let mut issues = Vec::new();
    let mut projects = Vec::new();
    for directory in &repositories.repositories {
        match load_manifest(&directory.join("refyard.yaml")) {
            Ok(manifest) => projects.push(Project {
                path: directory.clone(),
                requires: manifest.requires,
            }),
            Err(error) => issues.push(ValidationIssue {
                kind: "unavailable_project",
                location: directory.display().to_string(),
                message: user_message(&error),
            }),
        }
    }
    for root in &repositories.scan_roots {
        if !root.is_dir() {
            issues.push(ValidationIssue {
                kind: "unavailable_scan_root",
                location: root.display().to_string(),
                message: "scan root unavailable".into(),
            });
        }
    }
    let registered: BTreeSet<_> = registry
        .artifacts
        .iter()
        .map(|artifact| &artifact.resource)
        .collect();
    for project in &projects {
        for resource in &project.requires {
            if !registered.contains(resource) {
                issues.push(ValidationIssue {
                    kind: "unregistered_resource",
                    location: project.path.display().to_string(),
                    message: format!(
                        "{} revision {:?} has no registry entry",
                        resource.id, resource.revision
                    ),
                });
            }
        }
    }
    for status in resource_statuses(registry_path, registry, &projects) {
        if let Err(error) = fs::metadata(&status.local_path) {
            issues.push(ValidationIssue {
                kind: "unavailable_resource",
                location: status.local_path.display().to_string(),
                message: format!(
                    "{}: {}",
                    status.artifact.resource.id,
                    user_message(&ErrorUnion::from(error))
                ),
            });
        }
        if status.projects.is_empty() {
            issues.push(ValidationIssue {
                kind: "unreferenced_resource",
                location: status.local_path.display().to_string(),
                message: format!(
                    "{} revision {:?} has no known references",
                    status.artifact.resource.id, status.artifact.resource.revision
                ),
            });
        }
    }
    ValidationReport { issues }
}
