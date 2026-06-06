//! NexusView forensic triage engine — pure, interface-agnostic core (RNF-04).
//!
//! Pipeline: [`MappedFile`](mmap) → [`LineIndex`](index) → [`Dataset`] over a
//! [`ParserSchema`], queried with [`Query`] to produce a [`View`]. No component
//! here knows about Swift, FFI, or any UI: the same engine backs the desktop
//! app, a future CLI, and the MCP server.
//!
//! Design invariants:
//! - The file is memory-mapped, never heap-loaded (RF-02, RNF-02).
//! - The data path never panics; bad bytes are sanitized (RNF-05).
//! - Heavy work (indexing, search) is parallel and off any UI thread (RNF-01/03).

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod bloom;
pub mod encoding;
pub mod error;
pub mod export;
pub mod group;
pub mod query;
pub mod schema;
pub mod sort;
pub mod tags;
pub mod view;

mod dataset;
mod index;
mod mmap;
mod parser;

pub use dataset::Dataset;
pub use error::{NexusError, Result};
pub use group::{GroupTree, Item};
pub use query::Query;
pub use schema::ParserSchema;
pub use sort::SortKey;
pub use view::View;

/// Semantic version of the engine, surfaced through the FFI for diagnostics.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
