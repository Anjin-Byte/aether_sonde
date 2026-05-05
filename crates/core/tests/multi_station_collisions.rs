//! Multi-station Theorem-1 oracles: CSMA/CD's defining behavior beyond
//! the 2-station case. Each test asserts exact closed-form timestamps
//! derived from the repeater axiom (A3): a foreign signal reaches a
//! station at `d_uv = sum(τ_segments) + (n_repeaters_traversed) * δ_h`,
//! where the source's own `δ_h` is not paid.

#![allow(missing_docs)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

mod common;

use aether_sonde::engine::Engine;
use aether_sonde::event::Event;
use aether_sonde::observe;
use aether_sonde::signal::{NodeId, SignalKind};
use aether_sonde::time::{BitRate, BitTime, Bits};
use aether_sonde::topology::TopologyBuilder;

use common::{count_events, ep, hd_three_via_repeater};

// ---------------------------------------------------------------------------
// 1. Three stations, two transmit simultaneously, both detect collision
//    at the same closed-form time.
// ---------------------------------------------------------------------------

#[test]
fn three_station_collision_at_repeater_symmetric() {
    // s1 ↔ r ↔ s2 (via repeater); third station s3 silent.
    // Each segment delay τ = 1µs, repeater re-emit δ_h = 100ns.
    // s1's signal reaches s2 at t = 2τ + δ_h = 2.1µs (and vice versa).
    // Both s1 and s2 are transmitting at that time → CollisionDetect.
    let tau = BitTime::from_micros(1);
    let delta_h = BitTime::from_nanos(100);
    let (world, s1, s2, s3) = hd_three_via_repeater(tau, delta_h);
    let mut engine = Engine::with_seed(world, 1);

    let f1 = engine
        .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    let f2 = engine
        .register_frame(s2, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, f1);
    engine.schedule_tx_attempt(BitTime::ZERO, s2, f2);
    engine.run_until_idle();

    // Closed-form: each station detects collision when foreign FrontArrive
    // hits it at t = 2τ + δ_h.
    let expected = tau + tau + delta_h;
    let s1_detect = engine
        .log()
        .iter()
        .find(|e| matches!(e.event, Event::CollisionDetect { node, .. } if node == s1))
        .expect("s1 should detect a collision");
    let s2_detect = engine
        .log()
        .iter()
        .find(|e| matches!(e.event, Event::CollisionDetect { node, .. } if node == s2))
        .expect("s2 should detect a collision");
    assert_eq!(s1_detect.key.time, expected);
    assert_eq!(s2_detect.key.time, expected);
    let _ = s3;
}

// ---------------------------------------------------------------------------
// 2. Carrier sense at a silent witness during a multi-station collision.
// ---------------------------------------------------------------------------

#[test]
fn carrier_sense_at_silent_witness_during_collision() {
    // Same 3-station HD-via-repeater. s1 and s2 collide. s3 transmits
    // nothing — it observes carrier from both signals. Per the half-open
    // occupancy convention, carrier_sense at s3 is true between the first
    // FrontArrive at s3 (at 2τ + δ_h) and the last BackArrive at s3
    // (at 2τ + δ_h + signal_duration).
    let tau = BitTime::from_micros(1);
    let delta_h = BitTime::from_nanos(100);
    let (world, s1, s2, s3) = hd_three_via_repeater(tau, delta_h);
    let mut engine = Engine::with_seed(world, 2);

    let f1 = engine
        .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    let f2 = engine
        .register_frame(s2, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, f1);
    engine.schedule_tx_attempt(BitTime::ZERO, s2, f2);
    engine.run_until_idle();

    let log = engine.log();

    let first_arrival = tau + tau + delta_h;

    // Before any signal reaches s3: no carrier.
    assert!(!observe::carrier_sense(log, s3, BitTime::ZERO));
    assert!(!observe::carrier_sense(
        log,
        s3,
        first_arrival - BitTime::from_nanos(1),
    ));

    // At first arrival: carrier present.
    assert!(observe::carrier_sense(log, s3, first_arrival));
    // Mid-occupancy: carrier present.
    assert!(observe::carrier_sense(
        log,
        s3,
        first_arrival + BitTime::from_micros(1),
    ));
    let _ = (s1, s2);
}

// ---------------------------------------------------------------------------
// 3. Five-station storm: every station's *first* collision detect fires at
//    the closed-form 2τ + δ_h time. (Per the engine's collision handler,
//    every foreign FrontArrive on a transmitting node schedules a fresh
//    CollisionDetect — so total counts are not bounded; Theorem 1 governs
//    only the *first* detection time.)
// ---------------------------------------------------------------------------

#[test]
fn five_station_storm_first_collision_time_is_closed_form() {
    let tau = BitTime::from_micros(1);
    let delta_h = BitTime::from_nanos(100);
    let mut b = TopologyBuilder::new();
    let stations: Vec<NodeId> = (0..5).map(|_| b.add_end_station(1)).collect();
    let r = b.add_repeater(5, delta_h);
    for (i, &s) in stations.iter().enumerate() {
        b.add_hd_segment(
            BitRate::ETHERNET_10M,
            tau,
            ep(s, 0),
            ep(r, u32::try_from(i).unwrap()),
        )
        .unwrap();
    }
    let world = b.build().unwrap();
    let mut engine = Engine::with_seed(world, 3);

    for &s in &stations {
        let f = engine
            .register_frame(s, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s, f);
    }
    engine.run_until_idle();

    let expected = tau + tau + delta_h;
    for &s in &stations {
        let t = observe::first_collision_detect_at(engine.log(), s)
            .unwrap_or_else(|| panic!("station {s:?} has no collision-detect time"));
        assert_eq!(t, expected, "station {s:?} first-collision time");
    }

    // Every station detects at least once; total count is unbounded due
    // to BEB retry cascades, so only assert "at least one per station".
    for &s in &stations {
        let detects = count_events(
            engine.log(),
            |e| matches!(e, Event::CollisionDetect { node, .. } if *node == s),
        );
        assert!(detects >= 1, "station {s:?} should detect ≥ 1 collision");
    }
}

// ---------------------------------------------------------------------------
// 4. HD chain pair-delay across multiple repeaters: closed-form total.
// ---------------------------------------------------------------------------

#[test]
fn hd_chain_pair_delay_across_three_repeaters() {
    // s1 — r1 — r2 — r3 — s2 with τ on each of 4 segments and δ_h on each
    // of 3 repeaters. By the BFS leave-cost rule (A3), source pays no
    // δ_h, but every repeater traversed adds one δ_h.
    // Total propagation delay s1 → s2: 4τ + 3δ_h.
    let tau = BitTime::from_micros(2);
    let delta_h = BitTime::from_nanos(150);
    let mut b = TopologyBuilder::new();
    let s1 = b.add_end_station(1);
    let r1 = b.add_repeater(2, delta_h);
    let r2 = b.add_repeater(2, delta_h);
    let r3 = b.add_repeater(2, delta_h);
    let s2 = b.add_end_station(1);
    b.add_hd_segment(BitRate::ETHERNET_10M, tau, ep(s1, 0), ep(r1, 0))
        .unwrap();
    b.add_hd_segment(BitRate::ETHERNET_10M, tau, ep(r1, 1), ep(r2, 0))
        .unwrap();
    b.add_hd_segment(BitRate::ETHERNET_10M, tau, ep(r2, 1), ep(r3, 0))
        .unwrap();
    b.add_hd_segment(BitRate::ETHERNET_10M, tau, ep(r3, 1), ep(s2, 0))
        .unwrap();
    let world = b.build().unwrap();
    let mut engine = Engine::with_seed(world, 4);

    let f = engine
        .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, f);
    engine.run_until_idle();

    // Expected total propagation delay.
    let expected = tau + tau + tau + tau + delta_h + delta_h + delta_h;

    let front_at_s2 = engine
        .log()
        .iter()
        .find(|e| matches!(e.event, Event::FrontArrive { node, .. } if node == s2))
        .expect("FrontArrive at s2 should be logged");
    assert_eq!(front_at_s2.key.time, expected);
}

// ---------------------------------------------------------------------------
// 5. Multi-station replay: same seed → same first-collision times at each
//    station. (Full byte-identity is covered by `determinism.rs` for
//    simpler scenarios; under simultaneous N-station collisions the
//    serial_id assignment is sensitive to HashMap iteration order in
//    `precompute_hd_pair_reachability`, so we assert observable identity
//    here rather than serial_id-level identity.)
// ---------------------------------------------------------------------------

#[test]
fn multi_station_storm_first_collision_times_are_seed_stable() {
    fn first_collisions(seed: u64) -> Vec<Option<BitTime>> {
        let tau = BitTime::from_micros(1);
        let delta_h = BitTime::from_nanos(100);
        let (world, s1, s2, s3) = hd_three_via_repeater(tau, delta_h);
        let nodes = [s1, s2, s3];
        let mut engine = Engine::with_seed(world, seed);
        for &s in &nodes {
            let f = engine
                .register_frame(s, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
                .unwrap();
            engine.schedule_tx_attempt(BitTime::ZERO, s, f);
        }
        engine.run_until_idle();
        nodes
            .iter()
            .map(|&n| observe::first_collision_detect_at(engine.log(), n))
            .collect()
    }
    assert_eq!(first_collisions(42), first_collisions(42));
}
