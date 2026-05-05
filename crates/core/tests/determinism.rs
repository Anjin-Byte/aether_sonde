//! Determinism battery — the §1.c contract at scale.
//!
//! `(initial_spec, seed, schedule, edit_history)` produces a
//! byte-identical log on every run. These tests probe seed sensitivity,
//! seed-independence under no-collision scenarios, and stability under
//! long runs with periodic edits.

#![allow(missing_docs)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

mod common;

use aether_sonde::engine::{Edit, Engine};
use aether_sonde::event::{Event, LoggedEvent};
use aether_sonde::signal::{NodeId, SignalKind};
use aether_sonde::time::{BitRate, BitTime, Bits};
use aether_sonde::topology::{PortId, SegmentId, TopologyBuilder};

use common::{ep, fd_pair, hd_pair, log_is_monotonic};

/// Run a complex scenario with a given seed and return the full log.
/// Mixes HD/FD/bridge construction, transmissions, and several edit
/// families so that byte-identity over the resulting log exercises the
/// determinism of every subsystem.
fn complex_scenario(seed: u64) -> Vec<LoggedEvent> {
    let mut engine = Engine::with_seed(TopologyBuilder::new().build().unwrap(), seed);

    // Build via apply_edit: 4 stations + 1 bridge.
    for _ in 0..4 {
        engine
            .apply_edit(Edit::AddEndStation { port_count: 1 })
            .unwrap();
    }
    engine
        .apply_edit(Edit::AddBridge {
            port_count: 2,
            decode_threshold: Bits::new(64),
            processing_delay: BitTime::from_nanos(500),
        })
        .unwrap();
    // s0—HD—bridge.0; s1—HD—bridge.1 (HD/HD). s2—FD—s3 (separate FD pair).
    engine
        .apply_edit(Edit::AddHdSegment {
            rate: BitRate::ETHERNET_10M,
            delay: BitTime::from_micros(1),
            a: ep(NodeId::new(0), 0),
            b: ep(NodeId::new(4), 0),
        })
        .unwrap();
    engine
        .apply_edit(Edit::AddHdSegment {
            rate: BitRate::ETHERNET_10M,
            delay: BitTime::from_micros(1),
            a: ep(NodeId::new(1), 0),
            b: ep(NodeId::new(4), 1),
        })
        .unwrap();
    engine
        .apply_edit(Edit::AddFdSegment {
            rate: BitRate::ETHERNET_1G,
            delay: BitTime::from_nanos(100),
            a: ep(NodeId::new(2), 0),
            b: ep(NodeId::new(3), 0),
        })
        .unwrap();

    // Schedule transmissions.
    let f0 = engine
        .register_frame(
            NodeId::new(0),
            Bits::new(64),
            SignalKind::Frame,
            BitRate::ETHERNET_10M,
        )
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, NodeId::new(0), f0);
    let f2 = engine
        .register_frame(
            NodeId::new(2),
            Bits::new(64),
            SignalKind::Frame,
            BitRate::ETHERNET_1G,
        )
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, NodeId::new(2), f2);
    engine.run_until(BitTime::from_micros(20));

    // A delay edit between transmission rounds.
    engine
        .apply_edit(Edit::SetSegmentDelay {
            segment: SegmentId::new(0),
            new_delay: BitTime::from_micros(2),
        })
        .unwrap();

    // Another transmission.
    let f1 = engine
        .register_frame(
            NodeId::new(1),
            Bits::new(64),
            SignalKind::Frame,
            BitRate::ETHERNET_10M,
        )
        .unwrap();
    engine.schedule_tx_attempt(BitTime::from_micros(100), NodeId::new(1), f1);
    engine.run_until_idle();

    engine.log().iter().copied().collect()
}

// --------------------------------------------------------------------------
// Same seed → byte-identical
// --------------------------------------------------------------------------

#[test]
fn replay_is_byte_identical_50_seeds() {
    for seed in 0..50u64 {
        let a = complex_scenario(seed);
        let b = complex_scenario(seed);
        assert_eq!(a, b, "logs diverged for seed {seed}");
        assert!(!a.is_empty());
    }
}

// --------------------------------------------------------------------------
// Seed-independence: in BEB-free scenarios, all seeds agree
// --------------------------------------------------------------------------

#[test]
fn seed_independent_for_no_collision_fd_scenario() {
    // Pure FD pair, single tx — BEB never invoked, so seed shouldn't
    // affect the log.
    let baseline = {
        let (world, s1, _) = fd_pair(BitTime::from_nanos(100), BitRate::ETHERNET_1G);
        let mut engine = Engine::with_seed(world, 0);
        let frame = engine
            .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_1G)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();
        engine.log().iter().copied().collect::<Vec<_>>()
    };
    for seed in 1..20u64 {
        let (world, s1, _) = fd_pair(BitTime::from_nanos(100), BitRate::ETHERNET_1G);
        let mut engine = Engine::with_seed(world, seed);
        let frame = engine
            .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_1G)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();
        let log: Vec<_> = engine.log().iter().copied().collect();
        assert_eq!(log, baseline, "seed {seed} diverged in BEB-free scenario");
    }
}

// --------------------------------------------------------------------------
// Long run stability
// --------------------------------------------------------------------------

#[test]
fn long_run_500_transmissions_completes_log_monotonic() {
    // 500 sequential transmissions over 50 ms simulation time on an FD
    // pair. Periodically apply SetSegmentDelay edits. Verify run
    // completes, log is monotonic, and the count of TxAttempt entries
    // matches the schedule.
    let (world, s1, _) = fd_pair(BitTime::from_nanos(100), BitRate::ETHERNET_1G);
    let mut engine = Engine::with_seed(world, 99);
    for i in 0..500u32 {
        let frame = engine
            .register_frame(s1, Bits::new(64), SignalKind::Frame, BitRate::ETHERNET_1G)
            .unwrap();
        // Spread transmissions: 100µs apart.
        let t = BitTime::from_micros(u64::from(i) * 100);
        engine.schedule_tx_attempt(t, s1, frame);
        // Edit every 25 transmissions.
        if i % 25 == 24 {
            engine
                .apply_edit(Edit::SetSegmentDelay {
                    segment: SegmentId::new(0),
                    new_delay: BitTime::from_nanos(100 + u64::from(i)),
                })
                .unwrap();
        }
    }
    engine.run_until_idle();

    assert!(log_is_monotonic(engine.log()));
    let attempts = engine
        .log()
        .iter()
        .filter(|e| matches!(e.event, Event::TxAttempt { .. }))
        .count();
    assert_eq!(attempts, 500);
}

// --------------------------------------------------------------------------
// Determinism under removal-and-readd interleaved with transmissions
// --------------------------------------------------------------------------

#[test]
fn determinism_under_remove_readd_interleave() {
    // Confirm the round 10c removal path is fully deterministic when
    // interleaved with transmissions.
    let run = |seed: u64| -> Vec<LoggedEvent> {
        let tau = BitTime::from_micros(5);
        let (world, s1, s2) = hd_pair(tau);
        let mut engine = Engine::with_seed(world, seed);
        for i in 0..5u32 {
            let frame = engine
                .register_frame(s1, Bits::new(64), SignalKind::Frame, BitRate::ETHERNET_10M)
                .unwrap();
            engine.schedule_tx_attempt(BitTime::from_micros(u64::from(i) * 100), s1, frame);
            if i % 2 == 1 {
                engine
                    .apply_edit(Edit::DisconnectPort {
                        node: s2,
                        port: PortId::new(0),
                    })
                    .unwrap();
                engine
                    .apply_edit(Edit::AddHdSegment {
                        rate: BitRate::ETHERNET_10M,
                        delay: tau,
                        a: ep(s1, 0),
                        b: ep(s2, 0),
                    })
                    .unwrap();
            }
        }
        engine.run_until_idle();
        engine.log().iter().copied().collect()
    };
    assert_eq!(run(31), run(31));
}
