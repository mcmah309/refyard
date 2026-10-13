//! Conservative cleanup planning and deletion gated by complete filesystem discovery.

use std::{
    collections::BTreeSet,
    env, fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use eros::{Context, Result};
use serde::Serialize;

use crate::{
    discovery::{ScanOptions, scan},
    errors::user_error,
    graph::{artifact_path, reverse_dependencies},
    manifest::{Registry, ResourceKey, load_manifest, load_registry},
    storage::{Index, invalidate_index, load_index, load_repositories, physical_path},
};

// Data
//------------------------------------------------------------------------------

/// An unreferenced registry location. A blocked entry can never be deleted by this plan.
#[derive(Debug, Serialize)]
pub struct CleanupCandidate {
    pub resource: ResourceKey,
    pub path: PathBuf,
    pub blocked_reason: Option<String>,
    pub deleted: bool,
}

/// A dry-run or executed cleanup report. Missing resources require no filesystem action.
#[derive(Debug, Serialize)]
pub struct CleanupReport {
    pub dry_run: bool,
    pub candidates: Vec<CleanupCandidate>,
    pub missing: Vec<ResourceKey>,
}

// Functions
//------------------------------------------------------------------------------

/// Identify unused registered files and directories, optionally deleting eligible locations.
///
/// Invariants: deletion requires a prior successful full scan and repeats that scan immediately.
/// Registry paths, state, projects, and referenced resource paths are protected from overlapping
/// deletions. The authorization is consumed before filesystem changes begin.
pub fn cleanup(
    state_directory: &Path,
    registry_path: &Path,
    delete: bool,
) -> Result<CleanupReport> {
    let mut repositories = load_repositories(state_directory)?;
    let mut index = load_index(state_directory, &repositories)?;
    if delete {
        if !index.full_scan || !index.issues.is_empty() || repositories.scan_roots.is_empty() {
            return Err(user_error(
                "deletion requires a successful full scan; run `refyard scan --full <ROOT>` first",
            ));
        }
        index = scan(
            state_directory,
            &mut repositories,
            &[],
            ScanOptions {
                full: true,
                ..ScanOptions::default()
            },
        )?;
        if !index.full_scan {
            return Err(user_error("Fresh full scan failed; no resources were deleted. Run `refyard scan --full` to inspect scan issues.")
                .context(format!("fresh scan issues: {:?}", index.issues)));
        }
    }
    let registry = load_registry(registry_path)?;
    let mut report = plan_cleanup(state_directory, registry_path, &registry, &index)?;
    if !delete {
        return Ok(report);
    }
    invalidate_index(state_directory)?;
    // Detect source changes while constructing the plan before making any deletion.
    if load_registry(registry_path)? != registry {
        return Err(user_error(
            "Registry changed during cleanup; no resources were deleted. Keep source files stable and retry after a full scan.",
        ));
    }
    for project in &index.projects {
        if load_manifest(&project.path.join("refyard.yaml"))?.requires != project.requires {
            return Err(user_error(format!(
                "manifest changed during cleanup: {}",
                project.path.display()
            )));
        }
    }
    report.dry_run = false;
    let mut deleted_paths = BTreeSet::<PathBuf>::new();
    for candidate in &mut report.candidates {
        if candidate.blocked_reason.is_some() {
            continue;
        }
        // A parent directory or an alias may already have been removed by this plan.
        if deleted_paths
            .iter()
            .any(|path| candidate.path.starts_with(path))
        {
            candidate.deleted = true;
            continue;
        }
        let metadata = fs::symlink_metadata(&candidate.path)
            .with_user_context(|| format!("Could not inspect cleanup target {}. Check filesystem access and run a new full scan.", candidate.path.display()))?;
        if metadata.file_type().is_symlink() || physical_path(&candidate.path)? != candidate.path {
            return Err(user_error(format!(
                "cleanup target changed to a symlink: {}",
                candidate.path.display()
            )));
        }
        if metadata.is_dir() {
            fs::remove_dir_all(&candidate.path)
                .with_user_context(|| format!("Could not delete directory {}. Check permissions; some earlier candidates may already have been deleted.", candidate.path.display()))?;
        } else if metadata.is_file() {
            fs::remove_file(&candidate.path)
                .with_user_context(|| format!("Could not delete file {}. Check permissions; some earlier candidates may already have been deleted.", candidate.path.display()))?;
        } else {
            return Err(user_error(format!(
                "Cleanup target changed type: {}. Run a new full scan before retrying.",
                candidate.path.display()
            )));
        }
        deleted_paths.insert(candidate.path.clone());
        candidate.deleted = true;
    }
    Ok(report)
}

/// Plan deletions using physical paths so aliases and directory containment cannot hide references.
pub fn plan_cleanup(
    state_directory: &Path,
    registry_path: &Path,
    registry: &Registry,
    index: &Index,
) -> Result<CleanupReport> {
    let references = reverse_dependencies(&index.projects);
    let mut referenced_paths = Vec::new();
    for artifact in &registry.artifacts {
        if references.contains_key(&artifact.resource) {
            referenced_paths.extend(protected_locations(&artifact_path(
                registry_path,
                artifact,
            ))?);
        }
    }
    let mut protected_paths = protected_locations(state_directory)?;
    protected_paths.extend(protected_locations(registry_path)?);
    for project in &index.repositories.repositories {
        protected_paths.extend(protected_locations(project)?);
        protected_paths.extend(protected_locations(&project.join("refyard.yaml"))?);
    }
    let mut protected_ancestors = index.repositories.scan_roots.clone();
    protected_ancestors.push(env::current_dir()?);
    let mut candidates = Vec::new();
    let mut missing = Vec::new();
    for artifact in &registry.artifacts {
        if references.contains_key(&artifact.resource) {
            continue;
        }
        let location = artifact_path(registry_path, artifact);
        let metadata = match fs::symlink_metadata(&location) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                missing.push(artifact.resource.clone());
                continue;
            }
            Err(error) => return Err(error.into()),
        };
        let path = physical_path(&location)?;
        let blocked_reason = if metadata.file_type().is_symlink() {
            Some("symlink resources must be managed externally".into())
        } else if !metadata.is_file() && !metadata.is_dir() {
            Some("resource is not a regular file or directory".into())
        } else if referenced_paths
            .iter()
            .any(|referenced| overlaps(&path, referenced))
        {
            Some("location overlaps a referenced resource".into())
        } else if protected_paths
            .iter()
            .any(|protected| overlaps(&path, protected))
        {
            Some("location overlaps a project, registry, or Refyard state".into())
        } else if protected_ancestors
            .iter()
            .any(|protected| protected.starts_with(&path))
        {
            Some("location contains a scan root or working directory".into())
        } else if !index.issues.is_empty() {
            Some("scan has unresolved errors; references may be incomplete".into())
        } else {
            None
        };
        candidates.push(CleanupCandidate {
            resource: artifact.resource.clone(),
            path,
            blocked_reason,
            deleted: false,
        });
    }
    // Parent directories precede children, making nested unused locations a single deletion.
    candidates.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(CleanupReport {
        dry_run: true,
        candidates,
        missing,
    })
}

// Private
//==============================================================================

fn overlaps(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

/// Protect both a resource's target and symlinks needed to reach that target.
fn protected_locations(location: &Path) -> Result<Vec<PathBuf>> {
    let mut locations = vec![physical_path(location)?];
    for ancestor in location.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                if let (Some(parent), Some(name)) = (ancestor.parent(), ancestor.file_name()) {
                    locations.push(physical_path(parent)?.join(name));
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(locations)
}
