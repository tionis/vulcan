#![forbid(unsafe_code)]

//! Long-lived Vulcan service boundaries.
//!
//! The initial slice owns the device-local multi-wiki registry. Async runtime,
//! HTTP, watcher, and scheduling modules will be added here without making the
//! registry depend on a running daemon.

pub mod clone;
pub mod registry;
pub mod sync;
