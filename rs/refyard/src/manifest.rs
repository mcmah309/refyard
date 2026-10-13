//! Source-of-truth schemas and validation for project manifests and machine registries.

use std::{
    collections::BTreeSet,
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
};

use eros::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::{
    errors::user_error,
    storage::{create_yaml_if_missing, read_yaml},
};

// Data
//------------------------------------------------------------------------------

/// The exact identity of a resource. An omitted revision denotes an unversioned resource.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceKey {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

/// Dependencies declared by one project, independent of their machine locations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub requires: Vec<ResourceKey>,
}

/// A locally managed file or directory and optional retrieval information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    #[serde(flatten)]
    pub resource: ResourceKey,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

/// The machine's authoritative mapping from resource identities to locations.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
    pub artifacts: Vec<Artifact>,
}

// Functions
//------------------------------------------------------------------------------

/// Create an empty registry when its location is absent, preserving existing files and symlinks.
/// Existing contents are validated separately by `load_registry` when needed by a command.
pub fn ensure_registry(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => create_yaml_if_missing(path, &Registry::default()),
        Err(error) => Err(error.into()),
    }.with_user_context(|| format!("Could not access the registry at {}. Check the parent directory, permissions, and free space.", path.display()))
}

/// Read a manifest and reject malformed, empty, or ambiguous resource identities.
pub fn load_manifest(path: &Path) -> Result<Manifest> {
    let manifest: Manifest = read_yaml(path)?;
    validate_resources(manifest.requires.iter())
        .with_user_context(|| format!("Check the manifest at {}.", path.display()))?;
    Ok(manifest)
}

/// Read a registry. Relative artifact paths are interpreted relative to this file.
pub fn load_registry(path: &Path) -> Result<Registry> {
    let registry: Registry = read_yaml(path)?;
    validate_resources(registry.artifacts.iter().map(|artifact| &artifact.resource))
        .with_user_context(|| format!("Check the registry at {}.", path.display()))?;
    for artifact in &registry.artifacts {
        if artifact.path.as_os_str().is_empty() {
            return Err(user_error(format!(
                "Artifact {} has an empty path. Supply a file or directory location.",
                artifact.resource.id
            )));
        }
    }
    Ok(registry)
}

/// Reject blank identity components and repeated identities within a source document.
pub fn validate_resources<'a>(resources: impl IntoIterator<Item = &'a ResourceKey>) -> Result<()> {
    let mut seen = BTreeSet::new();
    for resource in resources {
        if resource.id.trim().is_empty()
            || resource
                .revision
                .as_ref()
                .is_some_and(|revision| revision.trim().is_empty())
        {
            return Err(user_error(
                "Resource IDs and supplied revisions must not be blank.",
            ));
        }
        if !seen.insert(resource) {
            return Err(user_error(format!(
                "duplicate resource: {} revision {:?}",
                resource.id, resource.revision
            )));
        }
    }
    Ok(())
}
