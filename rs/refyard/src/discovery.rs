//! Manifest discovery and rebuilding the dependency index from authoritative project files.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf, absolute},
};

use eros::{Context, Result};
use ignore::WalkBuilder;

use crate::{
    errors::{user_error, user_message},
    manifest::load_manifest,
    storage::{
        Index, Project, Repositories, ScanIssue, invalidate_index, save_repositories, write_yaml,
    },
};

// Data
//------------------------------------------------------------------------------

/// Optional discovery exclusions. Either exclusion prevents cleanup authorization.
#[derive(Debug, Clone, Copy, Default)]
pub struct ScanOptions {
    pub full: bool,
    pub respect_gitignore: bool,
    pub ignore_hidden: bool,
}

// Functions
//------------------------------------------------------------------------------

/// Register a repository directory or its manifest, requiring a valid manifest immediately.
pub fn register(repositories: &mut Repositories, path: &Path) -> Result<()> {
    let path = absolute(path)?;
    let directory = if path.is_dir() {
        fs::canonicalize(&path).with_user_context(|| {
            format!(
                "Could not register {}. Check that the project is accessible.",
                path.display()
            )
        })?
    } else if path.file_name().is_some_and(|name| name == "refyard.yaml") {
        fs::canonicalize(path.parent().expect("manifest has a parent"))?
    } else {
        return Err(user_error(
            "Supply a repository directory or refyard.yaml file.",
        ));
    };
    load_manifest(&directory.join("refyard.yaml"))?;
    repositories.repositories.push(directory);
    repositories.repositories.sort();
    repositories.repositories.dedup();
    Ok(())
}

/// Scan all durable roots and registered repositories, persisting discovered locations.
///
/// Invariants: a full scan includes hidden and ignored entries. Any traversal or manifest error
/// revokes cleanup authorization, while retaining unavailable repositories for later inspection.
pub fn scan(
    state_directory: &Path,
    repositories: &mut Repositories,
    roots: &[PathBuf],
    options: ScanOptions,
) -> Result<Index> {
    // Revoke authorization even if the new scan fails before producing an index.
    invalidate_index(state_directory)?;
    if options.full && (options.respect_gitignore || options.ignore_hidden) {
        return Err(user_error(
            "A full scan cannot exclude hidden or gitignored paths.",
        ));
    }
    for root in roots {
        let root = fs::canonicalize(root).with_user_context(|| {
            format!(
                "Could not scan {}. Check that the root is accessible.",
                root.display()
            )
        })?;
        if !root.is_dir() {
            return Err(user_error(format!(
                "Scan root must be a directory: {}",
                root.display()
            )));
        }
        repositories.scan_roots.push(root);
    }
    repositories.scan_roots.sort();
    repositories.scan_roots.dedup();
    if options.full && repositories.scan_roots.is_empty() {
        return Err(user_error(
            "A full scan requires at least one scan root. Run `refyard scan --full <ROOT>`.",
        ));
    }

    let mut issues = Vec::new();
    let mut discovered: BTreeSet<PathBuf> = repositories.repositories.iter().cloned().collect();
    for root in &repositories.scan_roots {
        if !root.is_dir() {
            issues.push(ScanIssue {
                path: root.clone(),
                message: "scan root unavailable".into(),
            });
            continue;
        }
        let walker = WalkBuilder::new(root)
            .standard_filters(false)
            .hidden(options.ignore_hidden)
            .git_ignore(options.respect_gitignore)
            .git_exclude(options.respect_gitignore)
            .git_global(options.respect_gitignore)
            .require_git(false)
            .follow_links(false)
            .build();
        for entry in walker {
            match entry {
                Ok(entry) => {
                    if entry.error().is_some() {
                        issues.push(ScanIssue {
                            path: entry.path().to_path_buf(),
                            message: "Could not process ignore rules. Check the ignore files for this location.".into(),
                        });
                    }
                    if entry.file_name() == "refyard.yaml" && !entry.path().is_dir() {
                        let directory = entry.path().parent().expect("manifest has a parent");
                        discovered.insert(directory.to_path_buf());
                    }
                }
                Err(_) => issues.push(ScanIssue {
                    path: root.clone(),
                    message:
                        "Could not traverse an entry under this root. Check filesystem access."
                            .into(),
                }),
            }
        }
    }
    repositories.repositories = discovered.into_iter().collect();
    save_repositories(state_directory, repositories)?;
    let mut projects = Vec::new();
    for directory in &repositories.repositories {
        match load_manifest(&directory.join("refyard.yaml")) {
            Ok(manifest) => projects.push(Project {
                path: directory.clone(),
                requires: manifest.requires,
            }),
            Err(error) => issues.push(ScanIssue {
                path: directory.clone(),
                message: user_message(&error),
            }),
        }
    }
    let full_scan = options.full && issues.is_empty();
    let index = Index {
        version: 1,
        repositories: repositories.clone(),
        projects,
        issues,
        full_scan,
    };
    write_yaml(&state_directory.join("index.yaml"), &index)?;
    Ok(index)
}
