//! Link-layer device polymorphism — the framework.
//!
//! # Public surface
//!
//! - [`DeviceSnapshot`]: a typed read-side view of a device's
//!   internal state. Frontends and tests query
//!   `Engine::device_snapshot(node)` to render or assert against
//!   per-device state.
//! - [`DeviceCommand`]: a typed write-side action that mutates a
//!   device's internal state mid-simulation (insert MAC entry, flush
//!   table, etc.). Distinct from [`crate::engine::Edit`], which
//!   mutates *topology*. Applied via `Engine::apply_device_command`.
//! - [`DeviceCommandError`]: typed errors at the command boundary.
//! - [`MacTableEntry`], [`MacEntryOrigin`]: the per-entry shape of
//!   forwarding tables maintained by learning-capable devices.
//!
//! # Crate-internal architecture
//!
//! - `LinkLayerBehavior` trait (crate-private): the per-device
//!   dispatch surface (`on_frame_arrive`, `on_egress_idle`,
//!   `on_aging_tick`, `snapshot`, `apply_command`).
//! - `DeviceRuntime` enum (crate-private): a sealed enum with one
//!   variant per stateful device family. The engine pattern-matches
//!   once to dispatch through the trait.
//!
//! # Dispatch protocol
//!
//! The engine dispatches a hook by *removing* the runtime from
//! `Engine::devices`, calling the trait method (which receives
//! `&mut Engine` so it can `schedule`, `register_frame`, etc.), then
//! reinserting. This take-and-reinsert pattern lets the trait method
//! mutate engine state without borrow-checker fights — the only
//! restriction is that a runtime's hook must not recurse into
//! `Engine::devices` for *its own node*. In practice, hooks call
//! engine methods that touch the queue, frame registry, and side
//! maps, but never re-dispatch the same device.
//!
//! # Recipe — adding a new device family
//!
//! See [`docs/devices.md`](../../../../docs/devices.md) for the
//! step-by-step recipe.

use crate::engine::Engine;
use crate::event::FrameId;
use crate::frame::MacAddress;
use crate::signal::NodeId;
use crate::time::BitTime;
use crate::topology::PortId;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

pub(crate) mod bridge;
pub(crate) mod switch;

// ---------------------------------------------------------------------------
// MacTableEntry / MacEntryOrigin
// ---------------------------------------------------------------------------

/// One entry in a learning device's forwarding table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct MacTableEntry {
    /// The MAC address this entry forwards.
    pub mac: MacAddress,
    /// The egress port frames destined for `mac` should leave on.
    pub port: PortId,
    /// Simulation time at which this entry was learned or inserted.
    pub learned_at: BitTime,
    /// Whether this entry came from auto-learning or a manual command.
    pub origin: MacEntryOrigin,
}

/// How a [`MacTableEntry`] entered the forwarding table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub enum MacEntryOrigin {
    /// Auto-populated by the device's learning logic on frame arrival.
    Learned,
    /// Inserted by an explicit
    /// [`DeviceCommand::InsertMacEntry`] application.
    ManualInsert,
}

// ---------------------------------------------------------------------------
// Per-device snapshot shapes
// ---------------------------------------------------------------------------

/// Snapshot of an end-station node. End stations carry no
/// device-specific runtime state in round 4; the snapshot exists for
/// type-shape uniformity so the [`DeviceSnapshot`] discriminated
/// union covers every node kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct EndStationSnapshot {
    /// Number of ports declared on the end station.
    pub port_count: u32,
}

/// Snapshot of a repeater (hub) node. Repeaters re-emit signals at
/// the PHY layer per axiom A3 and carry no L2 state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct RepeaterSnapshot {
    /// Number of ports declared on the repeater.
    pub port_count: u32,
    /// Repeater re-emit delay (`δ_h` per axiom A3).
    pub delta_h: BitTime,
}

/// Snapshot of a flooding bridge node. Mirrors the runtime state the
/// engine maintains for round-3-and-earlier bridges.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct BridgeSnapshot {
    /// Decode threshold `η_b` (cut-through vs store-and-forward).
    pub decode_threshold_bits: u64,
    /// Processing delay `π_b`.
    pub processing_delay: BitTime,
    /// Per-port egress queue depth at the moment of snapshot.
    pub egress_queue_depth: Vec<(PortId, usize)>,
    /// Per-port serializer-busy flag at the moment of snapshot.
    pub egress_busy: Vec<(PortId, bool)>,
}

/// Snapshot of a learning-capable switch node.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct SwitchSnapshot {
    /// Decode threshold `η_b` (cut-through vs store-and-forward).
    pub decode_threshold_bits: u64,
    /// Processing delay `π_b`.
    pub processing_delay: BitTime,
    /// MAC-aging threshold; entries older than this are expired by
    /// scheduled aging ticks. Zero means aging is disabled.
    pub aging_threshold: BitTime,
    /// All entries currently in the forwarding table, in insertion order.
    pub mac_table: Vec<MacTableEntry>,
    /// Per-port egress queue depth at the moment of snapshot.
    pub egress_queue_depth: Vec<(PortId, usize)>,
    /// Per-port serializer-busy flag at the moment of snapshot.
    pub egress_busy: Vec<(PortId, bool)>,
}

// ---------------------------------------------------------------------------
// DeviceSnapshot — sealed
// ---------------------------------------------------------------------------

/// Typed, publicly-exhaustive snapshot of a device's internal state.
///
/// Returned by `Engine::device_snapshot(node)`. Pattern-matching on
/// the variant gives consumers a fully-typed view without runtime
/// downcasting; adding a new device family is a deliberate breaking
/// change visible at every consumer's `match`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(tag = "type")
)]
pub enum DeviceSnapshot {
    /// End-station device.
    EndStation(EndStationSnapshot),
    /// Repeater (hub) device.
    Repeater(RepeaterSnapshot),
    /// Flooding bridge device.
    Bridge(BridgeSnapshot),
    /// Learning switch device. Round 6 deliverable.
    Switch(SwitchSnapshot),
}

// ---------------------------------------------------------------------------
// DeviceCommand — sealed, mid-simulation device-state edits
// ---------------------------------------------------------------------------

/// A typed mid-simulation mutation of a device's internal state.
///
/// Distinct from [`crate::engine::Edit`], which mutates *topology*.
/// `DeviceCommand` is for changes that don't alter the topology graph
/// — inserting/removing forwarding-table entries, flushing tables,
/// adjusting per-device configuration knobs.
///
/// Applied via `Engine::apply_device_command`. Each application logs
/// an [`crate::event::Event::DeviceCommandApplied`] entry so the
/// determinism contract extends to commands:
/// `(spec, seed, schedule, edits, commands)` → byte-identical log.
///
/// `#[non_exhaustive]` lets future device families add command
/// variants without breaking matchers.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(
    feature = "serde",
    derive(Serialize, Deserialize),
    serde(tag = "type")
)]
#[non_exhaustive]
pub enum DeviceCommand {
    /// Insert a manual entry into a learning device's forwarding table.
    InsertMacEntry {
        /// The device whose table to mutate.
        node: NodeId,
        /// The MAC address to forward.
        mac: MacAddress,
        /// The egress port to associate with `mac`.
        port: PortId,
    },
    /// Remove a forwarding-table entry. No-op if the entry is absent.
    RemoveMacEntry {
        /// The device whose table to mutate.
        node: NodeId,
        /// The MAC address whose entry to remove.
        mac: MacAddress,
    },
    /// Empty a learning device's forwarding table.
    FlushMacTable {
        /// The device whose table to flush.
        node: NodeId,
    },
    /// Change a learning device's MAC-aging threshold. A threshold of
    /// `BitTime::ZERO` disables aging.
    SetSwitchAgingThreshold {
        /// The device to reconfigure.
        node: NodeId,
        /// The new aging threshold.
        threshold: BitTime,
    },
}

impl DeviceCommand {
    /// The [`NodeId`] this command targets.
    #[must_use]
    pub const fn target_node(&self) -> NodeId {
        match self {
            Self::InsertMacEntry { node, .. }
            | Self::RemoveMacEntry { node, .. }
            | Self::FlushMacTable { node }
            | Self::SetSwitchAgingThreshold { node, .. } => *node,
        }
    }
}

// ---------------------------------------------------------------------------
// DeviceCommandError
// ---------------------------------------------------------------------------

/// Errors returned by `Engine::apply_device_command`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize), serde(tag = "kind"))]
#[non_exhaustive]
pub enum DeviceCommandError {
    /// The command targets a non-existent node.
    UnknownNode {
        /// The unknown node ID.
        node: NodeId,
    },
    /// The command's variant is not applicable to the target device's
    /// kind (e.g., `InsertMacEntry` on a flooding `Bridge`).
    NotApplicable {
        /// Human-readable reason.
        reason: &'static str,
    },
    /// The command's argument is invalid for the target device
    /// (e.g., port out of range).
    InvalidArgument {
        /// Human-readable reason.
        reason: &'static str,
    },
}

impl core::fmt::Display for DeviceCommandError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownNode { node } => write!(f, "unknown node: {node:?}"),
            Self::NotApplicable { reason } => {
                write!(f, "command not applicable: {reason}")
            }
            Self::InvalidArgument { reason } => {
                write!(f, "invalid command argument: {reason}")
            }
        }
    }
}

impl core::error::Error for DeviceCommandError {}

// ---------------------------------------------------------------------------
// LinkLayerBehavior trait — crate-private dispatch surface
// ---------------------------------------------------------------------------

/// The per-device dispatch hooks the engine calls when something
/// happens at this node.
///
/// Crate-private — not part of the public API. External crates
/// cannot add device families; they're a finite, modeled set that
/// extends through the [`DeviceRuntime`] sealed enum.
///
/// Hooks receive `&mut Engine` directly. The engine's dispatch
/// site removes the runtime from `Engine::devices` before the call
/// (see the take-and-reinsert protocol in the module docs), so the
/// hook can freely call `engine.schedule`, `engine.register_frame`,
/// etc. without aliasing its own state.
pub(crate) trait LinkLayerBehavior {
    /// A frame's leading edge has arrived at `port` and decode is
    /// complete. The runtime decides forwarding (flood, unicast,
    /// drop) and asks the engine to enqueue the appropriate egress
    /// events.
    fn on_frame_arrive(
        &mut self,
        engine: &mut Engine,
        node: NodeId,
        port: PortId,
        frame_id: FrameId,
    );

    /// An egress serializer at `port` became idle. The runtime can
    /// dequeue the next frame and ask the engine to schedule its
    /// transmission.
    fn on_egress_idle(&mut self, engine: &mut Engine, node: NodeId, port: PortId);

    /// Periodic time-based hook (aging, heartbeat, etc.). Default
    /// no-op for devices that don't need it.
    #[allow(dead_code, reason = "wired up by Switch in step 6")]
    fn on_aging_tick(&mut self, _engine: &mut Engine, _node: NodeId) {}

    // -- Egress-queue interface ------------------------------------------
    //
    // Default no-op impls let non-queueing device families (end
    // stations, repeaters) ignore these. Queueing devices (Bridge,
    // Switch) override them. The engine's `Enqueue`/`Dequeue`
    // handlers route through these so the engine never has to
    // pattern-match on the runtime variant for queue mechanics.

    /// Push `frame` onto the back of the egress queue for `port`.
    /// Default: no-op for non-queueing devices.
    #[allow(dead_code, reason = "wired up in step 3")]
    fn enqueue_for_port(&mut self, _port: PortId, _frame: FrameId) {}

    /// Pop the front of the egress queue for `port`, expecting it
    /// to be `expected`. Returns `true` if the front matched and
    /// was popped, `false` if the front did not match (the original
    /// front is restored) or if the queue was empty. Default:
    /// `false` for non-queueing devices.
    #[allow(dead_code, reason = "wired up in step 3")]
    fn dequeue_for_port(&mut self, _port: PortId, _expected: FrameId) -> bool {
        false
    }

    /// Front of the egress queue for `port` without removing.
    /// Default: `None` for non-queueing devices.
    #[allow(dead_code, reason = "wired up in step 3")]
    fn front_for_port(&self, _port: PortId) -> Option<FrameId> {
        None
    }

    /// Set the egress-busy flag for `port`. Default: no-op for
    /// non-queueing devices.
    #[allow(dead_code, reason = "wired up in step 3")]
    fn set_busy(&mut self, _port: PortId, _busy: bool) {}

    /// Whether the egress for `port` is currently busy. Default:
    /// `false` for non-queueing devices.
    #[allow(dead_code, reason = "wired up in step 3")]
    fn is_busy(&self, _port: PortId) -> bool {
        false
    }

    /// Snapshot the runtime's internal state for inspection.
    fn snapshot(&self, engine: &Engine, node: NodeId) -> DeviceSnapshot;

    /// Apply a [`DeviceCommand`]. Variants that don't apply to this
    /// device kind return [`DeviceCommandError::NotApplicable`].
    fn apply_command(&mut self, cmd: &DeviceCommand) -> Result<(), DeviceCommandError>;
}

// ---------------------------------------------------------------------------
// DeviceRuntime — crate-private sealed enum
// ---------------------------------------------------------------------------

/// The runtime state of a stateful link-layer device. End stations
/// and repeaters carry no per-device state in round 4 and don't
/// appear here; their snapshots are computed from engine-side state
/// at query time.
///
/// Sealed and crate-private. Step 3 introduces the `Bridge` variant;
/// step 6 introduces the `Switch` variant.
#[derive(Debug, Clone)]
pub(crate) enum DeviceRuntime {
    /// Flooding bridge runtime — per-port egress queue + busy flag.
    Bridge(bridge::BridgeRuntime),
    /// Learning switch runtime — MAC table + per-port egress queue.
    Switch(switch::SwitchRuntime),
}

impl DeviceRuntime {
    /// Pattern-match on the variant to call the appropriate
    /// `LinkLayerBehavior` hook. Used by the engine's dispatch sites
    /// after a take-and-reinsert.
    pub(crate) fn on_frame_arrive(
        &mut self,
        engine: &mut Engine,
        node: NodeId,
        port: PortId,
        frame_id: FrameId,
    ) {
        match self {
            Self::Bridge(b) => b.on_frame_arrive(engine, node, port, frame_id),
            Self::Switch(s) => s.on_frame_arrive(engine, node, port, frame_id),
        }
    }

    /// See [`Self::on_frame_arrive`].
    pub(crate) fn on_egress_idle(&mut self, engine: &mut Engine, node: NodeId, port: PortId) {
        match self {
            Self::Bridge(b) => b.on_egress_idle(engine, node, port),
            Self::Switch(s) => s.on_egress_idle(engine, node, port),
        }
    }

    /// Forward to the variant's [`LinkLayerBehavior::on_aging_tick`].
    pub(crate) fn on_aging_tick(&mut self, engine: &mut Engine, node: NodeId) {
        match self {
            Self::Bridge(b) => b.on_aging_tick(engine, node),
            Self::Switch(s) => s.on_aging_tick(engine, node),
        }
    }

    /// Forward to the variant's [`LinkLayerBehavior::enqueue_for_port`].
    pub(crate) fn enqueue_for_port(&mut self, port: PortId, frame: FrameId) {
        match self {
            Self::Bridge(b) => b.enqueue_for_port(port, frame),
            Self::Switch(s) => s.enqueue_for_port(port, frame),
        }
    }

    /// Forward to the variant's [`LinkLayerBehavior::dequeue_for_port`].
    pub(crate) fn dequeue_for_port(&mut self, port: PortId, expected: FrameId) -> bool {
        match self {
            Self::Bridge(b) => b.dequeue_for_port(port, expected),
            Self::Switch(s) => s.dequeue_for_port(port, expected),
        }
    }

    /// Forward to the variant's [`LinkLayerBehavior::front_for_port`].
    #[allow(dead_code, reason = "exposed for future engine dispatch sites")]
    pub(crate) fn front_for_port(&self, port: PortId) -> Option<FrameId> {
        match self {
            Self::Bridge(b) => b.front_for_port(port),
            Self::Switch(s) => s.front_for_port(port),
        }
    }

    /// Forward to the variant's [`LinkLayerBehavior::set_busy`].
    pub(crate) fn set_busy(&mut self, port: PortId, busy: bool) {
        match self {
            Self::Bridge(b) => b.set_busy(port, busy),
            Self::Switch(s) => s.set_busy(port, busy),
        }
    }

    /// Forward to the variant's [`LinkLayerBehavior::is_busy`].
    pub(crate) fn is_busy(&self, port: PortId) -> bool {
        match self {
            Self::Bridge(b) => b.is_busy(port),
            Self::Switch(s) => s.is_busy(port),
        }
    }

    /// Forward to the variant's [`LinkLayerBehavior::snapshot`].
    pub(crate) fn snapshot(&self, engine: &Engine, node: NodeId) -> DeviceSnapshot {
        match self {
            Self::Bridge(b) => b.snapshot(engine, node),
            Self::Switch(s) => s.snapshot(engine, node),
        }
    }

    /// Forward to the variant's [`LinkLayerBehavior::apply_command`].
    pub(crate) fn apply_command(
        &mut self,
        cmd: &DeviceCommand,
    ) -> Result<(), DeviceCommandError> {
        match self {
            Self::Bridge(b) => b.apply_command(cmd),
            Self::Switch(s) => s.apply_command(cmd),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test code"
)]
mod tests {
    use super::*;

    #[test]
    fn mac_table_entry_carries_origin() {
        let entry = MacTableEntry {
            mac: MacAddress::new([1, 2, 3, 4, 5, 6]),
            port: PortId::new(0),
            learned_at: BitTime::from_micros(10),
            origin: MacEntryOrigin::Learned,
        };
        assert_eq!(entry.origin, MacEntryOrigin::Learned);
    }

    #[test]
    fn device_command_error_implements_error_trait() {
        let err = DeviceCommandError::UnknownNode {
            node: NodeId::new(99),
        };
        let _: &dyn core::error::Error = &err;
        let msg = format!("{err}");
        assert!(msg.contains("unknown node"));
    }

    #[test]
    fn device_command_variants_construct_cleanly() {
        let cmds = [
            DeviceCommand::InsertMacEntry {
                node: NodeId::new(0),
                mac: MacAddress::ZERO,
                port: PortId::new(0),
            },
            DeviceCommand::RemoveMacEntry {
                node: NodeId::new(0),
                mac: MacAddress::ZERO,
            },
            DeviceCommand::FlushMacTable {
                node: NodeId::new(0),
            },
            DeviceCommand::SetSwitchAgingThreshold {
                node: NodeId::new(0),
                threshold: BitTime::from_micros(100),
            },
        ];
        // Each variant constructs without panicking.
        assert_eq!(cmds.len(), 4);
    }

    #[test]
    fn device_snapshot_variants_are_publicly_exhaustive() {
        // Compile-time check: matching every variant works.
        let s = DeviceSnapshot::EndStation(EndStationSnapshot { port_count: 2 });
        let label = match s {
            DeviceSnapshot::EndStation(_) => "end-station",
            DeviceSnapshot::Repeater(_) => "repeater",
            DeviceSnapshot::Bridge(_) => "bridge",
            DeviceSnapshot::Switch(_) => "switch",
        };
        assert_eq!(label, "end-station");
    }
}
