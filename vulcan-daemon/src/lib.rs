#![forbid(unsafe_code)]

//! Long-lived Vulcan service boundaries.
//!
//! The initial slice owns the device-local multi-wiki registry. Async runtime,
//! HTTP, watcher, and scheduling modules will be added here without making the
//! registry depend on a running daemon.

pub mod clone;
pub mod companion;
pub mod credentials;
pub mod http;
pub mod process;
pub mod registry;
pub mod runtime;
pub mod semantic_worker;
pub mod service;
pub mod status;
pub mod supervisor;
pub mod sync;
pub mod watch;
