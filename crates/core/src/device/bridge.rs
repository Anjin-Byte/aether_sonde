//! Flooding-bridge runtime.
//!
//! A `BridgeRuntime` maintains the per-port egress queue and busy
//! flag for one bridge node. The engine drives it through the
//! [`super::LinkLayerBehavior`] trait:
//!
//! - On `on_frame_arrive`, the bridge selects egress ports via
//!   [`crate::bridge::FloodForwarding`] and asks the engine to
//!   schedule an `Enqueue` event for each.
//! - On `on_egress_idle`, the bridge clears the busy flag for the
//!   port and, if the queue has a next frame, asks the engine to
//!   schedule a `Dequeue` after the inter-frame gap.
//!
//! Queue mechanics (push/pop/busy) are exposed as trait methods so
//! the engine's `Enqueue`/`Dequeue` handlers can drive any queueing
//! device family (currently `Bridge`; round 6 adds `Switch`) without
//! pattern-matching on the runtime variant.

use std::collections::{HashMap, VecDeque};

use crate::bridge::{Forwarding, FloodForwarding};
use crate::engine::Engine;
use crate::event::{Event, FrameId, Phase};
use crate::signal::NodeId;
use crate::time::BitRate;
use crate::topology::{NodeKind, BridgeData, PortId};

use super::{
    BridgeSnapshot, DeviceCommand, DeviceCommandError, DeviceSnapshot, LinkLayerBehavior,
};

/// Per-port egress queue + busy flag. Mirrors the round-3 inline
/// `BridgeRuntimeState` exactly so the migration produces
/// byte-identical logs against the existing oracle suite.
#[derive(Debug, Clone, Default)]
pub(crate) struct BridgeRuntime {
    egress_queues: HashMap<PortId, VecDeque<FrameId>>,
    egress_busy: HashMap<PortId, bool>,
}

impl BridgeRuntime {
    /// Per-port queue depth, sorted by port id for deterministic
    /// snapshots.
    #[allow(dead_code, reason = "wired up by snapshot in step 4")]
    fn queue_depths(&self) -> Vec<(PortId, usize)> {
        let mut v: Vec<(PortId, usize)> = self
            .egress_queues
            .iter()
            .map(|(p, q)| (*p, q.len()))
            .collect();
        v.sort_by_key(|(p, _)| *p);
        v
    }

    /// Per-port busy flag, sorted by port id for deterministic
    /// snapshots.
    #[allow(dead_code, reason = "wired up by snapshot in step 4")]
    fn busy_flags(&self) -> Vec<(PortId, bool)> {
        let mut v: Vec<(PortId, bool)> = self
            .egress_busy
            .iter()
            .map(|(p, b)| (*p, *b))
            .collect();
        v.sort_by_key(|(p, _)| *p);
        v
    }
}

impl LinkLayerBehavior for BridgeRuntime {
    fn on_frame_arrive(
        &mut self,
        engine: &mut Engine,
        node: NodeId,
        port: PortId,
        frame_id: FrameId,
    ) {
        // Flood: schedule an `Enqueue` for every egress port the
        // forwarding policy selects (everything except the ingress).
        let all_ports = engine.bridge_ports(node);
        let policy = FloodForwarding;
        let egress_ports: Vec<PortId> = policy.egress_ports(&(), port, &all_ports);
        let now = engine.now();
        for egress_port in egress_ports {
            if let Some(serializer) = engine.world().bridge_egress_serializer(node, egress_port) {
                engine.schedule(
                    now,
                    Phase::LocalDecision,
                    Event::Enqueue {
                        serializer,
                        frame: frame_id,
                    },
                );
            }
        }
    }

    fn on_egress_idle(&mut self, engine: &mut Engine, node: NodeId, port: PortId) {
        // The bridge's egress on `port` just freed up. Clear busy.
        self.egress_busy.insert(port, false);
        // If the queue has another frame ready, schedule its
        // `Dequeue` after the inter-frame gap. Mirrors the round-3
        // inline branch of `handle_tx_end`.
        let Some(next_frame) = self
            .egress_queues
            .get(&port)
            .and_then(|q| q.front().copied())
        else {
            return;
        };
        let Some(serializer) = engine.world().bridge_egress_serializer(node, port) else {
            return;
        };
        let now = engine.now();
        let mac = engine.mac_config(node);
        let rate = engine
            .bridge_egress_rate(node, port)
            .unwrap_or(BitRate::ETHERNET_10M);
        let ifg = mac.ifg.duration_at(rate);
        engine.schedule(
            now + ifg,
            Phase::LocalDecision,
            Event::Dequeue {
                serializer,
                frame: next_frame,
            },
        );
    }

    fn enqueue_for_port(&mut self, port: PortId, frame: FrameId) {
        self.egress_queues.entry(port).or_default().push_back(frame);
    }

    fn dequeue_for_port(&mut self, port: PortId, expected: FrameId) -> bool {
        let queue = self.egress_queues.entry(port).or_default();
        match queue.pop_front() {
            Some(f) if f == expected => true,
            Some(other) => {
                // Stale Dequeue (out-of-order): restore the front
                // and report no match. Mirrors the round-3 branch
                // in `handle_dequeue`.
                queue.push_front(other);
                false
            }
            None => false,
        }
    }

    fn front_for_port(&self, port: PortId) -> Option<FrameId> {
        self.egress_queues.get(&port).and_then(|q| q.front().copied())
    }

    fn set_busy(&mut self, port: PortId, busy: bool) {
        self.egress_busy.insert(port, busy);
    }

    fn is_busy(&self, port: PortId) -> bool {
        self.egress_busy.get(&port).copied().unwrap_or(false)
    }

    fn snapshot(&self, engine: &Engine, node: NodeId) -> DeviceSnapshot {
        let (decode_threshold_bits, processing_delay) = match engine.world().node(node).copied() {
            Some(NodeKind::Bridge(BridgeData {
                decode_threshold,
                processing_delay,
            })) => (decode_threshold.as_u64(), processing_delay),
            // Should not happen if the runtime exists for this node;
            // fall back to zeros so the snapshot is well-formed.
            _ => (0, crate::time::BitTime::ZERO),
        };
        DeviceSnapshot::Bridge(BridgeSnapshot {
            decode_threshold_bits,
            processing_delay,
            egress_queue_depth: self.queue_depths(),
            egress_busy: self.busy_flags(),
        })
    }

    fn apply_command(&mut self, _cmd: &DeviceCommand) -> Result<(), DeviceCommandError> {
        // Round 4 flooding bridges have no commandable state. The
        // user-facing API still returns `NotApplicable` rather than
        // a panic, so frontends can probe device kinds uniformly.
        Err(DeviceCommandError::NotApplicable {
            reason: "flooding bridge has no commandable state",
        })
    }
}
