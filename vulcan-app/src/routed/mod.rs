//! Reads a client routes to a daemon that already holds the vault's state
//! (Roadmap 10.5, `docs/specs/index-ownership-and-client-routing.md`).
//!
//! Each routed operation has one implementation here, run by the CLI against
//! the cache directly and by the daemon against its retained session, so a
//! routed answer is the direct answer computed elsewhere. Results cross the
//! wire as full-fidelity [`wire`] types and are printed by the client.

pub mod wire;
