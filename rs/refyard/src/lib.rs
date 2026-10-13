//! Track declared shared resources using manifests, a machine registry, and a disposable index.
//!
//! Callers of filesystem mutation operations should hold `storage::lock_state` for the complete
//! operation. The CLI does so automatically; source files must remain stable during cleanup.

pub mod cleanup;
pub mod configuration;
pub mod discovery;
pub mod errors;
pub mod graph;
pub mod manifest;
pub mod storage;
