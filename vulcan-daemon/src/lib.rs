#![forbid(unsafe_code)]

//! Long-lived Vulcan service boundaries.
//!
//! The initial slice owns the device-local multi-wiki registry. Async runtime,
//! HTTP, watcher, and scheduling modules will be added here without making the
//! registry depend on a running daemon.

pub mod alert_delivery;
pub mod alerts;
pub mod clone;
pub mod companion;
pub mod conflict_worker;
pub mod credentials;
pub mod daemon_host;
pub mod environment;
pub mod final_sync;
pub mod host;
pub mod http;
pub mod mcp_remote;
pub mod mcp_session;
pub mod mcp_state;
pub mod notifications;
pub mod process;
pub mod registry;
pub mod runtime;
pub mod semantic_worker;
pub mod service;
pub mod shutdown;
pub mod status;
pub mod supervisor;
pub mod sync;
pub mod termux_scheduler;
pub mod update_schedule;
pub mod watch;
