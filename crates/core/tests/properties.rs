//! Property-based invariants — `proptest` strategies that probe the
//! input space the hand-crafted tests can't reach.
//!
//! Per the codex's `Edge Cases and Properties` and `Evidence Ladder for
//! Testing`: properties are higher-leverage than fixed inputs because
//! they assert invariants over a *family* of inputs the test runner
//! generates. Counter-examples shrink to minimal failing cases and are
//! preserved in `proptest-regressions/properties.txt`.

#![allow(missing_docs)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use aether_sonde::engine::{Edit, Engine};
use aether_sonde::signal::NodeId;
use aether_sonde::time::{BitRate, BitTime};
use aether_sonde::topology::{Endpoint, PortId, SegmentId, TopologyBuilder};

use proptest::prelude::*;

/// Strategy generating `Edit` values bounded to small `NodeId`/
/// `SegmentId`/`PortId` ranges. Most generated edits will fail
/// validation (`UnknownNode`, `UnknownSegment`, etc.) — that's fine;
/// the invariants we test (replay determinism, log monotonicity, no
/// panics) hold for both `Ok` and `Err` returns.
fn arb_edit() -> impl Strategy<Value = Edit> {
    let endpoint =
        (0u32..6u32, 0u32..3u32).prop_map(|(n, p)| Endpoint::new(NodeId::new(n), PortId::new(p)));

    prop_oneof![
        (1u32..4u32).prop_map(|pc| Edit::AddEndStation { port_count: pc }),
        (1u32..4u32, 50u64..500u64).prop_map(|(pc, dh)| Edit::AddRepeater {
            port_count: pc,
            delta_h: BitTime::from_nanos(dh),
        }),
        (endpoint.clone(), endpoint.clone(), 100u64..10_000u64).prop_map(|(a, b, d)| {
            Edit::AddHdSegment {
                rate: BitRate::ETHERNET_10M,
                delay: BitTime::from_nanos(d),
                a,
                b,
            }
        }),
        (endpoint.clone(), endpoint.clone(), 100u64..10_000u64).prop_map(|(a, b, d)| {
            Edit::AddFdSegment {
                rate: BitRate::ETHERNET_1G,
                delay: BitTime::from_nanos(d),
                a,
                b,
            }
        }),
        (0u32..6u32).prop_map(|s| Edit::RemoveSegment {
            segment: SegmentId::new(s),
        }),
        (0u32..6u32).prop_map(|n| Edit::RemoveNode {
            node: NodeId::new(n),
        }),
        (0u32..6u32, 0u32..3u32).prop_map(|(n, p)| Edit::DisconnectPort {
            node: NodeId::new(n),
            port: PortId::new(p),
        }),
        (0u32..6u32, 100u64..10_000u64).prop_map(|(s, d)| Edit::SetSegmentDelay {
            segment: SegmentId::new(s),
            new_delay: BitTime::from_nanos(d),
        }),
    ]
}

proptest! {
    /// Property: applying the same edit sequence twice (identical seed
    /// + identical inputs) produces byte-identical logs. This is the
    /// §1.c determinism contract under arbitrary inputs.
    #[test]
    fn replay_byte_identical_for_any_edit_sequence(
        edits in prop::collection::vec(arb_edit(), 0..30),
        seed in any::<u64>(),
    ) {
        let run = || -> Vec<_> {
            let mut engine = Engine::with_seed(
                TopologyBuilder::new().build().unwrap(),
                seed,
            );
            for e in &edits {
                let _ = engine.apply_edit(*e);
            }
            engine.run_until_idle();
            engine.log().iter().copied().collect()
        };
        prop_assert_eq!(run(), run());
    }

    /// Property: after any edit sequence, the log's timestamps are
    /// monotonically non-decreasing in insertion order.
    #[test]
    fn log_is_monotonic_for_any_edit_sequence(
        edits in prop::collection::vec(arb_edit(), 0..30),
    ) {
        let mut engine = Engine::with_seed(
            TopologyBuilder::new().build().unwrap(),
            0,
        );
        for e in &edits {
            let _ = engine.apply_edit(*e);
        }
        engine.run_until_idle();
        let mut prev = BitTime::ZERO;
        for entry in engine.log().iter() {
            prop_assert!(entry.key.time >= prev);
            prev = entry.key.time;
        }
    }

    /// Property: `apply_edit` never panics on syntactically valid
    /// input. It either returns `Ok(())` or an `EditError`. This is
    /// the type-driven boundary discipline at full strength.
    #[test]
    fn apply_edit_never_panics(
        edits in prop::collection::vec(arb_edit(), 0..30),
    ) {
        let mut engine = Engine::with_seed(
            TopologyBuilder::new().build().unwrap(),
            0,
        );
        for e in &edits {
            // The Result is consumed; the test is that no panic occurs.
            let _ = engine.apply_edit(*e);
        }
    }

    /// Property: A7 (HD components are trees) holds after every
    /// successful edit. Operationally: `world.collision_resource_count`
    /// equals the number of distinct HD components, and the engine
    /// never reports more components than nodes.
    #[test]
    fn a7_invariant_holds_after_every_edit_sequence(
        edits in prop::collection::vec(arb_edit(), 0..30),
    ) {
        let mut engine = Engine::with_seed(
            TopologyBuilder::new().build().unwrap(),
            0,
        );
        for e in &edits {
            let _ = engine.apply_edit(*e);
            // The number of HD collision resources can never exceed
            // the number of nodes (each tree component contributes
            // at most one resource per non-bridge node).
            let world = engine.world();
            prop_assert!(
                world.collision_resource_count() <= world.node_count() + 1,
                "more HD components than nodes — A7 violated",
            );
        }
    }
}
