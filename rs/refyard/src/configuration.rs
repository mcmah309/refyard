//! Platform-standard machine locations with optional explicit CLI overrides.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use eros::Result;

use crate::errors::user_error;

/// Registry and durable state paths shared across working directories.
#[derive(Debug)]
pub struct Locations {
    pub registry: PathBuf,
    pub state_directory: PathBuf,
}

/// Resolve omitted paths using the operating system's per-user application directories.
/// Explicit paths bypass system discovery when both locations are supplied.
pub fn resolve_locations(
    registry: Option<&Path>,
    state_directory: Option<&Path>,
) -> Result<Locations> {
    if let (Some(registry), Some(state_directory)) = (registry, state_directory) {
        return Ok(Locations {
            registry: registry.to_path_buf(),
            state_directory: state_directory.to_path_buf(),
        });
    }
    let directories = ProjectDirs::from("", "", "refyard").ok_or_else(|| {
        user_error("Could not locate the system configuration directory. Supply --registry and --state-dir explicitly.")
    })?;
    let registry = match registry {
        Some(path) => path.to_path_buf(),
        None => directories.config_local_dir().join("refyard.registry.yaml"),
    };
    let state_directory = match state_directory {
        Some(path) => path.to_path_buf(),
        None => match directories.state_dir() {
            Some(path) => path.to_path_buf(),
            None => directories.data_local_dir().join("state"),
        },
    };
    Ok(Locations {
        registry,
        state_directory,
    })
}
