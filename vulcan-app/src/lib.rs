//! Reusable application-layer workflow services for Vulcan.
//!
//! `vulcan-core` owns vault semantics, parsing, indexing, config models, and
//! synchronous domain logic. `vulcan-app` composes those primitives into
//! reusable workflows that may touch the filesystem or coordinate multiple
//! core operations without pulling in CLI/TUI concerns.

mod error;

pub mod config;
pub mod notes;
pub mod plugins;
pub mod templates;
pub mod trust;

pub use error::AppError;
