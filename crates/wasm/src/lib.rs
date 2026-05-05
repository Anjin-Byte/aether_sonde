//! WebAssembly bindings for the Aether Sonde simulation core.
//!
//! This crate is a **thin delegation layer**: every public function
//! forwards to `aether_sonde`. Simulation logic lives in the core; this
//! crate exists only to make that logic callable from JavaScript /
//! TypeScript.
//!
//! # Boundary shape
//!
//! - **Opaque handles**: [`TopologyBuilder`], [`World`], and [`Engine`]
//!   are JS classes whose Rust state lives in linear memory. TS holds a
//!   reference and calls methods on it. Per the codex's
//!   *Boundary Crossing Cost* note, this minimizes per-call marshaling.
//! - **Control-plane via serde**: `Edit` and `Event` and the four error
//!   enums cross as serde-wasm-bindgen objects. Errors are thrown as
//!   discriminated-union JS values with a `kind` field (or `type` for
//!   `Edit` / `Event`); TS narrows on the discriminator.
//! - **Coarse-grained calls**: each method does substantive work
//!   (build a topology, run to a horizon, retrieve a log). Avoid
//!   per-field property access on the JS side.

#![allow(missing_docs)]

use aether_sonde::engine as core_engine;
use aether_sonde::event::Log as CoreLog;
use aether_sonde::observe;
use aether_sonde::signal::{NodeId, SignalKind};
use aether_sonde::time::{BitRate, BitTime, Bits};
use aether_sonde::topology as core_topology;

use serde::{Deserialize, Serialize};
use wasm_bindgen::prelude::*;

// ---------------------------------------------------------------------------
// Module init: install the panic hook for nicer JS-side error reports.
// ---------------------------------------------------------------------------

/// One-time initializer that routes Rust panics to the JS console.
/// Idempotent; safe to call repeatedly.
#[wasm_bindgen(start)]
pub fn init() {
    // No external panic_hook crate dep yet; default behavior is fine.
    // When we adopt `console_error_panic_hook` (v1.1), this is the place.
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Serializer configured to emit `u64`/`i64`/`u128`/`i128` as JavaScript
/// `BigInt` (matching the TS types in `@aether-sonde/sim`). Without this,
/// serde-wasm-bindgen emits regular JS `number`, which loses precision
/// for values beyond 2^53 and breaks discriminated-union types that
/// declare these fields as `bigint`.
fn serializer() -> serde_wasm_bindgen::Serializer {
    serde_wasm_bindgen::Serializer::new().serialize_large_number_types_as_bigints(true)
}

fn to_js<T: Serialize>(value: &T) -> Result<JsValue, JsValue> {
    value.serialize(&serializer()).map_err(JsValue::from)
}

fn from_js<T: for<'de> Deserialize<'de>>(value: JsValue) -> Result<T, JsValue> {
    serde_wasm_bindgen::from_value(value).map_err(JsValue::from)
}

fn err_to_js<E: Serialize>(err: &E) -> JsValue {
    err.serialize(&serializer()).unwrap_or(JsValue::UNDEFINED)
}

// ---------------------------------------------------------------------------
// TopologyBuilder
// ---------------------------------------------------------------------------

/// Mutable builder for a [`World`]. Mirrors
/// `aether_sonde::topology::TopologyBuilder` 1:1.
#[wasm_bindgen]
pub struct TopologyBuilder(core_topology::TopologyBuilder);

#[wasm_bindgen]
impl TopologyBuilder {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self(core_topology::TopologyBuilder::new())
    }

    /// Append an end-station node. Returns the new node's numeric ID.
    #[wasm_bindgen(js_name = addEndStation)]
    pub fn add_end_station(&mut self, port_count: u32) -> u32 {
        self.0.add_end_station(port_count).as_u32()
    }

    /// Append a repeater (hub) node with re-emit delay `delta_h_ps` in
    /// picoseconds.
    #[wasm_bindgen(js_name = addRepeater)]
    pub fn add_repeater(&mut self, port_count: u32, delta_h_ps: u64) -> u32 {
        self.0
            .add_repeater(port_count, BitTime::new(delta_h_ps))
            .as_u32()
    }

    /// Append a bridge node.
    #[wasm_bindgen(js_name = addBridge)]
    pub fn add_bridge(
        &mut self,
        port_count: u32,
        decode_threshold_bits: u64,
        processing_delay_ps: u64,
    ) -> u32 {
        self.0
            .add_bridge(
                port_count,
                Bits::new(decode_threshold_bits),
                BitTime::new(processing_delay_ps),
            )
            .as_u32()
    }

    /// Append a learning-switch node. `mac_table_capacity == 0` is
    /// unbounded; `aging_threshold_ps == 0` disables aging.
    #[wasm_bindgen(js_name = addSwitch)]
    pub fn add_switch(
        &mut self,
        port_count: u32,
        decode_threshold_bits: u64,
        processing_delay_ps: u64,
        mac_table_capacity: u32,
        aging_threshold_ps: u64,
    ) -> u32 {
        self.0
            .add_switch(
                port_count,
                core_topology::SwitchData {
                    decode_threshold: Bits::new(decode_threshold_bits),
                    processing_delay: BitTime::new(processing_delay_ps),
                    mac_table_capacity,
                    aging_threshold: BitTime::new(aging_threshold_ps),
                },
            )
            .as_u32()
    }

    /// Append an HD shared-medium segment between
    /// `(a_node, a_port)` and `(b_node, b_port)`. Throws a `BuildError`
    /// JS object on failure.
    #[wasm_bindgen(js_name = addHdSegment)]
    pub fn add_hd_segment(
        &mut self,
        rate_bps: u64,
        delay_ps: u64,
        a_node: u32,
        a_port: u32,
        b_node: u32,
        b_port: u32,
    ) -> Result<u32, JsValue> {
        let rate = BitRate::from_bps(rate_bps).ok_or_else(|| JsValue::from_str("zero bit rate"))?;
        let a =
            core_topology::Endpoint::new(NodeId::new(a_node), core_topology::PortId::new(a_port));
        let b =
            core_topology::Endpoint::new(NodeId::new(b_node), core_topology::PortId::new(b_port));
        self.0
            .add_hd_segment(rate, BitTime::new(delay_ps), a, b)
            .map(core_topology::SegmentId::as_u32)
            .map_err(|e| err_to_js(&e))
    }

    /// Append an FD point-to-point segment.
    #[wasm_bindgen(js_name = addFdSegment)]
    pub fn add_fd_segment(
        &mut self,
        rate_bps: u64,
        delay_ps: u64,
        a_node: u32,
        a_port: u32,
        b_node: u32,
        b_port: u32,
    ) -> Result<u32, JsValue> {
        let rate = BitRate::from_bps(rate_bps).ok_or_else(|| JsValue::from_str("zero bit rate"))?;
        let a =
            core_topology::Endpoint::new(NodeId::new(a_node), core_topology::PortId::new(a_port));
        let b =
            core_topology::Endpoint::new(NodeId::new(b_node), core_topology::PortId::new(b_port));
        self.0
            .add_fd_segment(rate, BitTime::new(delay_ps), a, b)
            .map(core_topology::SegmentId::as_u32)
            .map_err(|e| err_to_js(&e))
    }

    /// Validate and finalize the topology. Throws a `BuildError` on
    /// validation failure.
    pub fn build(self) -> Result<World, JsValue> {
        self.0.build().map(World).map_err(|e| err_to_js(&e))
    }
}

impl Default for TopologyBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// World
// ---------------------------------------------------------------------------

/// A validated, mutable topology. Returned by
/// [`TopologyBuilder::build`]. Most methods are query-only; topology
/// mutation goes through [`Engine::apply_edit`].
#[wasm_bindgen]
pub struct World(core_topology::World);

#[wasm_bindgen]
impl World {
    /// Number of live nodes.
    #[wasm_bindgen(js_name = nodeCount)]
    pub fn node_count(&self) -> usize {
        self.0.node_count()
    }

    /// Number of live segments.
    #[wasm_bindgen(js_name = segmentCount)]
    pub fn segment_count(&self) -> usize {
        self.0.segment_count()
    }

    /// Number of HD-connected (collision) components.
    #[wasm_bindgen(js_name = collisionResourceCount)]
    pub fn collision_resource_count(&self) -> usize {
        self.0.collision_resource_count()
    }

    /// Total serializer count.
    #[wasm_bindgen(js_name = serializerCount)]
    pub fn serializer_count(&self) -> usize {
        self.0.serializer_count()
    }
}

// ---------------------------------------------------------------------------
// Engine
// ---------------------------------------------------------------------------

/// The discrete-event scheduler. Owns a [`World`] (passed in at
/// construction) and a deterministic RNG.
#[wasm_bindgen]
pub struct Engine(core_engine::Engine);

#[wasm_bindgen]
impl Engine {
    /// Construct an engine wrapping `world`. The seed drives BEB; with
    /// the same `(world, seed, schedule, edits)` inputs the engine
    /// produces a byte-identical log.
    #[wasm_bindgen(constructor)]
    pub fn new(world: World, seed: u64) -> Self {
        Self(core_engine::Engine::with_seed(world.0, seed))
    }

    /// Number of live nodes in the engine's world.
    #[wasm_bindgen(js_name = nodeCount)]
    pub fn node_count(&self) -> usize {
        self.0.world().node_count()
    }

    /// Set the MAC configuration for `node`. Pass a JS object matching
    /// the `MacConfig` schema.
    #[wasm_bindgen(js_name = setMacConfig)]
    pub fn set_mac_config(&mut self, node: u32, config: JsValue) -> Result<(), JsValue> {
        let cfg: core_engine::MacConfig = from_js(config)?;
        self.0.set_mac_config(NodeId::new(node), cfg);
        Ok(())
    }

    /// Register a frame for transmission. Returns the assigned `FrameId`
    /// (a number).
    #[wasm_bindgen(js_name = registerFrame)]
    pub fn register_frame(
        &mut self,
        source_node: u32,
        bits: u64,
        is_jam: bool,
        rate_bps: u64,
    ) -> Result<u32, JsValue> {
        let kind = if is_jam {
            SignalKind::Jam
        } else {
            SignalKind::Frame
        };
        let rate = BitRate::from_bps(rate_bps).ok_or_else(|| JsValue::from_str("zero bit rate"))?;
        self.0
            .register_frame(NodeId::new(source_node), Bits::new(bits), kind, rate)
            .map(aether_sonde::event::FrameId::as_u32)
            .map_err(|e| err_to_js(&e))
    }

    /// Schedule a `TxAttempt` at simulation time `time_ps` (picoseconds)
    /// from `node` for `frame`.
    #[wasm_bindgen(js_name = scheduleTxAttempt)]
    pub fn schedule_tx_attempt(&mut self, time_ps: u64, node: u32, frame: u32) {
        self.0.schedule_tx_attempt(
            BitTime::new(time_ps),
            NodeId::new(node),
            aether_sonde::event::FrameId::new(frame),
        );
    }

    /// Run the dispatch loop until `time_ps` (picoseconds).
    #[wasm_bindgen(js_name = runUntil)]
    pub fn run_until(&mut self, time_ps: u64) {
        self.0.run_until(BitTime::new(time_ps));
    }

    /// Drain the queue.
    #[wasm_bindgen(js_name = runUntilIdle)]
    pub fn run_until_idle(&mut self) {
        self.0.run_until_idle();
    }

    /// Apply a topology `Edit`. The `edit` argument is a JS object
    /// matching the `Edit` discriminated-union schema (with `type` field).
    /// Throws an `EditError` JS object on failure.
    #[wasm_bindgen(js_name = applyEdit)]
    pub fn apply_edit(&mut self, edit: JsValue) -> Result<(), JsValue> {
        let edit: core_engine::Edit = from_js(edit)?;
        self.0.apply_edit(edit).map_err(|e| err_to_js(&e))
    }

    /// Snapshot the engine's event log as a JS array.
    pub fn log(&self) -> Result<JsValue, JsValue> {
        to_js(self.0.log())
    }

    /// Typed snapshot of `node`'s link-layer device state. Returns
    /// `null` if `node` is unknown. The returned JS object is a
    /// discriminated union tagged by `type` (`EndStation`, `Repeater`,
    /// `Bridge`, `Switch`).
    #[wasm_bindgen(js_name = deviceSnapshot)]
    pub fn device_snapshot(&self, node: u32) -> Result<JsValue, JsValue> {
        match self.0.device_snapshot(NodeId::new(node)) {
            Some(snap) => to_js(&snap),
            None => Ok(JsValue::NULL),
        }
    }

    /// Apply a typed `DeviceCommand`. The `cmd` argument is a JS
    /// object matching the `DeviceCommand` discriminated-union schema
    /// (with `type` field). Throws a `DeviceCommandError` JS object
    /// on failure (with `kind` field).
    #[wasm_bindgen(js_name = applyDeviceCommand)]
    pub fn apply_device_command(&mut self, cmd: JsValue) -> Result<(), JsValue> {
        let cmd: aether_sonde::device::DeviceCommand = from_js(cmd)?;
        self.0.apply_device_command(cmd).map_err(|e| err_to_js(&e))
    }
}

// ---------------------------------------------------------------------------
// Observables — pure functions over a log snapshot
// ---------------------------------------------------------------------------

/// Whether at least one signal is occupying `node` at time `t_ps`.
/// `log` is the JS object returned by [`Engine::log`].
#[wasm_bindgen(js_name = carrierSense)]
pub fn carrier_sense(log: JsValue, node: u32, t_ps: u64) -> Result<bool, JsValue> {
    let log: CoreLog = from_js(log)?;
    Ok(observe::carrier_sense(
        &log,
        NodeId::new(node),
        BitTime::new(t_ps),
    ))
}

/// Whether `node` has experienced a collision by time `t_ps`.
#[wasm_bindgen(js_name = collisionDetect)]
pub fn collision_detect(log: JsValue, node: u32, t_ps: u64) -> Result<bool, JsValue> {
    let log: CoreLog = from_js(log)?;
    Ok(observe::collision_detect(
        &log,
        NodeId::new(node),
        BitTime::new(t_ps),
    ))
}

/// The earliest time `node` observed a collision, or `null` if none.
#[wasm_bindgen(js_name = firstCollisionDetectAt)]
pub fn first_collision_detect_at(log: JsValue, node: u32) -> Result<Option<u64>, JsValue> {
    let log: CoreLog = from_js(log)?;
    Ok(observe::first_collision_detect_at(&log, NodeId::new(node)).map(BitTime::as_u64))
}
