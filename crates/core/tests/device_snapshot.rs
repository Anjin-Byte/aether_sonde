//! Integration tests for `Engine::device_snapshot`.
//!
//! Sharp-oracle assertions on the typed snapshot API:
//!   - End stations and repeaters return shape-uniform snapshots
//!     computed from `World` state.
//!   - Bridges return a snapshot whose queue/busy view reflects the
//!     runtime's per-port state at the moment of the call.
//!   - The snapshot is a pure-function read: calling it twice in a
//!     row produces equal values; the call has no side effects on
//!     the log or scheduled queue.
//!   - Unknown nodes return `None`.

#![allow(missing_docs)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

mod common;

use aether_sonde::device::DeviceSnapshot;
use aether_sonde::engine::Engine;
use aether_sonde::frame::{EtherType, Frame, MacAddress};
use aether_sonde::signal::{NodeId, SignalKind};
use aether_sonde::time::{BitRate, BitTime, Bits};
use aether_sonde::topology::{PortId, TopologyBuilder};

use common::{bridge_hd_topology, hd_pair, hd_three_via_repeater};

// ---------------------------------------------------------------------------
// End-station snapshot is computed from the world.
// ---------------------------------------------------------------------------

#[test]
fn end_station_snapshot_reports_port_count() {
    let (world, s1, _s2) = hd_pair(BitTime::from_micros(5));
    let engine = Engine::with_seed(world, 0);
    let snap = engine.device_snapshot(s1).unwrap();
    match snap {
        DeviceSnapshot::EndStation(es) => assert_eq!(es.port_count, 1),
        other => panic!("expected EndStation, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Repeater snapshot reports port_count and δ_h.
// ---------------------------------------------------------------------------

#[test]
fn repeater_snapshot_reports_port_count_and_delta_h() {
    let delta_h = BitTime::from_nanos(100);
    let (world, _s1, _s2, _s3) = hd_three_via_repeater(BitTime::from_micros(5), delta_h);
    // Repeater is the 4th node added (index 3).
    let engine = Engine::with_seed(world, 0);
    let r = NodeId::new(3);
    let snap = engine.device_snapshot(r).unwrap();
    match snap {
        DeviceSnapshot::Repeater(rs) => {
            assert_eq!(rs.port_count, 3);
            assert_eq!(rs.delta_h, delta_h);
        }
        other => panic!("expected Repeater, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Bridge snapshot reports BridgeData fields and (initially empty)
// runtime state.
// ---------------------------------------------------------------------------

#[test]
fn bridge_snapshot_initially_reports_zero_queues_and_no_busy() {
    let eta_b = Bits::new(64);
    let pi_b = BitTime::from_micros(1);
    let (world, _s1, _s2, br) = bridge_hd_topology(
        BitTime::from_micros(5),
        BitTime::from_micros(5),
        eta_b,
        pi_b,
    );
    let engine = Engine::with_seed(world, 0);
    let snap = engine.device_snapshot(br).unwrap();
    match snap {
        DeviceSnapshot::Bridge(bs) => {
            assert_eq!(bs.decode_threshold_bits, eta_b.as_u64());
            assert_eq!(bs.processing_delay, pi_b);
            // No traffic has happened; the runtime has not seen any
            // ports yet, so the per-port maps are empty.
            assert!(bs.egress_queue_depth.is_empty());
            assert!(bs.egress_busy.is_empty());
        }
        other => panic!("expected Bridge, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Bridge snapshot is idempotent (no side effects).
// ---------------------------------------------------------------------------

#[test]
fn bridge_snapshot_is_idempotent_and_pure() {
    let (world, s1, _s2, br) = bridge_hd_topology(
        BitTime::from_micros(5),
        BitTime::from_micros(5),
        Bits::new(64),
        BitTime::from_micros(1),
    );
    let mut engine = Engine::with_seed(world, 0);
    // Drive a frame so the bridge runtime touches per-port state.
    let frame = engine
        .register_frame_with(
            s1,
            Frame::ethernet(
                MacAddress::ZERO,
                MacAddress::ZERO,
                EtherType::IPV4,
                Bits::new(512),
            ),
            SignalKind::Frame,
            BitRate::ETHERNET_10M,
        )
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
    engine.run_until_idle();

    let log_len_before = engine.log().iter().count();
    let snap_a = engine.device_snapshot(br).unwrap();
    let snap_b = engine.device_snapshot(br).unwrap();
    let log_len_after = engine.log().iter().count();
    assert_eq!(snap_a, snap_b, "snapshot must be idempotent");
    assert_eq!(
        log_len_before, log_len_after,
        "snapshot must not append to the log"
    );
}

// ---------------------------------------------------------------------------
// Unknown nodes return None.
// ---------------------------------------------------------------------------

#[test]
fn unknown_node_snapshot_returns_none() {
    let engine = Engine::with_seed(TopologyBuilder::new().build().unwrap(), 0);
    assert!(engine.device_snapshot(NodeId::new(999)).is_none());
}

// ---------------------------------------------------------------------------
// Bridge snapshot reflects post-relay runtime state.
//
// After a frame has been relayed through the bridge, the egress port
// has been touched: it appears in the busy-flag map (cleared after
// TxEnd) and the queue map (drained, depth 0).
// ---------------------------------------------------------------------------

#[test]
fn bridge_snapshot_reflects_post_relay_state() {
    let (world, s1, _s2, br) = bridge_hd_topology(
        BitTime::from_micros(5),
        BitTime::from_micros(5),
        Bits::new(64),
        BitTime::from_micros(1),
    );
    let mut engine = Engine::with_seed(world, 0);
    let frame = engine
        .register_frame_with(
            s1,
            Frame::ethernet(
                MacAddress::ZERO,
                MacAddress::ZERO,
                EtherType::IPV4,
                Bits::new(512),
            ),
            SignalKind::Frame,
            BitRate::ETHERNET_10M,
        )
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
    engine.run_until_idle();

    let DeviceSnapshot::Bridge(bs) = engine.device_snapshot(br).unwrap() else {
        panic!("expected Bridge snapshot");
    };
    // Egress port 1 is the only port the relay touched (ingress was
    // port 0). After TxEnd the port should be unbusy and its queue
    // empty.
    let port_1 = PortId::new(1);
    let busy_1 = bs
        .egress_busy
        .iter()
        .find(|(p, _)| *p == port_1)
        .map(|(_, b)| *b);
    assert_eq!(
        busy_1,
        Some(false),
        "egress port 1 should be tracked and not busy"
    );
    let depth_1 = bs
        .egress_queue_depth
        .iter()
        .find(|(p, _)| *p == port_1)
        .map(|(_, d)| *d);
    assert_eq!(
        depth_1,
        Some(0),
        "egress port 1 queue should be drained to depth 0"
    );
}

// ---------------------------------------------------------------------------
// Snapshot variants are publicly exhaustive — adding a new device
// family forces every consumer's match to update.
// ---------------------------------------------------------------------------

#[test]
fn device_snapshot_variants_are_publicly_exhaustive() {
    let (world, s1, _s2) = hd_pair(BitTime::from_micros(5));
    let engine = Engine::with_seed(world, 0);
    let snap = engine.device_snapshot(s1).unwrap();
    let label = match snap {
        DeviceSnapshot::EndStation(_) => "end-station",
        DeviceSnapshot::Repeater(_) => "repeater",
        DeviceSnapshot::Bridge(_) => "bridge",
        DeviceSnapshot::Switch(_) => "switch",
    };
    assert_eq!(label, "end-station");
}
