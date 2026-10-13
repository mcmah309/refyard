//! Explicitly safe user errors and presentation separate from internal Eros diagnostics.

use std::io::{Error as IoError, ErrorKind};

use eros::{ErrorUnion, error};
use thiserror::Error;
use yaml_serde::Error as YamlError;

/// An application-authored message intentionally safe to present directly to a CLI user.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct UserError {
    pub message: String,
}

/// Create an explicitly user-facing error for rejected input or a required corrective action.
pub fn user_error(message: impl Into<String>) -> ErrorUnion {
    error!(UserError {
        message: message.into()
    })
}

/// Present allowlisted root errors and only context frames explicitly marked user-facing.
/// Unknown errors and their source chains are never rendered directly.
pub fn user_message(error: &ErrorUnion) -> String {
    let mut message = if let Some(error) = error.downcast_inner_ref::<UserError>() {
        error.to_string()
    } else if let Some(error) = error.downcast_inner_ref::<IoError>() {
        // Map known kinds rather than exposing arbitrary custom I/O error payloads.
        match error.kind() {
            ErrorKind::NotFound => "The requested file or directory does not exist.",
            ErrorKind::PermissionDenied => {
                "Permission denied. Check access to the requested location."
            }
            ErrorKind::AlreadyExists => {
                "A file or directory already exists at the requested location."
            }
            ErrorKind::NotADirectory => "A directory was expected at the requested location.",
            ErrorKind::IsADirectory => "A file was expected at the requested location.",
            ErrorKind::StorageFull => "The filesystem is full. Free space and retry.",
            ErrorKind::ReadOnlyFilesystem => "The filesystem is read-only.",
            _ => "A filesystem operation failed.",
        }
        .to_owned()
    } else if let Some(error) = error.downcast_inner_ref::<YamlError>() {
        // YAML diagnostics can echo document contents; only the location is safe by default.
        if let Some(location) = error.location() {
            format!(
                "Invalid YAML at line {}, column {}. Check the fields and value types.",
                location.line(),
                location.column()
            )
        } else {
            "Invalid YAML. Check the fields and value types.".to_owned()
        }
    } else {
        "An unexpected error occurred. Retry the operation.".to_owned()
    };
    for frame in error.contexts().filter(|frame| frame.is_user_facing()) {
        message.push('\n');
        message.push_str(&frame.to_string());
    }
    message
}
