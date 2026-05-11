//! Reusable application-layer workflow services for Vulcan.
//!
//! `vulcan-core` owns vault semantics, parsing, indexing, config models, and
//! synchronous domain logic. `vulcan-app` composes those primitives into
//! reusable workflows that may touch the filesystem or coordinate multiple
//! core operations without pulling in CLI/TUI concerns.
//!
//! Depend on this crate when you are building a local integration, daemon, MCP
//! handler, or test harness that wants Vulcan's higher-level workflows without
//! terminal rendering or `clap` parsing. The app layer remains synchronous and
//! returns typed reports that callers can serialize as JSON, display in a CLI,
//! or expose through future daemon endpoints.
//!
//! Common entrypoints:
//!
//! - Full local workflows: `notes`, `scan`, `browse`, `config`, `trust`, and
//!   `plugins`.
//! - Static export/site generation: `export`, `site`, and `serve`.
//! - Task and template automation: `tasks` and `templates`.
//! - Agent skill commands and custom tools: `tools`, which handles discovery,
//!   schema validation, permission ceilings, execution, linting, and authoring
//!   reports.
//! - Optional web workflows: `web`, enabled by the `web` feature.
//!
//! `vulcan-app` should not contain TUI state, terminal styling, editor/browser
//! launching, or direct stdout/stderr rendering. Those concerns belong in
//! `vulcan-cli` or a future daemon transport adapter.

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
#[cfg(feature = "web")]
pub mod web;

pub use error::AppError;
