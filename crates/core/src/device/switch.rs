//! Learning-switch runtime — the round-4 framework smoke test.
//!
//! `SwitchRuntime` exercises three claims about the
//! [`super::LinkLayerBehavior`] framework:
//!
//! 1. **Stateful runtime that mutates on frame arrival.**
//!    `on_frame_arrive` reads the source MAC, inserts/refreshes the
//!    forwarding-table entry for `(source_mac → ingress_port)`, looks
//!    up the destination MAC, and either unicasts (one `Enqueue`) or
//!    floods (one `Enqueue` per non-ingress egress port).
//!
//! 2. **State readable as a typed snapshot.**
//!    `snapshot` returns a [`super::SwitchSnapshot`] with the full
//!    forwarding table, aging threshold, and per-port egress queue
//!    depth + busy flag.
//!
//! 3. **State editable via typed commands.**
//!    `apply_command` dispatches on
//!    [`super::DeviceCommand`] variants:
//!    - `InsertMacEntry` adds a manual entry (origin
//!      `ManualInsert`).
//!    - `RemoveMacEntry` removes by MAC.
//!    - `FlushMacTable` empties the table.
//!    - `SetSwitchAgingThreshold` reconfigures aging.
//!
//! Aging fires via [`crate::event::Event::AgingTick`]: when the first
//! entry is learned, the runtime schedules a tick at
//! `now + aging_threshold`. Each tick expires entries older than the
//! threshold and schedules the next tick if the table is still
//! non-empty.

use std::collections::{HashMap, VecDeque};

use crate::bridge::{Forwarding, FloodForwarding};
use crate::engine::Engine;
use crate::event::{Event, FrameId, Phase};
use crate::frame::MacAddress;
use crate::signal::NodeId;
use crate::time::{BitRate, BitTime};
use crate::topology::{NodeKind, PortId, SwitchData};

use super::{
    DeviceCommand, DeviceCommandError, DeviceSnapshot, LinkLayerBehavior, MacEntryOrigin,
    MacTableEntry, SwitchSnapshot,
};

/// Per-switch runtime: forwarding table, per-port egress queue and
/// busy flag, aging threshold (mutable mid-simulation), and a flag
/// recording whether an aging-tick chain is already scheduled.
#[derive(Debug, Clone)]
pub(crate) struct SwitchRuntime {
    /// Static config snapshot at construction. The `aging_threshold`
    /// is mirrored on the runtime so `SetSwitchAgingThreshold` can
    /// mutate it without touching `World`.
    config: SwitchData,
    /// Live aging threshold (may differ from `config.aging_threshold`
    /// after a `SetSwitchAgingThreshold` command).
    aging_threshold: BitTime,
    /// Forwarding table indexed by destination MAC.
    mac_table: HashMap<MacAddress, MacTableEntry>,
    /// Per-port egress queue (mirrors `BridgeRuntime`).
    egress_queues: HashMap<PortId, VecDeque<FrameId>>,
    /// Per-port busy flag.
    egress_busy: HashMap<PortId, bool>,
    /// Whether an aging-tick is already scheduled. Prevents a new
    /// learn from scheduling redundant ticks.
    aging_scheduled: bool,
}

impl SwitchRuntime {
    /// Construct a runtime from static `SwitchData`. The runtime's
    /// `aging_threshold` mirrors `data.aging_threshold` initially.
    pub(crate) fn new(data: SwitchData) -> Self {
        Self {
            config: data,
            aging_threshold: data.aging_threshold,
            mac_table: HashMap::new(),
            egress_queues: HashMap::new(),
            egress_busy: HashMap::new(),
            aging_scheduled: false,
        }
    }

    /// Insert or refresh a forwarding-table entry. Honors
    /// `mac_table_capacity` (zero means unbounded).
    fn insert_entry(&mut self, entry: MacTableEntry) {
        // If at capacity and the MAC is new, drop the oldest entry.
        if self.config.mac_table_capacity != 0
            && !self.mac_table.contains_key(&entry.mac)
            && self.mac_table.len() >= self.config.mac_table_capacity as usize
        {
            // Find the oldest entry deterministically: by learned_at,
            // then mac for tie-break.
            if let Some((oldest, _)) = self
                .mac_table
                .iter()
                .min_by(|a, b| {
                    a.1.learned_at
                        .cmp(&b.1.learned_at)
                        .then_with(|| a.0.cmp(b.0))
                })
                .map(|(m, e)| (*m, *e))
            {
                self.mac_table.remove(&oldest);
            }
        }
        self.mac_table.insert(entry.mac, entry);
    }

    /// Schedule an `AgingTick` if aging is enabled, the table is
    /// non-empty, and no tick is already scheduled.
    fn maybe_schedule_aging(&mut self, engine: &mut Engine, node: NodeId) {
        if self.aging_threshold == BitTime::ZERO || self.mac_table.is_empty() {
            return;
        }
        if self.aging_scheduled {
            return;
        }
        let now = engine.now();
        engine.schedule(
            now + self.aging_threshold,
            Phase::LocalDecision,
            Event::AgingTick { node },
        );
        self.aging_scheduled = true;
    }

    /// Sorted view of the table for snapshots. Sorted by
    /// `learned_at`, then `mac`, so snapshot equality is
    /// deterministic across runs.
    fn sorted_entries(&self) -> Vec<MacTableEntry> {
        let mut v: Vec<MacTableEntry> = self.mac_table.values().copied().collect();
        v.sort_by(|a, b| {
            a.learned_at
                .cmp(&b.learned_at)
                .then_with(|| a.mac.cmp(&b.mac))
        });
        v
    }

    fn queue_depths(&self) -> Vec<(PortId, usize)> {
        let mut v: Vec<(PortId, usize)> = self
            .egress_queues
            .iter()
            .map(|(p, q)| (*p, q.len()))
            .collect();
        v.sort_by_key(|(p, _)| *p);
        v
    }

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

impl LinkLayerBehavior for SwitchRuntime {
    fn on_frame_arrive(
        &mut self,
        engine: &mut Engine,
        node: NodeId,
        port: PortId,
        frame_id: FrameId,
    ) {
        let now = engine.now();

        // 1. Learn from the source MAC (if not the all-zeros opaque
        //    sentinel — a `Frame::opaque` doesn't carry semantic
        //    addresses).
        let frame = engine.registered_frame(frame_id).copied();
        if let Some(f) = frame
            && f.source != MacAddress::ZERO
        {
            // Existing entry: refresh learned_at only if the entry
            // came from learning. Manual entries are not refreshed.
            let should_refresh = self
                .mac_table
                .get(&f.source)
                .is_none_or(|e| matches!(e.origin, MacEntryOrigin::Learned));
            if should_refresh {
                self.insert_entry(MacTableEntry {
                    mac: f.source,
                    port,
                    learned_at: now,
                    origin: MacEntryOrigin::Learned,
                });
                self.maybe_schedule_aging(engine, node);
            }
        }

        // 2. Forward: unicast to the known port if the destination
        //    is in the table; otherwise flood (every non-ingress port).
        let dest = frame.map(|f| f.destination);
        let known_egress = dest
            .filter(|d| *d != MacAddress::ZERO)
            .and_then(|d| self.mac_table.get(&d).copied())
            .map(|e| e.port);

        let egress_ports: Vec<PortId> = if let Some(eg) = known_egress
            && eg != port
        {
            // Known unicast (and not back out the ingress).
            vec![eg]
        } else if known_egress.is_some() {
            // Known but ingress equals egress — drop (don't reflect).
            Vec::new()
        } else {
            // Unknown destination: flood like a bridge.
            let all_ports = engine.bridge_ports(node);
            let policy = FloodForwarding;
            policy.egress_ports(&(), port, &all_ports)
        };

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
        // Identical to `BridgeRuntime`: clear busy and schedule next
        // dequeue if the queue has a frame.
        self.egress_busy.insert(port, false);
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

    fn on_aging_tick(&mut self, engine: &mut Engine, node: NodeId) {
        self.aging_scheduled = false;
        if self.aging_threshold == BitTime::ZERO {
            return;
        }
        let now = engine.now();
        // Expire entries older than `aging_threshold`. Manual entries
        // are not aged out.
        let cutoff = now.as_u64().saturating_sub(self.aging_threshold.as_u64());
        self.mac_table.retain(|_, e| {
            matches!(e.origin, MacEntryOrigin::ManualInsert) || e.learned_at.as_u64() >= cutoff
        });
        // Reschedule if the table is still non-empty.
        self.maybe_schedule_aging(engine, node);
    }

    fn enqueue_for_port(&mut self, port: PortId, frame: FrameId) {
        self.egress_queues.entry(port).or_default().push_back(frame);
    }

    fn dequeue_for_port(&mut self, port: PortId, expected: FrameId) -> bool {
        let queue = self.egress_queues.entry(port).or_default();
        match queue.pop_front() {
            Some(f) if f == expected => true,
            Some(other) => {
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
            Some(NodeKind::Switch(data)) => {
                (data.decode_threshold.as_u64(), data.processing_delay)
            }
            _ => (
                self.config.decode_threshold.as_u64(),
                self.config.processing_delay,
            ),
        };
        DeviceSnapshot::Switch(SwitchSnapshot {
            decode_threshold_bits,
            processing_delay,
            aging_threshold: self.aging_threshold,
            mac_table: self.sorted_entries(),
            egress_queue_depth: self.queue_depths(),
            egress_busy: self.busy_flags(),
        })
    }

    fn apply_command(&mut self, cmd: &DeviceCommand) -> Result<(), DeviceCommandError> {
        match cmd {
            DeviceCommand::InsertMacEntry { node: _, mac, port } => {
                self.insert_entry(MacTableEntry {
                    mac: *mac,
                    port: *port,
                    learned_at: BitTime::ZERO,
                    origin: MacEntryOrigin::ManualInsert,
                });
                Ok(())
            }
            DeviceCommand::RemoveMacEntry { node: _, mac } => {
                self.mac_table.remove(mac);
                Ok(())
            }
            DeviceCommand::FlushMacTable { node: _ } => {
                self.mac_table.clear();
                Ok(())
            }
            DeviceCommand::SetSwitchAgingThreshold {
                node: _,
                threshold,
            } => {
                self.aging_threshold = *threshold;
                Ok(())
            }
        }
    }
}
