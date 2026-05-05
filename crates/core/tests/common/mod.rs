//! Shared scenario builders for the integration test suite.
//!
//! These helpers compose only public API of `aether-sonde` — they cannot
//! depend on any `pub(crate)` item. Integration tests in sibling files
//! `mod common;` to use them.

#![allow(
    dead_code,
    unreachable_pub,
    reason = "shared across multiple integration binaries; each uses a subset"
)]
#![allow(missing_docs)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code: panicking on impossible states is the right behavior"
)]

use aether_sonde::engine::Engine;
use aether_sonde::event::{Event, Log};
use aether_sonde::signal::NodeId;
use aether_sonde::time::{BitRate, BitTime, Bits};
use aether_sonde::topology::{Endpoint, PortId, TopologyBuilder, World};

pub fn ep(node: NodeId, port: u32) -> Endpoint {
    Endpoint::new(node, PortId::new(port))
}

/// Two end stations joined by a single HD segment at 10 Mbps.
/// Returns the world and the two station IDs.
pub fn hd_pair(delay: BitTime) -> (World, NodeId, NodeId) {
    let mut b = TopologyBuilder::new();
    let s1 = b.add_end_station(1);
    let s2 = b.add_end_station(1);
    b.add_hd_segment(BitRate::ETHERNET_10M, delay, ep(s1, 0), ep(s2, 0))
        .unwrap();
    (b.build().unwrap(), s1, s2)
}

/// Two end stations joined by a single FD segment at the given rate.
pub fn fd_pair(delay: BitTime, rate: BitRate) -> (World, NodeId, NodeId) {
    let mut b = TopologyBuilder::new();
    let s1 = b.add_end_station(1);
    let s2 = b.add_end_station(1);
    b.add_fd_segment(rate, delay, ep(s1, 0), ep(s2, 0)).unwrap();
    (b.build().unwrap(), s1, s2)
}

/// Three stations in one HD collision domain via a 3-port repeater:
/// `s1 — r — s2`, `s1 — r — s3` (r is the hub).
pub fn hd_three_via_repeater(delay: BitTime, delta_h: BitTime) -> (World, NodeId, NodeId, NodeId) {
    let mut b = TopologyBuilder::new();
    let s1 = b.add_end_station(1);
    let s2 = b.add_end_station(1);
    let s3 = b.add_end_station(1);
    let r = b.add_repeater(3, delta_h);
    b.add_hd_segment(BitRate::ETHERNET_10M, delay, ep(s1, 0), ep(r, 0))
        .unwrap();
    b.add_hd_segment(BitRate::ETHERNET_10M, delay, ep(s2, 0), ep(r, 1))
        .unwrap();
    b.add_hd_segment(BitRate::ETHERNET_10M, delay, ep(s3, 0), ep(r, 2))
        .unwrap();
    (b.build().unwrap(), s1, s2, s3)
}

/// HD-1 → bridge → HD-1: two single-station HD components joined by a bridge.
/// Returns `(world, s1, s2, bridge)`.
pub fn bridge_hd_topology(
    delay_a: BitTime,
    delay_b: BitTime,
    decode_threshold: Bits,
    processing_delay: BitTime,
) -> (World, NodeId, NodeId, NodeId) {
    let mut b = TopologyBuilder::new();
    let s1 = b.add_end_station(1);
    let s2 = b.add_end_station(1);
    let br = b.add_bridge(2, decode_threshold, processing_delay);
    b.add_hd_segment(BitRate::ETHERNET_10M, delay_a, ep(s1, 0), ep(br, 0))
        .unwrap();
    b.add_hd_segment(BitRate::ETHERNET_10M, delay_b, ep(s2, 0), ep(br, 1))
        .unwrap();
    (b.build().unwrap(), s1, s2, br)
}

/// Construct a fresh empty engine.
pub fn empty_engine(seed: u64) -> Engine {
    Engine::with_seed(TopologyBuilder::new().build().unwrap(), seed)
}

/// Count log entries matching a predicate.
pub fn count_events<F>(log: &Log, predicate: F) -> usize
where
    F: Fn(&Event) -> bool,
{
    log.iter().filter(|e| predicate(&e.event)).count()
}

/// True if the log's timestamps are non-decreasing in insertion order.
pub fn log_is_monotonic(log: &Log) -> bool {
    let mut prev = BitTime::ZERO;
    for entry in log.iter() {
        if entry.key.time < prev {
            return false;
        }
        prev = entry.key.time;
    }
    true
}
