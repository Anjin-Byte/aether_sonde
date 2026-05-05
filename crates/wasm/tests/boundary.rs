//! Boundary tests — `wasm-bindgen-test`s that verify the WASM bindings
//! preserve the core's correctness contract under JS marshaling.
//!
//! Run with: `wasm-pack test crates/wasm --node`

#![allow(missing_docs)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]

use aether_sonde_wasm::*;
use wasm_bindgen::JsValue;
use wasm_bindgen_test::*;

// ---------------------------------------------------------------------------
// Round-trip topology + simulation
// ---------------------------------------------------------------------------

#[wasm_bindgen_test]
fn build_hd_pair_via_topology_builder() {
    let mut b = TopologyBuilder::new();
    let s1 = b.add_end_station(1);
    let s2 = b.add_end_station(1);
    // 10 Mbps, 5 µs delay = 5_000_000 ps.
    b.add_hd_segment(10_000_000, 5_000_000, s1, 0, s2, 0)
        .unwrap();
    let world = b.build().unwrap();
    assert_eq!(world.node_count(), 2);
    assert_eq!(world.segment_count(), 1);
    assert_eq!(world.collision_resource_count(), 1);
}

#[wasm_bindgen_test]
fn hd_1_run_produces_propagation_events() {
    // The classic HD-1 oracle from the native suite: a 512-bit frame at
    // 10 Mbps, τ = 5 µs, produces TxAttempt + TxStart + FrontArrive at
    // τ + TxEnd + BackArrive — exactly 5 events.
    let mut b = TopologyBuilder::new();
    let s1 = b.add_end_station(1);
    let s2 = b.add_end_station(1);
    b.add_hd_segment(10_000_000, 5_000_000, s1, 0, s2, 0)
        .unwrap();
    let world = b.build().unwrap();

    let mut engine = Engine::new(world, 1);
    let frame = engine.register_frame(s1, 512, false, 10_000_000).unwrap();
    engine.schedule_tx_attempt(0, s1, frame);
    engine.run_until_idle();

    let log = engine.log().unwrap();
    let entries_val = js_sys::Reflect::get(&log, &JsValue::from_str("entries")).unwrap();
    let entries = js_sys::Array::from(&entries_val);
    assert_eq!(entries.length(), 5);
}

// ---------------------------------------------------------------------------
// Apply each Edit variant from JS
// ---------------------------------------------------------------------------

#[wasm_bindgen_test]
fn apply_edit_add_end_station() {
    let world = TopologyBuilder::new().build().unwrap();
    let mut engine = Engine::new(world, 0);
    assert_eq!(engine.node_count(), 0);

    // Build the Edit JS object: { type: "AddEndStation", port_count: 1 }.
    let edit = js_sys::Object::new();
    js_sys::Reflect::set(
        &edit,
        &JsValue::from_str("type"),
        &JsValue::from_str("AddEndStation"),
    )
    .unwrap();
    js_sys::Reflect::set(
        &edit,
        &JsValue::from_str("port_count"),
        &JsValue::from(1u32),
    )
    .unwrap();

    engine.apply_edit(JsValue::from(edit)).unwrap();
    engine.run_until_idle();
    assert_eq!(engine.node_count(), 1);
}

#[wasm_bindgen_test]
fn apply_edit_unknown_node_throws_discriminated_error() {
    let world = TopologyBuilder::new().build().unwrap();
    let mut engine = Engine::new(world, 0);

    // Edit referencing a non-existent node:
    //   { type: "SetMacConfig", node: 99, config: ... }
    let cfg = js_sys::Object::new();
    let backoff = js_sys::Object::new();
    js_sys::Reflect::set(
        &backoff,
        &JsValue::from_str("attempt_limit"),
        &JsValue::from(16u32),
    )
    .unwrap();
    js_sys::Reflect::set(
        &backoff,
        &JsValue::from_str("backoff_limit"),
        &JsValue::from(10u32),
    )
    .unwrap();
    js_sys::Reflect::set(&cfg, &JsValue::from_str("backoff"), &backoff).unwrap();
    let jam = js_sys::Object::new();
    js_sys::Reflect::set(&jam, &JsValue::from_str("bits"), &JsValue::from(32u64)).unwrap();
    js_sys::Reflect::set(&cfg, &JsValue::from_str("jam"), &jam).unwrap();
    let ifg = js_sys::Object::new();
    js_sys::Reflect::set(&ifg, &JsValue::from_str("bits"), &JsValue::from(96u64)).unwrap();
    js_sys::Reflect::set(&cfg, &JsValue::from_str("ifg"), &ifg).unwrap();

    let edit = js_sys::Object::new();
    js_sys::Reflect::set(
        &edit,
        &JsValue::from_str("type"),
        &JsValue::from_str("SetMacConfig"),
    )
    .unwrap();
    js_sys::Reflect::set(&edit, &JsValue::from_str("node"), &JsValue::from(99u32)).unwrap();
    js_sys::Reflect::set(&edit, &JsValue::from_str("config"), &cfg).unwrap();

    let result = engine.apply_edit(JsValue::from(edit));
    assert!(result.is_err());
    let err = result.unwrap_err();
    let kind = js_sys::Reflect::get(&err, &JsValue::from_str("kind"))
        .unwrap()
        .as_string()
        .unwrap();
    assert_eq!(kind, "UnknownNode");
}

// ---------------------------------------------------------------------------
// Determinism — same inputs → same log
// ---------------------------------------------------------------------------

#[wasm_bindgen_test]
fn same_seed_produces_byte_identical_log() {
    let run = || -> JsValue {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        b.add_hd_segment(10_000_000, 5_000_000, s1, 0, s2, 0)
            .unwrap();
        let world = b.build().unwrap();
        let mut engine = Engine::new(world, 42);
        let frame = engine.register_frame(s1, 512, false, 10_000_000).unwrap();
        engine.schedule_tx_attempt(0, s1, frame);
        engine.run_until_idle();
        engine.log().unwrap()
    };
    let a = run();
    let b = run();
    // `JSON.stringify` can't serialize BigInt without a replacer; pass a
    // function that converts BigInt to string. Same-seed runs produce the
    // same string.
    let replacer = js_sys::Function::new_with_args(
        "_key, value",
        "return typeof value === 'bigint' ? value.toString() : value;",
    );
    let json_a = js_sys::JSON::stringify_with_replacer(&a, &replacer.into())
        .unwrap()
        .as_string()
        .unwrap();
    let replacer = js_sys::Function::new_with_args(
        "_key, value",
        "return typeof value === 'bigint' ? value.toString() : value;",
    );
    let json_b = js_sys::JSON::stringify_with_replacer(&b, &replacer.into())
        .unwrap()
        .as_string()
        .unwrap();
    assert_eq!(json_a, json_b);
}

// ---------------------------------------------------------------------------
// Observables agree with native semantics
// ---------------------------------------------------------------------------

#[wasm_bindgen_test]
fn carrier_sense_observable_matches_log() {
    let mut b = TopologyBuilder::new();
    let s1 = b.add_end_station(1);
    let s2 = b.add_end_station(1);
    b.add_hd_segment(10_000_000, 5_000_000, s1, 0, s2, 0)
        .unwrap();
    let world = b.build().unwrap();
    let mut engine = Engine::new(world, 0);
    let frame = engine.register_frame(s1, 512, false, 10_000_000).unwrap();
    engine.schedule_tx_attempt(0, s1, frame);
    engine.run_until_idle();

    let log = engine.log().unwrap();

    // Before TxStart: no carrier at receiver.
    assert!(!carrier_sense(log.clone(), s2, 0).unwrap());
    // After FrontArrive at τ=5µs but before BackArrive at 56.2µs: carrier present.
    assert!(carrier_sense(log.clone(), s2, 10_000_000).unwrap());
    // After BackArrive: no carrier.
    assert!(!carrier_sense(log, s2, 60_000_000).unwrap());
}
