//! Durable repository registration, disposable indexing, and atomic file persistence.

use std::{
    fs::{self, File, OpenOptions},
    io::{ErrorKind, Write},
    path::{Component, Path, PathBuf, absolute},
};

use eros::{Context, Result, bail};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tempfile::NamedTempFile;

use crate::{errors::user_error, manifest::ResourceKey};

// Data
//------------------------------------------------------------------------------

/// Persistent discovery inputs. Both manual and discovered repositories survive index removal.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repositories {
    pub repositories: Vec<PathBuf>,
    pub scan_roots: Vec<PathBuf>,
}

/// One manifest's indexed dependency declarations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub path: PathBuf,
    pub requires: Vec<ResourceKey>,
}

/// A failed inspection that prevents the index from certifying a complete scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScanIssue {
    pub path: PathBuf,
    pub message: String,
}

/// A replaceable dependency snapshot. Only an error-free full scan authorizes cleanup.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub version: u32,
    pub repositories: Repositories,
    pub projects: Vec<Project>,
    pub issues: Vec<ScanIssue>,
    pub full_scan: bool,
}

// Functions
//------------------------------------------------------------------------------

/// Hold an exclusive advisory lock for the returned file's lifetime.
pub fn lock_state(directory: &Path) -> Result<File> {
    fs::create_dir_all(directory).with_user_context(|| {
        format!(
            "Could not create Refyard state at {}. Check permissions and free space.",
            directory.display()
        )
    })?;
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(directory.join("state.lock"))?;
    lock.lock()
        .user_context("Could not lock Refyard state. Check access to the state directory.")?;
    Ok(lock)
}

/// Read YAML with the source path attached to parsing and I/O errors.
pub fn read_yaml<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let contents = fs::read(path).with_user_context(|| {
        format!(
            "Could not read {}. Check that it exists and is readable.",
            path.display()
        )
    })?;
    Ok(yaml_serde::from_slice(&contents)
        .with_user_context(|| format!("Check the YAML document at {}.", path.display()))?)
}

/// Atomically replace a YAML document using a temporary file in the same directory.
pub fn write_yaml<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let path = absolute(path)?;
    let parent = path.parent().expect("absolute file path has a parent");
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(yaml_serde::to_string(value)?.as_bytes())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(&path)
        .with_context(|| format!("replace {}", path.display()))?;
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

/// Atomically create a YAML document, preserving any existing destination.
/// Concurrent creation is treated as success, without replacing the winner's document.
pub fn create_yaml_if_missing<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let path = absolute(path)?;
    let parent = path.parent().expect("absolute file path has a parent");
    fs::create_dir_all(parent)?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    temporary.write_all(yaml_serde::to_string(value)?.as_bytes())?;
    temporary.as_file().sync_all()?;
    match temporary.persist_noclobber(&path) {
        Ok(_) => {}
        Err(error) if error.error.kind() == ErrorKind::AlreadyExists => return Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("create {} without overwriting", path.display()))?
        }
    }
    #[cfg(unix)]
    File::open(parent)?.sync_all()?;
    Ok(())
}

/// Load repository registrations; a missing file means no repositories are registered yet.
pub fn load_repositories(directory: &Path) -> Result<Repositories> {
    let path = directory.join("repositories.yaml");
    match fs::metadata(&path) {
        Ok(_) => read_yaml(&path),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Repositories::default()),
        Err(error) => Err(error.into()),
    }
}

/// Save durable discovery inputs and discard any previous cleanup authorization.
pub fn save_repositories(directory: &Path, repositories: &Repositories) -> Result<()> {
    invalidate_index(directory)?;
    write_yaml(&directory.join("repositories.yaml"), repositories)
}

/// Read an indexed snapshot, rejecting stale registration inputs and incompatible formats.
pub fn load_index(directory: &Path, repositories: &Repositories) -> Result<Index> {
    let index: Index = read_yaml(&directory.join("index.yaml"))
        .user_context("Index unavailable. Run `refyard scan` to rebuild it.")?;
    if index.version != 1 || &index.repositories != repositories {
        return Err(user_error(
            "Index incompatible or registration changed. Run `refyard scan` to rebuild it.",
        ));
    }
    Ok(index)
}

/// Remove a stale index without requiring it to exist.
pub fn invalidate_index(directory: &Path) -> Result<()> {
    match fs::remove_file(directory.join("index.yaml")) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Resolve symlinks in existing ancestors, including for paths whose final components are missing.
pub fn physical_path(path: &Path) -> Result<PathBuf> {
    let absolute_path = absolute(path)?;
    let mut ancestor = absolute_path.as_path();
    let mut missing_components = Vec::new();
    let mut resolved = loop {
        match fs::canonicalize(ancestor) {
            Ok(path) => break path,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                let Some(name) = ancestor.file_name() else {
                    bail!("cannot resolve {}", path.display());
                };
                missing_components.push(name.to_os_string());
                ancestor = ancestor.parent().expect("non-root path has a parent");
            }
            Err(error) => return Err(error.into()),
        }
    };
    for component in missing_components.into_iter().rev() {
        resolved.push(component);
    }
    // `..` after a missing component must not defeat containment checks.
    let mut normalized = PathBuf::new();
    for component in resolved.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            _ => normalized.push(component.as_os_str()),
        }
    }
    Ok(normalized)
}
