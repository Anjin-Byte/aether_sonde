//! Closed-form bridge correctness oracles.
//!
//! Every test asserts exact timestamps derived from the eligibility
//! formula `t_elig = t_first_bit_in + decode_threshold / ingress_rate +
//! processing_delay`, FIFO ordering on egress queues, or the busy-egress
//! backpressure rule. No "looks right" assertions.

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
use aether_sonde::signal::{NodeId, SignalKind};
use aether_sonde::time::{BitRate, BitTime, Bits};
use aether_sonde::topology::TopologyBuilder;

use common::{count_events, ep};

// ---------------------------------------------------------------------------
// Helper: build a 1×2 bridge topology with custom segment rates.
// ---------------------------------------------------------------------------

struct BridgeScenario {
    s1: NodeId,
    s2: NodeId,
    bridge: NodeId,
    engine: Engine,
}

fn build_bridge(
    ingress_rate: BitRate,
    egress_rate: BitRate,
    delay_a: BitTime,
    delay_b: BitTime,
    decode_threshold: Bits,
    processing_delay: BitTime,
    seed: u64,
) -> BridgeScenario {
    let mut b = TopologyBuilder::new();
    let s1 = b.add_end_station(1);
    let s2 = b.add_end_station(1);
    let bridge = b.add_bridge(2, decode_threshold, processing_delay);
    b.add_hd_segment(ingress_rate, delay_a, ep(s1, 0), ep(bridge, 0))
        .unwrap();
    b.add_hd_segment(egress_rate, delay_b, ep(s2, 0), ep(bridge, 1))
        .unwrap();
    let world = b.build().unwrap();
    let engine = Engine::with_seed(world, seed);
    BridgeScenario {
        s1,
        s2,
        bridge,
        engine,
    }
}

// ---------------------------------------------------------------------------
// 1. Cut-through: small decode_threshold → eligibility before frame ends.
// ---------------------------------------------------------------------------

#[test]
fn cut_through_eligibility_below_full_frame() {
    // 1500-bit frame at 100 Mbps; bridge decode_threshold = 64 bits.
    // Frame duration on the wire: 1500/100Mbps = 15 µs.
    // Ingress segment delay: 1 µs.
    // First bit arrives at bridge at t = 1 µs.
    // Eligibility: t = 1µs + 64/100Mbps + 0 = 1µs + 640ns = 1.64µs.
    let delay = BitTime::from_micros(1);
    let mut scn = build_bridge(
        BitRate::ETHERNET_100M,
        BitRate::ETHERNET_100M,
        delay,
        delay,
        Bits::new(64),
        BitTime::ZERO,
        1,
    );

    let frame = scn
        .engine
        .register_frame(
            scn.s1,
            Bits::new(1500),
            SignalKind::Frame,
            BitRate::ETHERNET_100M,
        )
        .unwrap();
    scn.engine.schedule_tx_attempt(BitTime::ZERO, scn.s1, frame);
    scn.engine.run_until_idle();

    let eligible = scn
        .engine
        .log()
        .iter()
        .find(|e| matches!(e.event, Event::FrameEligible { .. }))
        .expect("FrameEligible should be logged");
    let expected = BitTime::from_micros(1) + BitTime::from_nanos(640);
    assert_eq!(eligible.key.time, expected);

    // Sanity: frame still in flight at eligibility time (cut-through invariant).
    let tx_end = scn
        .engine
        .log()
        .iter()
        .find(|e| matches!(e.event, Event::TxEnd { .. }))
        .unwrap();
    assert!(tx_end.key.time > expected);
    let _ = scn.s2; // silence unused warning
    let _ = scn.bridge;
}

// ---------------------------------------------------------------------------
// 2. Store-and-forward: decode_threshold = full frame → eligibility at end.
// ---------------------------------------------------------------------------

#[test]
fn store_and_forward_eligibility_at_end_of_frame() {
    // 1500-bit frame at 100 Mbps; bridge decode_threshold = 1500 bits.
    // First bit at bridge at t = 1µs; full-frame decoded at t = 1µs + 15µs = 16µs.
    let delay = BitTime::from_micros(1);
    let mut scn = build_bridge(
        BitRate::ETHERNET_100M,
        BitRate::ETHERNET_100M,
        delay,
        delay,
        Bits::new(1500),
        BitTime::ZERO,
        2,
    );

    let frame = scn
        .engine
        .register_frame(
            scn.s1,
            Bits::new(1500),
            SignalKind::Frame,
            BitRate::ETHERNET_100M,
        )
        .unwrap();
    scn.engine.schedule_tx_attempt(BitTime::ZERO, scn.s1, frame);
    scn.engine.run_until_idle();

    let eligible = scn
        .engine
        .log()
        .iter()
        .find(|e| matches!(e.event, Event::FrameEligible { .. }))
        .expect("FrameEligible should be logged");
    let expected = BitTime::from_micros(1) + BitTime::from_micros(15);
    assert_eq!(eligible.key.time, expected);
    let _ = (scn.s2, scn.bridge);
}

// ---------------------------------------------------------------------------
// 3. Processing delay π_b is added to eligibility timing.
// ---------------------------------------------------------------------------

#[test]
fn processing_delay_pi_b_added_to_eligibility() {
    // Cut-through with 64-bit threshold + 500ns processing delay.
    // t_elig = 1µs (segment delay) + 640ns (64/100Mbps) + 500ns (π_b) = 2.14µs.
    let delay = BitTime::from_micros(1);
    let pi_b = BitTime::from_nanos(500);
    let mut scn = build_bridge(
        BitRate::ETHERNET_100M,
        BitRate::ETHERNET_100M,
        delay,
        delay,
        Bits::new(64),
        pi_b,
        3,
    );

    let frame = scn
        .engine
        .register_frame(
            scn.s1,
            Bits::new(1500),
            SignalKind::Frame,
            BitRate::ETHERNET_100M,
        )
        .unwrap();
    scn.engine.schedule_tx_attempt(BitTime::ZERO, scn.s1, frame);
    scn.engine.run_until_idle();

    let eligible = scn
        .engine
        .log()
        .iter()
        .find(|e| matches!(e.event, Event::FrameEligible { .. }))
        .expect("FrameEligible should be logged");
    let expected = delay + BitTime::from_nanos(640) + pi_b;
    assert_eq!(eligible.key.time, expected);
    let _ = (scn.s2, scn.bridge);
}

// ---------------------------------------------------------------------------
// 4. FIFO ordering: two consecutive transmissions egress in arrival order.
// ---------------------------------------------------------------------------

#[test]
fn bridge_relay_preserves_event_ordering() {
    // s1 transmits two frames sequentially with adequate IFG. They arrive
    // at the bridge in order, become eligible in order, and Enqueue/Dequeue
    // events log in arrival order on the egress serializer.
    let delay = BitTime::from_micros(1);
    let mut scn = build_bridge(
        BitRate::ETHERNET_100M,
        BitRate::ETHERNET_100M,
        delay,
        delay,
        Bits::new(64),
        BitTime::ZERO,
        4,
    );

    // Two short frames, well-separated in time so they don't overlap on the wire.
    let frame_a = scn
        .engine
        .register_frame(
            scn.s1,
            Bits::new(64),
            SignalKind::Frame,
            BitRate::ETHERNET_100M,
        )
        .unwrap();
    let frame_b = scn
        .engine
        .register_frame(
            scn.s1,
            Bits::new(64),
            SignalKind::Frame,
            BitRate::ETHERNET_100M,
        )
        .unwrap();
    scn.engine
        .schedule_tx_attempt(BitTime::ZERO, scn.s1, frame_a);
    // Schedule second well after the first completes (frame_a duration is 640ns at 100M).
    scn.engine
        .schedule_tx_attempt(BitTime::from_micros(50), scn.s1, frame_b);
    scn.engine.run_until_idle();

    // Collect Enqueue events in log order; assert frame_a's enqueue precedes frame_b's.
    let enqueues: Vec<u32> = scn
        .engine
        .log()
        .iter()
        .filter_map(|e| match e.event {
            Event::Enqueue { frame, .. } => Some(frame.as_u32()),
            _ => None,
        })
        .collect();
    assert_eq!(enqueues.len(), 2, "two Enqueue events expected");
    // Frames are registered in order: frame_a first → relay frame_a's id first.
    // Bridge handler registers a relay frame at FrontArrive time; relay frames
    // get sequential IDs after the source frames. The first Enqueue's frame id
    // should be less than the second's (both were registered in arrival order).
    assert!(
        enqueues[0] < enqueues[1],
        "Enqueue arrival order violated: {enqueues:?}",
    );

    // Same FIFO check for Dequeue.
    let dequeues: Vec<u32> = scn
        .engine
        .log()
        .iter()
        .filter_map(|e| match e.event {
            Event::Dequeue { frame, .. } => Some(frame.as_u32()),
            _ => None,
        })
        .collect();
    assert_eq!(dequeues.len(), 2);
    assert_eq!(
        dequeues, enqueues,
        "Dequeue order should match Enqueue order",
    );
    let _ = scn.bridge;
}

// ---------------------------------------------------------------------------
// 5. Theorem 5: a frame relayed through a bridge produces zero collisions.
// ---------------------------------------------------------------------------

#[test]
fn theorem_5_holds_with_custom_decode_threshold() {
    // Re-validate the structural Theorem-5 invariant under a non-default
    // decode_threshold and processing_delay. Bridges separate collision
    // domains: s1's signal arrives at the bridge, gets relayed, but never
    // creates collisions on either segment.
    let delay = BitTime::from_micros(1);
    let mut scn = build_bridge(
        BitRate::ETHERNET_100M,
        BitRate::ETHERNET_100M,
        delay,
        delay,
        Bits::new(128),
        BitTime::from_nanos(750),
        5,
    );

    let frame = scn
        .engine
        .register_frame(
            scn.s1,
            Bits::new(512),
            SignalKind::Frame,
            BitRate::ETHERNET_100M,
        )
        .unwrap();
    scn.engine.schedule_tx_attempt(BitTime::ZERO, scn.s1, frame);
    scn.engine.run_until_idle();

    let collisions = count_events(scn.engine.log(), |e| {
        matches!(e, Event::CollisionDetect { .. })
    });
    assert_eq!(
        collisions, 0,
        "bridge relay must produce zero CollisionDetect events",
    );
    // s2 received the relayed frame.
    let s2_arrivals = count_events(
        scn.engine.log(),
        |e| matches!(e, Event::FrontArrive { node, .. } if *node == scn.s2),
    );
    assert!(s2_arrivals >= 1, "s2 should receive the relayed frame");
    let _ = scn.bridge;
}

// ---------------------------------------------------------------------------
// 6. Closed-form check: eligibility scales linearly with decode_threshold.
// ---------------------------------------------------------------------------

#[test]
fn eligibility_scales_linearly_with_decode_threshold() {
    // Three runs at the same rate but different decode_thresholds verify
    // the formula's linearity in η_b: doubling η_b doubles the (η_b/R_in)
    // contribution to t_elig.
    fn elig_for(threshold_bits: u64) -> BitTime {
        let delay = BitTime::from_micros(1);
        let mut scn = build_bridge(
            BitRate::ETHERNET_100M,
            BitRate::ETHERNET_100M,
            delay,
            delay,
            Bits::new(threshold_bits),
            BitTime::ZERO,
            6,
        );
        let frame = scn
            .engine
            .register_frame(
                scn.s1,
                Bits::new(1500),
                SignalKind::Frame,
                BitRate::ETHERNET_100M,
            )
            .unwrap();
        scn.engine.schedule_tx_attempt(BitTime::ZERO, scn.s1, frame);
        scn.engine.run_until_idle();
        scn.engine
            .log()
            .iter()
            .find_map(|e| match e.event {
                Event::FrameEligible { .. } => Some(e.key.time),
                _ => None,
            })
            .expect("FrameEligible should be logged")
    }
    let base = BitTime::from_micros(1); // segment delay
    let t_64 = elig_for(64);
    let t_128 = elig_for(128);
    let t_256 = elig_for(256);

    // 64 bits at 100 Mbps = 640 ns; 128 = 1280 ns; 256 = 2560 ns.
    assert_eq!(t_64, base + BitTime::from_nanos(640));
    assert_eq!(t_128, base + BitTime::from_nanos(1_280));
    assert_eq!(t_256, base + BitTime::from_nanos(2_560));
    // Exact linearity: doubling threshold doubles the offset above base.
    let off_64 = t_64.as_u64() - base.as_u64();
    let off_128 = t_128.as_u64() - base.as_u64();
    let off_256 = t_256.as_u64() - base.as_u64();
    assert_eq!(off_128, off_64 * 2);
    assert_eq!(off_256, off_64 * 4);
}
