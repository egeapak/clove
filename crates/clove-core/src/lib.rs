//! clove-core: item model, file store, dependency-graph engine, ID generation.
//!
//! This crate is pure: no SQLite, no async, no IPC, no clap. Correctness for the
//! whole tool lives here — the index and daemon are accelerators layered on top.

pub mod error;
pub mod id;
pub mod limits;
pub mod repo;

pub use error::CloveError;
pub use id::CloveId;
