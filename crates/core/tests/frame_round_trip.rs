//! Integration tests for the `Frame` round-trip through the engine.
//!
//! Two paths must produce identical timing:
//!   - `Engine::register_frame(node, bits, kind, rate)` (legacy)
//!   - `Engine::register_frame_with(node, Frame::opaque(bits), kind, rate)` (new)
//!
//! Frames carrying explicit MAC addresses survive registration and can
//! be retrieved via `Engine::registered_frame(id)`.

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
use aether_sonde::frame::{EtherType, Frame, FramePayload, MacAddress, VlanTag};
use aether_sonde::signal::SignalKind;
use aether_sonde::time::{BitRate, BitTime, Bits};

use common::hd_pair;

// ---------------------------------------------------------------------------
// Backwards compatibility: legacy and explicit-frame paths produce
// identical signal timing.
// ---------------------------------------------------------------------------

#[test]
fn legacy_and_explicit_paths_produce_identical_logs() {
    let tau = BitTime::from_micros(5);

    let log_legacy = {
        let (world, s1, _) = hd_pair(tau);
        let mut engine = Engine::with_seed(world, 1);
        let frame = engine
            .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();
        engine
            .log()
            .iter()
            .map(|e| (e.key.time, e.key.phase))
            .collect::<Vec<_>>()
    };

    let log_explicit = {
        let (world, s1, _) = hd_pair(tau);
        let mut engine = Engine::with_seed(world, 1);
        let frame = engine
            .register_frame_with(
                s1,
                Frame::opaque(Bits::new(512)),
                SignalKind::Frame,
                BitRate::ETHERNET_10M,
            )
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();
        engine
            .log()
            .iter()
            .map(|e| (e.key.time, e.key.phase))
            .collect::<Vec<_>>()
    };

    assert_eq!(log_legacy, log_explicit);
}

// ---------------------------------------------------------------------------
// Explicit MAC addresses round-trip through registration.
// ---------------------------------------------------------------------------

#[test]
fn ethernet_frame_round_trips_through_registration() {
    let (world, s1, _) = hd_pair(BitTime::from_micros(5));
    let mut engine = Engine::with_seed(world, 0);
    let dst = MacAddress::new([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
    let src = MacAddress::new([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
    let original = Frame::ethernet(dst, src, EtherType::IPV4, Bits::new(1500 * 8));

    let frame_id = engine
        .register_frame_with(s1, original, SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    let registered = engine.registered_frame(frame_id).unwrap();
    assert_eq!(*registered, original);
    assert_eq!(registered.destination, dst);
    assert_eq!(registered.source, src);
    assert_eq!(registered.ethertype, EtherType::IPV4);
}

// ---------------------------------------------------------------------------
// VLAN tag round-trips through registration.
// ---------------------------------------------------------------------------

#[test]
fn vlan_tag_round_trips_through_registration() {
    let (world, s1, _) = hd_pair(BitTime::from_micros(5));
    let mut engine = Engine::with_seed(world, 0);
    let frame = Frame::ethernet(
        MacAddress::ZERO,
        MacAddress::ZERO,
        EtherType::IPV4,
        Bits::new(64 * 8),
    )
    .with_vlan(VlanTag {
        priority: 5,
        drop_eligible: false,
        vid: 100,
    });
    let id = engine
        .register_frame_with(s1, frame, SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    let stored = engine.registered_frame(id).unwrap();
    let tag = stored.vlan.expect("VLAN tag preserved");
    assert_eq!(tag.priority, 5);
    assert_eq!(tag.vid, 100);
    assert!(!tag.drop_eligible);
}

// ---------------------------------------------------------------------------
// Wire length drives signal duration deterministically.
// ---------------------------------------------------------------------------

#[test]
fn wire_length_determines_signal_duration() {
    let tau = BitTime::from_micros(5);
    let (world, s1, _) = hd_pair(tau);
    let mut engine = Engine::with_seed(world, 0);
    let frame = Frame::ethernet(
        MacAddress::ZERO,
        MacAddress::ZERO,
        EtherType::IPV4,
        Bits::new(512),
    );
    let id = engine
        .register_frame_with(s1, frame, SignalKind::Frame, BitRate::ETHERNET_10M)
        .unwrap();
    engine.schedule_tx_attempt(BitTime::ZERO, s1, id);
    engine.run_until_idle();

    // 512 bits at 10 Mbps = 51.2 µs. TxEnd fires at exactly that time.
    let tx_end = engine
        .log()
        .iter()
        .find(|e| matches!(e.event, Event::TxEnd { .. }))
        .expect("TxEnd should be logged");
    assert_eq!(tx_end.key.time, BitTime::from_nanos(51_200));
}

// ---------------------------------------------------------------------------
// Zero-bit frames are rejected via either path.
// ---------------------------------------------------------------------------

#[test]
fn zero_bit_frame_rejected_via_explicit_path() {
    let (world, s1, _) = hd_pair(BitTime::from_micros(5));
    let mut engine = Engine::with_seed(world, 0);
    let result = engine.register_frame_with(
        s1,
        Frame::opaque(Bits::ZERO),
        SignalKind::Frame,
        BitRate::ETHERNET_10M,
    );
    assert!(result.is_err());
}

// ---------------------------------------------------------------------------
// `registered_frame` returns None for unknown FrameId.
// ---------------------------------------------------------------------------

#[test]
fn registered_frame_returns_none_for_unknown_id() {
    let (world, _, _) = hd_pair(BitTime::from_micros(5));
    let engine = Engine::with_seed(world, 0);
    let unknown = aether_sonde::event::FrameId::new(999);
    assert!(engine.registered_frame(unknown).is_none());
}

// ---------------------------------------------------------------------------
// FramePayload exhaustiveness: opaque is the only round-4 variant.
// ---------------------------------------------------------------------------

#[test]
fn frame_payload_exhaustive_round_4() {
    // This match exists to fail-to-compile when a new FramePayload
    // variant is added — round 4 has only Opaque.
    let f = Frame::opaque(Bits::new(64));
    let bits = match f.payload {
        FramePayload::Opaque { bits } => bits,
    };
    assert_eq!(bits, Bits::new(64));
}
