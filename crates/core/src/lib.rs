//! Aether Sonde simulation core.
//!
//! Discrete-event simulator for finite-length signal propagation and CSMA/CD
//! on graphs, generalized to mixed half-duplex shared media, full-duplex
//! point-to-point links, repeater/hub components, and bridge nodes.
//!
//! # Scope
//!
//! Phase 1 of the workspace plan: a single pure-core crate. No I/O, no
//! clocks, no randomness sourced from entropy, no parallelism. The simulator
//! is its own reference implementation: optimized paths land in sibling
//! crates and are validated against this one.
//!
//! # Module structure
//!
//! Modules are introduced in dependency order, leaves first.
//!
//! - [`time`] — canonical time, bit count, and bit rate types.
//! - [`signal`] — propagation primitive: signals, signal kinds, source IDs.
//! - [`resource`] — resource identifiers (collision / serializer), claims,
//!   and transmissions.
//! - [`policy`] — backoff (BEB), jam, and inter-frame-gap policies.
//! - [`topology`] — typed segments, nodes, the `TopologyBuilder` →
//!   `World` state transition, and global axiom validation.
//! - [`bridge`] — frame-relay primitives: eligibility timing, egress
//!   queue, and forwarding policies.
//! - [`event`] — sealed `Event` enum, `Phase` ordering, `EventKey`,
//!   append-only `Log`.
//! - [`engine`] — discrete-event scheduler. Round 8a: ordinary HD
//!   propagation path; collisions, FD, bridge land in 8b–8d.
//! - [`observe`] — endpoint observable queries: `carrier_sense`,
//!   `collision_detect`, `first_collision_detect_at`, `receive_complete`.

pub mod bridge;
pub mod device;
pub mod engine;
pub mod event;
pub mod frame;
pub mod observe;
pub mod policy;
pub mod resource;
pub mod signal;
pub mod time;
pub mod topology;
