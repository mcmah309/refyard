//! User-facing errors must never expose unapproved roots, source chains, or context frames.

use std::io;

use eros::error;
use refyard::{
    errors::{user_error, user_message},
    manifest::Manifest,
};
use thiserror::Error;

#[derive(Debug, Error)]
#[error("private internal failure")]
struct InternalFailure {
    #[source]
    source: io::Error,
}

#[test]
fn user_errors_and_marked_context_are_explicitly_displayed() {
    let error = user_error("The resource identity is already registered.")
        .context("private implementation detail")
        .user_context("Edit the registry to update its location.");
    assert_eq!(
        user_message(&error),
        "The resource identity is already registered.\nEdit the registry to update its location."
    );
    assert!(format!("{error:?}").contains("private implementation detail"));
}

#[test]
fn unexpected_roots_and_source_chains_use_a_safe_fallback() {
    let error = error!(InternalFailure {
        source: io::Error::other("private source payload")
    })
    .context("private diagnostic context")
    .user_context("Run a new scan before retrying.");
    assert_eq!(
        user_message(&error),
        "An unexpected error occurred. Retry the operation.\nRun a new scan before retrying."
    );
    let diagnostics = format!("{error:?}");
    assert!(diagnostics.contains("private source payload"));
    assert!(diagnostics.contains("private diagnostic context"));
}

#[test]
fn allowlisted_io_errors_do_not_expose_arbitrary_error_payloads() {
    let error = error!(io::Error::other("private filesystem payload"));
    assert_eq!(user_message(&error), "A filesystem operation failed.");
    let missing = error!(io::Error::new(
        io::ErrorKind::NotFound,
        "private missing path"
    ));
    assert_eq!(
        user_message(&missing),
        "The requested file or directory does not exist."
    );
}

#[test]
fn yaml_errors_show_locations_without_echoing_document_contents() {
    let parse_error =
        yaml_serde::from_str::<Manifest>("requires: []\nsuper_secret_field: private-value\n")
            .unwrap_err();
    assert!(parse_error.to_string().contains("super_secret_field"));
    let message = user_message(&error!(parse_error));
    assert!(message.contains("Invalid YAML"));
    assert!(!message.contains("super_secret_field"));
    assert!(!message.contains("private-value"));
}
