//! Integration tests for `Engine::apply_device_command`.
//!
//! Sharp-oracle assertions on the typed command boundary:
//!   - Unknown nodes return `UnknownNode`.
//!   - Commands with no applicable device-kind handler return
//!     `NotApplicable` (round 4: bridges have no commandable state).
//!   - Successful applications log a single
//!     `DeviceCommandApplied` entry per call.
//!
//! Switch-specific commands land in `crates/core/tests/switch_invariants.rs`
//! when the Switch device family ships in step 6.

#![allow(missing_docs)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

mod common;

use aether_sonde::device::{DeviceCommand, DeviceCommandError};
use aether_sonde::engine::Engine;
use aether_sonde::event::Event;
use aether_sonde::frame::MacAddress;
use aether_sonde::signal::NodeId;
use aether_sonde::time::{BitTime, Bits};
use aether_sonde::topology::{PortId, TopologyBuilder};

use common::bridge_hd_topology;

// ---------------------------------------------------------------------------
// Unknown-node target → UnknownNode.
// ---------------------------------------------------------------------------

#[test]
fn unknown_node_target_returns_unknown_node_error() {
    let mut engine = Engine::with_seed(TopologyBuilder::new().build().unwrap(), 0);
    let unknown = NodeId::new(99);
    let err = engine
        .apply_device_command(DeviceCommand::FlushMacTable { node: unknown })
        .unwrap_err();
    assert_eq!(err, DeviceCommandError::UnknownNode { node: unknown });
}

// ---------------------------------------------------------------------------
// Bridge has no commandable state → NotApplicable.
// ---------------------------------------------------------------------------

#[test]
fn bridge_target_returns_not_applicable_for_mac_table_commands() {
    let (world, _s1, _s2, br) = bridge_hd_topology(
        BitTime::from_micros(5),
        BitTime::from_micros(5),
        Bits::new(64),
        BitTime::from_micros(1),
    );
    let mut engine = Engine::with_seed(world, 0);
    let err = engine
        .apply_device_command(DeviceCommand::InsertMacEntry {
            node: br,
            mac: MacAddress::new([1, 2, 3, 4, 5, 6]),
            port: PortId::new(0),
        })
        .unwrap_err();
    assert!(matches!(
        err,
        DeviceCommandError::NotApplicable { .. }
    ));
}

// ---------------------------------------------------------------------------
// Failed commands don't log a DeviceCommandApplied entry.
// ---------------------------------------------------------------------------

#[test]
fn failed_command_does_not_log_applied_entry() {
    let (world, _s1, _s2, br) = bridge_hd_topology(
        BitTime::from_micros(5),
        BitTime::from_micros(5),
        Bits::new(64),
        BitTime::from_micros(1),
    );
    let mut engine = Engine::with_seed(world, 0);
    let log_len_before = engine.log().iter().count();
    let _ = engine.apply_device_command(DeviceCommand::FlushMacTable { node: br });
    // The error path returns before scheduling. The dispatch loop
    // hasn't run, so the log length is unchanged.
    let log_len_after = engine.log().iter().count();
    assert_eq!(log_len_before, log_len_after);
}

// ---------------------------------------------------------------------------
// `target_node` round-trips for every variant.
// ---------------------------------------------------------------------------

#[test]
fn target_node_returns_correct_node_for_every_variant() {
    let n = NodeId::new(7);
    assert_eq!(
        DeviceCommand::InsertMacEntry {
            node: n,
            mac: MacAddress::ZERO,
            port: PortId::new(0),
        }
        .target_node(),
        n
    );
    assert_eq!(
        DeviceCommand::RemoveMacEntry {
            node: n,
            mac: MacAddress::ZERO,
        }
        .target_node(),
        n
    );
    assert_eq!(
        DeviceCommand::FlushMacTable { node: n }.target_node(),
        n
    );
    assert_eq!(
        DeviceCommand::SetSwitchAgingThreshold {
            node: n,
            threshold: BitTime::from_micros(100),
        }
        .target_node(),
        n
    );
}

// ---------------------------------------------------------------------------
// Determinism: applying the same command sequence to two engines with
// the same seed produces byte-identical logs.
//
// (Round 4 has no command that succeeds against a bridge, so this test
// uses the failure path — both engines see the same error and same
// (unchanged) log length.)
// ---------------------------------------------------------------------------

#[test]
fn determinism_failed_command_paths_match_across_engines() {
    let make = || {
        let (world, _s1, _s2, br) = bridge_hd_topology(
            BitTime::from_micros(5),
            BitTime::from_micros(5),
            Bits::new(64),
            BitTime::from_micros(1),
        );
        let engine = Engine::with_seed(world, 1);
        (engine, br)
    };
    let (mut e1, br1) = make();
    let (mut e2, br2) = make();
    let r1 = e1.apply_device_command(DeviceCommand::FlushMacTable { node: br1 });
    let r2 = e2.apply_device_command(DeviceCommand::FlushMacTable { node: br2 });
    assert_eq!(r1, r2);
    let log1: Vec<_> = e1.log().iter().map(|e| (e.key, e.event)).collect();
    let log2: Vec<_> = e2.log().iter().map(|e| (e.key, e.event)).collect();
    assert_eq!(log1, log2);
}

// ---------------------------------------------------------------------------
// `DeviceCommandApplied` event is in the LocalDecision phase.
// ---------------------------------------------------------------------------

#[test]
fn device_command_applied_event_phase_is_local_decision() {
    use aether_sonde::event::Phase;
    // Synthesize the event and check its phase.
    let ev = Event::DeviceCommandApplied {
        node: NodeId::new(0),
    };
    assert_eq!(ev.phase(), Phase::LocalDecision);
}
