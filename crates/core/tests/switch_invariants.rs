//! Sharp-oracle integration tests for the learning-switch device.
//!
//! These tests prove the round-4 framework's three hardest claims:
//!   - **Stateful runtime that mutates on frame arrival**
//!     (`auto_learning_*` tests)
//!   - **State readable as a typed snapshot**
//!     (`*_snapshot_*` tests)
//!   - **State editable via typed commands**
//!     (`manual_*`, `flush_*`, `aging_*` tests)
//!
//! The tests assert exact MAC-table contents and aging behavior;
//! they don't accept "snapshot is non-empty" or similar weak oracles.

#![allow(missing_docs)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

mod common;

use aether_sonde::device::{
    DeviceCommand, DeviceCommandError, DeviceSnapshot, MacEntryOrigin,
};
use aether_sonde::engine::{Edit, Engine};
use aether_sonde::event::Event;
use aether_sonde::frame::{EtherType, Frame, MacAddress};
use aether_sonde::signal::{NodeId, SignalKind};
use aether_sonde::time::{BitRate, BitTime, Bits};
use aether_sonde::topology::{Endpoint, PortId, SwitchData, TopologyBuilder};

fn ep(node: NodeId, port: u32) -> Endpoint {
    Endpoint::new(node, PortId::new(port))
}

/// Topology: three end stations, each with a single port, all
/// connected to a 3-port switch.
///
///   s1 — sw[0]
///   s2 — sw[1]
///   s3 — sw[2]
///
/// Returns `(world, s1, s2, s3, switch)`.
fn switch_3_topology(
    aging_threshold: BitTime,
) -> (
    aether_sonde::topology::World,
    NodeId,
    NodeId,
    NodeId,
    NodeId,
) {
    let mut b = TopologyBuilder::new();
    let s1 = b.add_end_station(1);
    let s2 = b.add_end_station(1);
    let s3 = b.add_end_station(1);
    let sw = b.add_switch(
        3,
        SwitchData {
            decode_threshold: Bits::new(64),
            processing_delay: BitTime::from_micros(1),
            mac_table_capacity: 0,
            aging_threshold,
        },
    );
    let delay = BitTime::from_micros(5);
    b.add_hd_segment(BitRate::ETHERNET_10M, delay, ep(s1, 0), ep(sw, 0))
        .unwrap();
    b.add_hd_segment(BitRate::ETHERNET_10M, delay, ep(s2, 0), ep(sw, 1))
        .unwrap();
    b.add_hd_segment(BitRate::ETHERNET_10M, delay, ep(s3, 0), ep(sw, 2))
        .unwrap();
    (b.build().unwrap(), s1, s2, s3, sw)
}

fn mac(b: u8) -> MacAddress {
    MacAddress::new([0x00, 0x00, 0x00, 0x00, 0x00, b])
}

// ---------------------------------------------------------------------------
// AUTO-LEARNING — `on_frame_arrive` populates the MAC table.
// ---------------------------------------------------------------------------

#[test]
fn auto_learning_records_source_mac_after_first_frame() {
    let (world, s1, _s2, _s3, sw) = switch_3_topology(BitTime::ZERO);
    let mut engine = Engine::with_seed(world, 0);
    let frame = engine
        .register_frame_with(
            s1,
            Frame::ethernet(mac(2), mac(1), EtherType::IPV4, Bits::new(512)),
            SignalKind::Frame,
            BitRate::ETHERNET_10M,
        )
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
    engine.run_until_idle();

    // s1's MAC should now map to switch port 0.
    let snap = engine.device_snapshot(sw).unwrap();
    let DeviceSnapshot::Switch(sw_snap) = snap else {
        panic!("expected Switch snapshot");
    };
    let entry = sw_snap.mac_table.iter().find(|e| e.mac == mac(1));
    let entry = entry.expect("s1's MAC should be learned");
    assert_eq!(entry.port, PortId::new(0));
    assert_eq!(entry.origin, MacEntryOrigin::Learned);
    // s2 sent nothing; its MAC is not yet in the table.
    assert!(sw_snap.mac_table.iter().all(|e| e.mac != mac(2)));
}

// ---------------------------------------------------------------------------
// AUTO-LEARNING enables unicast on the second frame.
//
// First frame from s1→s2: switch floods (table empty for s2). Second
// frame from s2→s1: switch unicasts to port 0 only (s1's MAC was
// learned on the first frame). The byte-identical floor is: s3 sees
// the first frame but does NOT see the second.
// ---------------------------------------------------------------------------

#[test]
fn learned_destination_unicasts_instead_of_flooding() {
    let (world, s1, s2, s3, sw) = switch_3_topology(BitTime::ZERO);
    let mut engine = Engine::with_seed(world, 0);

    // Frame 1: s1 → s2 (s2's MAC unknown to switch → flood).
    let f1 = engine
        .register_frame_with(
            s1,
            Frame::ethernet(mac(2), mac(1), EtherType::IPV4, Bits::new(512)),
            SignalKind::Frame,
            BitRate::ETHERNET_10M,
        )
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, f1);
    engine.run_until_idle();

    // s3 should have seen f1 (FrontArrive at s3 from a switch-sourced
    // signal).
    let s3_arrivals_after_f1 = engine
        .log()
        .iter()
        .filter(|e| {
            matches!(
                e.event,
                Event::FrontArrive { node, .. } if node == s3
            )
        })
        .count();
    assert!(
        s3_arrivals_after_f1 >= 1,
        "s3 should have received a flood arrival from f1"
    );

    // Frame 2: s2 → s1 (s1's MAC learned → unicast to port 0).
    let f2 = engine
        .register_frame_with(
            s2,
            Frame::ethernet(mac(1), mac(2), EtherType::IPV4, Bits::new(512)),
            SignalKind::Frame,
            BitRate::ETHERNET_10M,
        )
        .unwrap();
    let t2 = BitTime::from_millis(1);
    engine.schedule_tx_attempt(t2, s2, f2);
    engine.run_until_idle();

    // After f2, s3 should have seen NO new arrivals (f2 unicast to s1
    // only).
    let s3_arrivals_total = engine
        .log()
        .iter()
        .filter(|e| matches!(e.event, Event::FrontArrive { node, .. } if node == s3))
        .count();
    assert_eq!(
        s3_arrivals_total, s3_arrivals_after_f1,
        "s3 should not see f2 because the switch unicasts to s1's port"
    );

    // Snapshot now carries both s1 and s2 in the table.
    let DeviceSnapshot::Switch(sw_snap) = engine.device_snapshot(sw).unwrap() else {
        panic!("expected Switch snapshot");
    };
    assert!(sw_snap.mac_table.iter().any(|e| e.mac == mac(1)));
    assert!(sw_snap.mac_table.iter().any(|e| e.mac == mac(2)));
}

// ---------------------------------------------------------------------------
// MANUAL OVERRIDE — `InsertMacEntry` populates the table directly.
// ---------------------------------------------------------------------------

#[test]
fn manual_insert_mac_entry_appears_in_snapshot() {
    let (world, _s1, _s2, _s3, sw) = switch_3_topology(BitTime::ZERO);
    let mut engine = Engine::with_seed(world, 0);
    engine
        .apply_device_command(DeviceCommand::InsertMacEntry {
            node: sw,
            mac: mac(42),
            port: PortId::new(2),
        })
        .unwrap();

    let DeviceSnapshot::Switch(snap) = engine.device_snapshot(sw).unwrap() else {
        panic!("expected Switch snapshot");
    };
    let entry = snap
        .mac_table
        .iter()
        .find(|e| e.mac == mac(42))
        .expect("manual entry should appear in snapshot");
    assert_eq!(entry.port, PortId::new(2));
    assert_eq!(entry.origin, MacEntryOrigin::ManualInsert);
}

// ---------------------------------------------------------------------------
// FLUSH — `FlushMacTable` empties the table.
// ---------------------------------------------------------------------------

#[test]
fn flush_mac_table_empties_the_table() {
    let (world, _s1, _s2, _s3, sw) = switch_3_topology(BitTime::ZERO);
    let mut engine = Engine::with_seed(world, 0);
    engine
        .apply_device_command(DeviceCommand::InsertMacEntry {
            node: sw,
            mac: mac(1),
            port: PortId::new(0),
        })
        .unwrap();
    engine
        .apply_device_command(DeviceCommand::InsertMacEntry {
            node: sw,
            mac: mac(2),
            port: PortId::new(1),
        })
        .unwrap();
    engine
        .apply_device_command(DeviceCommand::FlushMacTable { node: sw })
        .unwrap();

    let DeviceSnapshot::Switch(snap) = engine.device_snapshot(sw).unwrap() else {
        panic!("expected Switch snapshot");
    };
    assert!(snap.mac_table.is_empty());
}

// ---------------------------------------------------------------------------
// REMOVE — `RemoveMacEntry` removes a single entry.
// ---------------------------------------------------------------------------

#[test]
fn remove_mac_entry_removes_only_the_targeted_entry() {
    let (world, _s1, _s2, _s3, sw) = switch_3_topology(BitTime::ZERO);
    let mut engine = Engine::with_seed(world, 0);
    engine
        .apply_device_command(DeviceCommand::InsertMacEntry {
            node: sw,
            mac: mac(1),
            port: PortId::new(0),
        })
        .unwrap();
    engine
        .apply_device_command(DeviceCommand::InsertMacEntry {
            node: sw,
            mac: mac(2),
            port: PortId::new(1),
        })
        .unwrap();
    engine
        .apply_device_command(DeviceCommand::RemoveMacEntry {
            node: sw,
            mac: mac(1),
        })
        .unwrap();

    let DeviceSnapshot::Switch(snap) = engine.device_snapshot(sw).unwrap() else {
        panic!("expected Switch snapshot");
    };
    assert_eq!(snap.mac_table.len(), 1);
    assert_eq!(snap.mac_table[0].mac, mac(2));
}

// ---------------------------------------------------------------------------
// AGING — entries expire after `aging_threshold`.
// ---------------------------------------------------------------------------

#[test]
fn aging_expires_learned_entries_after_threshold() {
    let aging = BitTime::from_micros(50);
    let (world, s1, _s2, _s3, sw) = switch_3_topology(aging);
    let mut engine = Engine::with_seed(world, 0);

    // Learn s1's MAC at t = 0 by sending a frame.
    let f1 = engine
        .register_frame_with(
            s1,
            Frame::ethernet(mac(2), mac(1), EtherType::IPV4, Bits::new(64)),
            SignalKind::Frame,
            BitRate::ETHERNET_10M,
        )
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, f1);
    // Let the frame propagate and the switch learn.
    engine.run_until(BitTime::from_micros(20));

    let DeviceSnapshot::Switch(snap_pre) = engine.device_snapshot(sw).unwrap() else {
        panic!("expected Switch");
    };
    assert!(snap_pre.mac_table.iter().any(|e| e.mac == mac(1)));

    // Run far past the aging threshold without any new frames. The
    // aging tick should expire s1's learned entry.
    engine.run_until(BitTime::from_micros(500));

    let DeviceSnapshot::Switch(snap_post) = engine.device_snapshot(sw).unwrap() else {
        panic!("expected Switch");
    };
    assert!(
        !snap_post.mac_table.iter().any(|e| e.mac == mac(1)),
        "learned entry should have aged out"
    );
    // The log should contain at least one AgingTick at the switch.
    let tick_count = engine
        .log()
        .iter()
        .filter(|e| matches!(e.event, Event::AgingTick { node } if node == sw))
        .count();
    assert!(tick_count >= 1, "at least one AgingTick should have fired");
}

// ---------------------------------------------------------------------------
// AGING — manual entries are NOT aged out.
// ---------------------------------------------------------------------------

#[test]
fn aging_does_not_expire_manual_entries() {
    let aging = BitTime::from_micros(50);
    let (world, _s1, _s2, _s3, sw) = switch_3_topology(aging);
    let mut engine = Engine::with_seed(world, 0);
    engine
        .apply_device_command(DeviceCommand::InsertMacEntry {
            node: sw,
            mac: mac(99),
            port: PortId::new(0),
        })
        .unwrap();

    // Without any frames, the switch wouldn't normally schedule an
    // aging tick (manual inserts don't do so). Force one by also
    // learning, then run past the threshold.
    let f = engine
        .register_frame_with(
            NodeId::new(0), // s1
            Frame::ethernet(mac(2), mac(1), EtherType::IPV4, Bits::new(64)),
            SignalKind::Frame,
            BitRate::ETHERNET_10M,
        )
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, NodeId::new(0), f);
    engine.run_until(BitTime::from_micros(500));

    let DeviceSnapshot::Switch(snap) = engine.device_snapshot(sw).unwrap() else {
        panic!("expected Switch");
    };
    assert!(
        snap.mac_table.iter().any(|e| e.mac == mac(99)),
        "manual entry must survive aging"
    );
}

// ---------------------------------------------------------------------------
// SET_AGING — `SetSwitchAgingThreshold` updates the snapshot.
// ---------------------------------------------------------------------------

#[test]
fn set_switch_aging_threshold_updates_snapshot() {
    let (world, _s1, _s2, _s3, sw) = switch_3_topology(BitTime::from_micros(10));
    let mut engine = Engine::with_seed(world, 0);
    let DeviceSnapshot::Switch(snap_before) = engine.device_snapshot(sw).unwrap() else {
        panic!();
    };
    assert_eq!(snap_before.aging_threshold, BitTime::from_micros(10));

    engine
        .apply_device_command(DeviceCommand::SetSwitchAgingThreshold {
            node: sw,
            threshold: BitTime::from_micros(200),
        })
        .unwrap();
    let DeviceSnapshot::Switch(snap_after) = engine.device_snapshot(sw).unwrap() else {
        panic!();
    };
    assert_eq!(snap_after.aging_threshold, BitTime::from_micros(200));
}

// ---------------------------------------------------------------------------
// EDIT::AddSwitch — adding a switch via the post-build edit path
// installs a runtime so subsequent commands work.
// ---------------------------------------------------------------------------

#[test]
fn add_switch_via_edit_installs_runtime() {
    let mut engine = Engine::with_seed(TopologyBuilder::new().build().unwrap(), 0);
    engine
        .apply_edit(Edit::AddSwitch {
            port_count: 4,
            data: SwitchData {
                decode_threshold: Bits::new(64),
                processing_delay: BitTime::from_micros(1),
                mac_table_capacity: 0,
                aging_threshold: BitTime::ZERO,
            },
        })
        .unwrap();
    let sw = NodeId::new(0);
    let snap = engine.device_snapshot(sw).unwrap();
    assert!(matches!(snap, DeviceSnapshot::Switch(_)));
    // Apply a command — proves the runtime is wired up.
    engine
        .apply_device_command(DeviceCommand::InsertMacEntry {
            node: sw,
            mac: mac(7),
            port: PortId::new(0),
        })
        .unwrap();
    let DeviceSnapshot::Switch(snap2) = engine.device_snapshot(sw).unwrap() else {
        panic!();
    };
    assert!(snap2.mac_table.iter().any(|e| e.mac == mac(7)));
}

// ---------------------------------------------------------------------------
// COMMAND ERROR — unknown node returns UnknownNode (sanity check the
// boundary still works for switch-applicable commands).
// ---------------------------------------------------------------------------

#[test]
fn switch_command_unknown_node_returns_unknown_node_error() {
    let (world, _s1, _s2, _s3, _sw) = switch_3_topology(BitTime::ZERO);
    let mut engine = Engine::with_seed(world, 0);
    let unknown = NodeId::new(999);
    let err = engine
        .apply_device_command(DeviceCommand::FlushMacTable { node: unknown })
        .unwrap_err();
    assert_eq!(err, DeviceCommandError::UnknownNode { node: unknown });
}
