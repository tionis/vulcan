//! Reusable application-layer workflow services for Vulcan.
//!
//! `vulcan-core` owns vault semantics, parsing, indexing, config models, and
//! synchronous domain logic. `vulcan-app` composes those primitives into
//! reusable workflows that may touch the filesystem or coordinate multiple
//! core operations without pulling in CLI/TUI concerns.

mod error;

pub mod browse;
pub mod config;
pub mod export;
pub mod notes;
pub mod plugins;
pub mod scan;
pub mod serve;
pub mod site;
pub mod tasks;
pub mod templates;
pub mod tools;
pub mod trust;
pub mod web;

pub use error::AppError;
