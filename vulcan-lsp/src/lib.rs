#![forbid(unsafe_code)]

//! `vulcan lsp`: a synchronous Language Server Protocol transport over
//! Vulcan's editor services (Roadmap 9.40,
//! `docs/specs/editor-integration.md`).
//!
//! This is the only crate that knows LSP. Vault semantics live in
//! `vulcan-core` and the transport-neutral `vulcan_app::editor` services;
//! this crate negotiates the protocol, converts URIs and positions, and
//! routes requests. It runs without a daemon and never writes unsaved buffer
//! text to the cache.

mod convert;
mod handlers;
mod server;

pub use server::{run, run_stdio, LspError, LspOptions};

/// Version of the custom `vulcan/` protocol advertised under
/// `capabilities.experimental.vulcan`. Breaking changes bump it.
pub const PROTOCOL_VERSION: u32 = 1;
