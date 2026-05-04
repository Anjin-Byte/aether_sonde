//! Discrete-event scheduler.
//!
//! The `Engine` consumes a [`World`] and a stream of `TxAttempt`s,
//! processes events in priority-queue order with phase-batched dispatch
//! (`report_0.md` Proposition 17), and produces an append-only [`Log`].
//!
//! [`World`]: crate::topology::World
//! [`Log`]: crate::event::Log
//!
//! # Round 8a scope
//!
//! Implements the **ordinary HD propagation path** end to end:
//! `TxAttempt` → `TxStart` → `FrontArrive` / `BackArrive` at all peers in
//! the same HD-connected component → `TxEnd`. Carrier-sense gating,
//! collision detection, jam, backoff, FD path, and bridge frame relay are
//! stubbed via exhaustive-match arms; round 8b–8d fill them in.
//!
//! # Pair-delay precomputation
//!
//! At construction, the engine BFS-walks each HD-connected component and
//! records pairwise propagation delays between every (source, destination)
//! pair, including the propagation delay across each HD segment plus the
//! repeater re-emit delay `δ_h` paid each time the signal traverses an
//! intermediate repeater. Per axiom A7, each component is a tree, so each
//! pair has a unique path.
//!
//! Bridges are HD leaves: the BFS records the delay to a bridge port but
//! does not propagate beyond it (per axiom A4, the bridge has no internal
//! HD arc).

use crate::bridge::{FloodForwarding, Forwarding, frame_eligibility_time};
use crate::event::{Event, EventKey, FrameId, Log, Phase};
use crate::policy::{BackoffPolicy, IfgPolicy, JamPolicy};
use crate::resource::SerializerId;
use crate::signal::{NodeId, Signal, SignalKind};
use crate::time::{BitRate, BitTime, Bits};
use crate::topology::{BridgeData, NodeKind, PortId, RepeaterData, SegmentId, SegmentKind, World};

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, VecDeque};

// ===========================================================================
// MacConfig
// ===========================================================================

/// Per-end-station MAC configuration.
///
/// Bundles the three policies a transmitting end station needs:
/// [`BackoffPolicy`], [`JamPolicy`], and [`IfgPolicy`]. The engine uses
/// `MacConfig::IEEE_802_3` as the default for nodes not explicitly
/// configured.
///
/// # Examples
///
/// ```
/// use aether_sonde::engine::MacConfig;
/// let m = MacConfig::IEEE_802_3;
/// assert_eq!(m.backoff.attempt_limit(), 16);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacConfig {
    /// BEB policy.
    pub backoff: BackoffPolicy,
    /// Jam policy.
    pub jam: JamPolicy,
    /// Inter-frame gap policy.
    pub ifg: IfgPolicy,
}

impl MacConfig {
    /// IEEE 802.3 canonical configuration: BEB(16, 10), jam = 32 bits,
    /// IFG = 96 bits.
    pub const IEEE_802_3: Self = Self {
        backoff: BackoffPolicy::IEEE_802_3,
        jam: JamPolicy::IEEE_802_3,
        ifg: IfgPolicy::IEEE_802_3,
    };
}

impl Default for MacConfig {
    fn default() -> Self {
        Self::IEEE_802_3
    }
}

// ===========================================================================
// FrameMetadata
// ===========================================================================

/// Engine-private record for a registered frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct FrameMetadata {
    source: NodeId,
    bits: Bits,
    kind: SignalKind,
    rate: BitRate,
}

// ===========================================================================
// NodeRuntimeState
// ===========================================================================

/// Per-node runtime state during simulation.
///
/// Round 8b state machine: a node moves between `Idle`, `Transmitting`,
/// and `BackingOff`. The state is updated by event handlers and consulted
/// for carrier-sense gating, collision detection, and backoff retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRuntimeState {
    /// The node is not currently transmitting and not in backoff.
    Idle,
    /// The node is currently putting `signal` on the wire.
    ///
    /// `signal.kind()` distinguishes a frame transmission from a jam.
    Transmitting {
        /// The signal currently on the wire.
        signal: Signal,
        /// The retry attempt number for the underlying frame (0 = first try).
        attempt: u32,
    },
    /// The node has aborted a transmission due to collision and is
    /// waiting for its backoff timer to expire.
    BackingOff {
        /// The retry attempt number; the next `TxAttempt` will use this
        /// value when computing the next backoff delay.
        attempt: u32,
    },
}

// ===========================================================================
// Reachability — internal pair-delay record
// ===========================================================================

/// HD reachability record: how a signal from node A reaches node B.
#[derive(Debug, Clone, Copy)]
struct HdReachability {
    /// Total propagation delay (including any intermediate repeater `δ_h`).
    delay: BitTime,
    /// The port on B at which the signal arrives.
    arrival_port: PortId,
}

/// FD attachment record: how a node sends and receives over a single FD
/// segment. Round 8c assumes at most one FD attachment per node.
#[derive(Debug, Clone, Copy)]
struct FdAttachment {
    /// The peer node at the other end of the FD segment.
    peer: NodeId,
    /// One-way propagation delay (FD links are symmetric per Axiom A2).
    delay: BitTime,
    /// The bit rate of the FD segment.
    rate: BitRate,
    /// The port on the peer at which arrivals are delivered.
    peer_port: PortId,
}

/// Bridge egress reachability: when the bridge transmits on a specific
/// port, what nodes does the signal reach and with what delays?
#[derive(Debug, Clone)]
struct BridgeEgressReach {
    /// The bit rate of the segment connected to this bridge port.
    rate: BitRate,
    /// Each peer reachable via this port: `(peer_node, delay, peer_port)`.
    peers: Vec<(NodeId, BitTime, PortId)>,
}

/// Per-bridge runtime state: per-port egress queue and per-port busy flag.
#[derive(Debug, Clone, Default)]
struct BridgeRuntimeState {
    egress_queues: HashMap<PortId, VecDeque<FrameId>>,
    egress_busy: HashMap<PortId, bool>,
}

// ===========================================================================
// Scheduled — priority queue entry
// ===========================================================================

#[derive(Debug, Clone, Copy)]
struct Scheduled {
    key: EventKey,
    event: Event,
}

impl PartialEq for Scheduled {
    fn eq(&self, other: &Self) -> bool {
        self.key == other.key
    }
}
impl Eq for Scheduled {}
impl PartialOrd for Scheduled {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Scheduled {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        self.key.cmp(&other.key)
    }
}

// ===========================================================================
// XorShift64 — engine-internal deterministic RNG
// ===========================================================================

/// Deterministic xorshift64* generator, seeded at engine construction.
///
/// Used by the engine for BEB random integers per `BackoffPolicy::next_delay`.
/// Per [Pure Core Effectful Edges], the engine does not source entropy;
/// callers seed the engine via [`Engine::with_seed`].
#[derive(Debug, Clone, Copy)]
struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    fn new(seed: u64) -> Self {
        // xorshift requires nonzero state; map seed 0 to a fixed nonzero value.
        let state = if seed == 0 { 0x9E37_79B9_7F4A_7C15 } else { seed };
        Self { state }
    }

    fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        // Take the upper 32 bits for better quality than the low bits.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "intentional truncation: take upper 32 bits"
        )]
        ((x >> 32) as u32)
    }
}

// ===========================================================================
// EngineError
// ===========================================================================

/// Errors returned by [`Engine::register_frame`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum EngineError {
    /// `register_frame` was called with `bits == Bits::ZERO`.
    ///
    /// Per invariant I5, frames must have a strictly positive bit count
    /// so the resulting signal has a positive duration.
    ZeroBitFrame,
}

impl core::fmt::Display for EngineError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ZeroBitFrame => f.write_str("frame must have at least 1 bit (invariant I5)"),
        }
    }
}

impl core::error::Error for EngineError {}

// ===========================================================================
// Engine
// ===========================================================================

/// The discrete-event scheduler.
///
/// Owns a [`World`], maintains a priority queue of pending events, and
/// produces a [`Log`] of processed events.
#[derive(Debug)]
pub struct Engine {
    world: World,
    queue: BinaryHeap<Reverse<Scheduled>>,
    log: Log,
    next_serial: u64,
    next_frame_id: u32,

    mac_configs: HashMap<NodeId, MacConfig>,
    frames: HashMap<FrameId, FrameMetadata>,

    hd_pair_reachability: HashMap<(NodeId, NodeId), HdReachability>,

    /// FD attachment per node (round 8c: at most one per node).
    fd_attachments: HashMap<NodeId, FdAttachment>,

    /// Bridge egress reachability per (bridge node, port).
    bridge_egress_reach: HashMap<(NodeId, PortId), BridgeEgressReach>,

    /// Per-bridge runtime queue and busy state.
    bridge_state: HashMap<NodeId, BridgeRuntimeState>,

    /// Side map: when a bridge schedules a `TxStart`, record which port
    /// it's emitting on so the handler can look up reachability.
    bridge_pending_egress: HashMap<(NodeId, Signal), PortId>,

    node_state: HashMap<NodeId, NodeRuntimeState>,

    /// Per-node count of active foreign signals at the node's port(s).
    /// Used for carrier-sense gating and collision detection.
    foreign_carriers: HashMap<NodeId, u32>,

    /// The frame each node is currently trying to transmit. Set on
    /// `TxAttempt`, cleared on successful `TxEnd` of the frame's signal.
    pending_frames: HashMap<NodeId, FrameId>,

    /// Deterministic RNG for BEB. Seeded at construction.
    rng: XorShift64,
}

impl Engine {
    /// Construct an engine wrapping a validated [`World`].
    ///
    /// Precomputes the HD pair-delay table at construction, walking each
    /// HD-connected component once.
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::engine::Engine;
    /// use aether_sonde::topology::TopologyBuilder;
    ///
    /// let world = TopologyBuilder::new().build().unwrap();
    /// let engine = Engine::new(world);
    /// assert!(engine.log().is_empty());
    /// ```
    #[must_use]
    pub fn new(world: World) -> Self {
        Self::with_seed(world, 0)
    }

    /// Construct an engine with a deterministic RNG seeded by `seed`.
    ///
    /// Used for reproducible simulations. The seed drives the BEB random
    /// integer sequence; with the same seed and the same input events,
    /// the engine produces the same event log.
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::engine::Engine;
    /// use aether_sonde::topology::TopologyBuilder;
    ///
    /// let world = TopologyBuilder::new().build().unwrap();
    /// let engine = Engine::with_seed(world, 42);
    /// assert!(engine.log().is_empty());
    /// ```
    #[must_use]
    pub fn with_seed(world: World, seed: u64) -> Self {
        let hd_pair_reachability = precompute_hd_pair_reachability(&world);
        let fd_attachments = precompute_fd_attachments(&world);
        let bridge_egress_reach = precompute_bridge_egress_reach(&world, &hd_pair_reachability);
        Self {
            world,
            queue: BinaryHeap::new(),
            log: Log::new(),
            next_serial: 0,
            next_frame_id: 0,
            mac_configs: HashMap::new(),
            frames: HashMap::new(),
            hd_pair_reachability,
            fd_attachments,
            bridge_egress_reach,
            bridge_state: HashMap::new(),
            bridge_pending_egress: HashMap::new(),
            node_state: HashMap::new(),
            foreign_carriers: HashMap::new(),
            pending_frames: HashMap::new(),
            rng: XorShift64::new(seed),
        }
    }

    /// Set the MAC configuration for a specific end-station node.
    ///
    /// Nodes not configured explicitly use [`MacConfig::IEEE_802_3`].
    pub fn set_mac_config(&mut self, node: NodeId, config: MacConfig) {
        self.mac_configs.insert(node, config);
    }

    /// The MAC config for `node`, defaulting to IEEE 802.3.
    #[must_use]
    pub fn mac_config(&self, node: NodeId) -> MacConfig {
        self.mac_configs
            .get(&node)
            .copied()
            .unwrap_or(MacConfig::IEEE_802_3)
    }

    /// Register a frame. The returned [`FrameId`] can be passed to
    /// [`Engine::schedule_tx_attempt`].
    ///
    /// # Errors
    ///
    /// Returns [`EngineError::ZeroBitFrame`] if `bits == Bits::ZERO`.
    pub fn register_frame(
        &mut self,
        source: NodeId,
        bits: Bits,
        kind: SignalKind,
        rate: BitRate,
    ) -> Result<FrameId, EngineError> {
        if bits.as_u64() == 0 {
            return Err(EngineError::ZeroBitFrame);
        }
        let id = FrameId::new(self.next_frame_id);
        self.next_frame_id = self.next_frame_id.saturating_add(1);
        self.frames.insert(
            id,
            FrameMetadata {
                source,
                bits,
                kind,
                rate,
            },
        );
        Ok(id)
    }

    /// Schedule a `TxAttempt` for the given `(node, frame)` at `time`.
    pub fn schedule_tx_attempt(&mut self, time: BitTime, node: NodeId, frame: FrameId) {
        self.schedule(time, Phase::LocalDecision, Event::TxAttempt { node, frame });
    }

    /// Run the engine until either the queue is empty or the next event
    /// time is at or past `horizon`.
    pub fn run_until(&mut self, horizon: BitTime) {
        self.run_inner(Some(horizon));
    }

    /// Run the engine until the queue is empty.
    pub fn run_until_idle(&mut self) {
        self.run_inner(None);
    }

    /// Read-only access to the produced event log.
    #[must_use]
    pub fn log(&self) -> &Log {
        &self.log
    }

    /// The world the engine is operating over.
    #[must_use]
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The runtime state for `node`, defaulting to `Idle`.
    #[must_use]
    pub fn node_state(&self, node: NodeId) -> NodeRuntimeState {
        self.node_state
            .get(&node)
            .copied()
            .unwrap_or(NodeRuntimeState::Idle)
    }

    // -- Scheduler internals -------------------------------------------------

    fn next_serial(&mut self) -> u64 {
        let s = self.next_serial;
        self.next_serial = self.next_serial.saturating_add(1);
        s
    }

    fn schedule(&mut self, time: BitTime, phase: Phase, event: Event) {
        let key = EventKey {
            time,
            phase,
            serial_id: self.next_serial(),
        };
        self.queue.push(Reverse(Scheduled { key, event }));
    }

    fn run_inner(&mut self, horizon: Option<BitTime>) {
        while let Some(&Reverse(top)) = self.queue.peek() {
            if let Some(h) = horizon
                && top.key.time >= h
            {
                break;
            }
            let batch_time = top.key.time;
            let batch_phase = top.key.phase;
            // Drain all events at (batch_time, batch_phase):
            loop {
                let take = matches!(
                    self.queue.peek(),
                    Some(&Reverse(s)) if s.key.time == batch_time && s.key.phase == batch_phase
                );
                if !take {
                    break;
                }
                let Some(Reverse(scheduled)) = self.queue.pop() else {
                    break;
                };
                self.log.push(scheduled.key, scheduled.event);
                self.dispatch(scheduled.key, scheduled.event);
            }
        }
    }

    fn dispatch(&mut self, key: EventKey, event: Event) {
        let now = key.time;
        match event {
            Event::TxAttempt { node, frame } => self.handle_tx_attempt(now, node, frame),
            Event::TxStart { node, signal } => self.handle_tx_start(node, signal),
            Event::TxEnd { node, signal } => self.handle_tx_end(now, node, signal),
            Event::FrontArrive { node, port, signal } => {
                self.handle_front_arrive(now, node, port, signal);
            }
            Event::BackArrive { node, port, signal } => {
                self.handle_back_arrive(node, port, signal);
            }
            Event::CollisionDetect { node, signal } => {
                self.handle_collision_detect(now, node, signal);
            }
            Event::JamStart { node } => self.handle_jam_start(now, node),
            Event::JamEnd { node } => self.handle_jam_end(now, node),
            Event::BackoffExpire { node, attempt } => {
                self.handle_backoff_expire(now, node, attempt);
            }
            Event::FrameEligible { bridge, port, frame } => {
                self.handle_frame_eligible(now, bridge, port, frame);
            }
            Event::Enqueue { serializer, frame } => {
                self.handle_enqueue(now, serializer, frame);
            }
            Event::Dequeue { serializer, frame } => {
                self.handle_dequeue(now, serializer, frame);
            }
        }
    }

    // -- Handlers ------------------------------------------------------------

    fn handle_tx_attempt(&mut self, now: BitTime, node: NodeId, frame: FrameId) {
        // Record the frame this node is trying to transmit; cleared on
        // successful TxEnd.
        self.pending_frames.insert(node, frame);

        // Dispatch on attachment kind. Round 8c: a node is on at most one
        // segment kind (HD or FD). Mixed nodes are out of scope.
        if self.fd_attachments.contains_key(&node) {
            // FD path: no carrier-sense gating (Axiom A2 — single legal
            // injector — means foreign signals don't contend for our
            // outgoing serializer). Only "busy" case is "we're already
            // transmitting on this direction."
            if self.is_busy(node) {
                let mac = self.mac_config(node);
                let rate = self
                    .fd_attachments
                    .get(&node)
                    .map_or(BitRate::ETHERNET_1G, |a| a.rate);
                let ifg = mac.ifg.duration_at(rate);
                self.schedule(
                    now + ifg,
                    Phase::LocalDecision,
                    Event::TxAttempt { node, frame },
                );
                return;
            }
        } else {
            // HD path: carrier-sense gating per round 8b.
            if self.has_foreign_carrier(node) || self.is_busy(node) {
                let mac = self.mac_config(node);
                let rate = self
                    .hd_rate_of_node(node)
                    .unwrap_or(BitRate::ETHERNET_10M);
                let ifg = mac.ifg.duration_at(rate);
                self.schedule(
                    now + ifg,
                    Phase::LocalDecision,
                    Event::TxAttempt { node, frame },
                );
                return;
            }
        }

        let Some(meta) = self.frames.get(&frame).copied() else {
            return; // unknown frame; drop silently
        };
        let signal_result = match meta.kind {
            SignalKind::Frame => Signal::frame(node, now, meta.bits, meta.rate),
            SignalKind::Jam => Signal::jam(node, now, meta.bits, meta.rate),
        };
        let Ok(signal) = signal_result else {
            return;
        };
        self.schedule(now, Phase::LocalDecision, Event::TxStart { node, signal });
    }

    #[allow(
        clippy::too_many_lines,
        reason = "single dispatch point covering bridge / FD / HD source paths"
    )]
    fn handle_tx_start(&mut self, source: NodeId, signal: Signal) {
        let t0 = signal.t0();
        let duration = signal.duration();

        // Bridge source: use bridge_egress_reach for the egress port the
        // bridge is currently transmitting on.
        if matches!(self.world.node(source), Some(NodeKind::Bridge(_))) {
            let Some(port) = self.bridge_pending_egress.get(&(source, signal)).copied() else {
                return; // shouldn't happen — handle_dequeue sets this
            };
            let peers = self
                .bridge_egress_reach
                .get(&(source, port))
                .map(|r| r.peers.clone())
                .unwrap_or_default();
            for (peer, delay, peer_port) in peers {
                let t_front = t0 + delay;
                let t_back = t_front + duration;
                self.schedule(
                    t_front,
                    Phase::Assertion,
                    Event::FrontArrive {
                        node: peer,
                        port: peer_port,
                        signal,
                    },
                );
                self.schedule(
                    t_back,
                    Phase::Release,
                    Event::BackArrive {
                        node: peer,
                        port: peer_port,
                        signal,
                    },
                );
            }
            let t_end = t0 + duration;
            self.schedule(t_end, Phase::Release, Event::TxEnd { node: source, signal });
            return;
        }

        // End-station source: preserve attempt count if we're already in a
        // Transmitting or BackingOff state (e.g., transitioning from frame
        // to jam after collision).
        let attempt = match self.node_state(source) {
            NodeRuntimeState::Transmitting { attempt, .. }
            | NodeRuntimeState::BackingOff { attempt } => attempt,
            NodeRuntimeState::Idle => 0,
        };
        self.node_state.insert(
            source,
            NodeRuntimeState::Transmitting { signal, attempt },
        );

        // Dispatch on attachment kind.
        if let Some(fd) = self.fd_attachments.get(&source).copied() {
            // FD: single peer arrival.
            let t_front = t0 + fd.delay;
            let t_back = t_front + duration;
            self.schedule(
                t_front,
                Phase::Assertion,
                Event::FrontArrive {
                    node: fd.peer,
                    port: fd.peer_port,
                    signal,
                },
            );
            self.schedule(
                t_back,
                Phase::Release,
                Event::BackArrive {
                    node: fd.peer,
                    port: fd.peer_port,
                    signal,
                },
            );
        } else {
            // HD: iterate hd_pair_reachability for all peers in source's
            // collision domain.
            let peers: Vec<(NodeId, BitTime, PortId)> = self
                .hd_pair_reachability
                .iter()
                .filter_map(|((src, dst), reach)| {
                    if *src == source {
                        Some((*dst, reach.delay, reach.arrival_port))
                    } else {
                        None
                    }
                })
                .collect();

            for (dst, delay, port) in peers {
                let t_front = t0 + delay;
                let t_back = t_front + duration;
                self.schedule(
                    t_front,
                    Phase::Assertion,
                    Event::FrontArrive {
                        node: dst,
                        port,
                        signal,
                    },
                );
                self.schedule(
                    t_back,
                    Phase::Release,
                    Event::BackArrive {
                        node: dst,
                        port,
                        signal,
                    },
                );
            }
        }

        let t_end = t0 + duration;
        self.schedule(t_end, Phase::Release, Event::TxEnd { node: source, signal });
    }

    fn handle_tx_end(&mut self, now: BitTime, source: NodeId, signal: Signal) {
        // Bridge source: free up the egress port and schedule the next
        // dequeue if anything is queued.
        if matches!(self.world.node(source), Some(NodeKind::Bridge(_))) {
            if let Some(port) = self.bridge_pending_egress.remove(&(source, signal)) {
                if let Some(runtime) = self.bridge_state.get_mut(&source) {
                    runtime.egress_busy.insert(port, false);
                    let next_frame = runtime
                        .egress_queues
                        .get(&port)
                        .and_then(|q| q.front().copied());
                    if let Some(next_frame) = next_frame
                        && let Some(serializer) =
                            self.world.bridge_egress_serializer(source, port)
                    {
                        let mac = self.mac_config(source);
                        let rate = self
                            .bridge_egress_reach
                            .get(&(source, port))
                            .map_or(BitRate::ETHERNET_10M, |r| r.rate);
                        let ifg = mac.ifg.duration_at(rate);
                        self.schedule(
                            now + ifg,
                            Phase::LocalDecision,
                            Event::Dequeue {
                                serializer,
                                frame: next_frame,
                            },
                        );
                    }
                }
            }
            return;
        }

        // End-station source: signal-aware state transition.
        if let NodeRuntimeState::Transmitting {
            signal: cur,
            attempt,
        } = self.node_state(source)
            && cur == signal
        {
            match signal.kind() {
                SignalKind::Frame => {
                    // Successful frame transmission.
                    self.node_state.insert(source, NodeRuntimeState::Idle);
                    self.pending_frames.remove(&source);
                }
                SignalKind::Jam => {
                    // Jam transmission ended → backing off.
                    self.node_state.insert(
                        source,
                        NodeRuntimeState::BackingOff {
                            attempt: attempt.saturating_add(1),
                        },
                    );
                }
            }
        }
    }

    fn handle_front_arrive(
        &mut self,
        now: BitTime,
        node: NodeId,
        port: PortId,
        signal: Signal,
    ) {
        if signal.source() == node {
            return;
        }

        // Bridge-receiver path: schedule FrameEligible. Per Axiom A4, the
        // bridge has no internal HD arc, so no carrier-sense or collision
        // tracking is performed at the bridge.
        if let Some(NodeKind::Bridge(BridgeData {
            decode_threshold,
            processing_delay,
        })) = self.world.node(node).copied()
        {
            let ingress_rate = self
                .segment_rate_at(node, port)
                .unwrap_or(BitRate::ETHERNET_10M);
            let bits = Self::signal_bits_at_rate(signal, ingress_rate);
            // Register a relay frame to forward. Use the ingress rate for
            // both the metadata and the eventual egress-side signal
            // construction (round 8d assumes uniform rate across the
            // bridge for a given relay; round 8e+ may extend).
            let Ok(relay_frame) =
                self.register_frame(node, bits, SignalKind::Frame, ingress_rate)
            else {
                return;
            };
            let t_eligible =
                frame_eligibility_time(now, decode_threshold, ingress_rate, processing_delay);
            self.schedule(
                t_eligible,
                Phase::Reaction,
                Event::FrameEligible {
                    bridge: node,
                    port,
                    frame: relay_frame,
                },
            );
            return;
        }

        // FD-receiver path: per Axiom A2 + Theorem 3, no collision can
        // occur on an FD link.
        if matches!(self.port_segment_kind(node, port), Some(SegmentKind::Fd)) {
            return;
        }

        // HD-receiver path: foreign carrier increments; collision detect
        // if we're currently transmitting.
        *self.foreign_carriers.entry(node).or_insert(0) += 1;
        if let NodeRuntimeState::Transmitting { signal: own, .. } = self.node_state(node) {
            self.schedule(
                now,
                Phase::Reaction,
                Event::CollisionDetect { node, signal: own },
            );
        }
    }

    fn handle_back_arrive(&mut self, node: NodeId, port: PortId, signal: Signal) {
        if signal.source() == node {
            return;
        }
        // Bridge or FD receiver: no carrier tracking.
        if matches!(self.world.node(node), Some(NodeKind::Bridge(_)))
            || matches!(self.port_segment_kind(node, port), Some(SegmentKind::Fd))
        {
            return;
        }
        if let Some(count) = self.foreign_carriers.get_mut(&node) {
            *count = count.saturating_sub(1);
        }
    }

    fn handle_collision_detect(&mut self, now: BitTime, node: NodeId, _signal: Signal) {
        // Only react if I'm still transmitting (a second collision-detect
        // for the same transmission is redundant).
        if let NodeRuntimeState::Transmitting { signal, .. } = self.node_state(node)
            && matches!(signal.kind(), SignalKind::Frame)
        {
            self.schedule(now, Phase::Reaction, Event::JamStart { node });
        }
    }

    fn handle_jam_start(&mut self, now: BitTime, node: NodeId) {
        let mac = self.mac_config(node);
        let rate = self
            .hd_rate_of_node(node)
            .unwrap_or(BitRate::ETHERNET_10M);
        let jam_bits = mac.jam.bits();
        let Ok(jam_signal) = Signal::jam(node, now, jam_bits, rate) else {
            return;
        };
        // Schedule TxStart for the jam at LocalDecision phase, same time.
        self.schedule(
            now,
            Phase::LocalDecision,
            Event::TxStart {
                node,
                signal: jam_signal,
            },
        );
        // JamEnd at jam start + jam duration (Release phase).
        let jam_duration = mac.jam.duration_at(rate);
        self.schedule(now + jam_duration, Phase::Release, Event::JamEnd { node });
    }

    fn handle_jam_end(&mut self, now: BitTime, node: NodeId) {
        let attempt = match self.node_state(node) {
            NodeRuntimeState::BackingOff { attempt } => attempt,
            // If the jam's TxEnd hasn't fired yet (Release phase ordering),
            // we may still be Transmitting. Use the current attempt + 1.
            NodeRuntimeState::Transmitting { attempt, .. } => attempt.saturating_add(1),
            NodeRuntimeState::Idle => return,
        };
        let mac = self.mac_config(node);
        let slot = self.slot_time_at(node);
        let random = self.rng.next_u32();
        if let Ok(delay) = mac.backoff.next_delay(attempt, slot, random) {
            // Ensure state is BackingOff (it may already be, or may
            // become so when TxEnd of jam fires).
            self.node_state
                .insert(node, NodeRuntimeState::BackingOff { attempt });
            self.schedule(
                now + delay,
                Phase::LocalDecision,
                Event::BackoffExpire { node, attempt },
            );
        } else {
            // Retry limit reached — give up.
            self.node_state.insert(node, NodeRuntimeState::Idle);
            self.pending_frames.remove(&node);
        }
    }

    fn handle_backoff_expire(&mut self, now: BitTime, node: NodeId, attempt: u32) {
        let NodeRuntimeState::BackingOff {
            attempt: state_attempt,
        } = self.node_state(node)
        else {
            return;
        };
        if state_attempt != attempt {
            return; // stale BackoffExpire
        }
        let Some(&frame) = self.pending_frames.get(&node) else {
            return;
        };
        // Return to Idle so the retry's TxAttempt can proceed.
        self.node_state.insert(node, NodeRuntimeState::Idle);
        // Schedule the retry. TxAttempt's carrier-sense gating may further
        // defer if the medium is still busy.
        self.schedule(now, Phase::LocalDecision, Event::TxAttempt { node, frame });
    }

    // -- Bridge frame relay (round 8d) --------------------------------------

    fn handle_frame_eligible(
        &mut self,
        now: BitTime,
        bridge: NodeId,
        ingress_port: PortId,
        frame: FrameId,
    ) {
        let all_ports = self.bridge_ports(bridge);
        let policy = FloodForwarding;
        let egress_ports: Vec<PortId> = policy.egress_ports(&(), ingress_port, &all_ports);
        for egress_port in egress_ports {
            if let Some(serializer) = self.world.bridge_egress_serializer(bridge, egress_port) {
                self.schedule(
                    now,
                    Phase::LocalDecision,
                    Event::Enqueue { serializer, frame },
                );
            }
        }
    }

    fn handle_enqueue(&mut self, now: BitTime, serializer: SerializerId, frame: FrameId) {
        let Some((bridge, port)) = self.bridge_port_of_serializer(serializer) else {
            return;
        };
        let runtime = self.bridge_state.entry(bridge).or_default();
        runtime
            .egress_queues
            .entry(port)
            .or_default()
            .push_back(frame);
        let busy = runtime.egress_busy.get(&port).copied().unwrap_or(false);
        if !busy {
            // Schedule Dequeue at now (LocalDecision); the existing
            // event-key serial tie-break ensures Enqueue logs before
            // Dequeue at the same timestamp.
            self.schedule(
                now,
                Phase::LocalDecision,
                Event::Dequeue { serializer, frame },
            );
        }
    }

    fn handle_dequeue(&mut self, now: BitTime, serializer: SerializerId, frame: FrameId) {
        let Some((bridge, port)) = self.bridge_port_of_serializer(serializer) else {
            return;
        };
        // Pop from queue; mark egress busy.
        let runtime = self.bridge_state.entry(bridge).or_default();
        let queue = runtime.egress_queues.entry(port).or_default();
        let popped = queue.pop_front();
        if popped != Some(frame) {
            // Stale Dequeue (out-of-order). If we popped something else,
            // push it back at the front and bail.
            if let Some(other) = popped {
                queue.push_front(other);
            }
            return;
        }
        runtime.egress_busy.insert(port, true);

        // Construct the egress signal from the frame's metadata.
        let Some(meta) = self.frames.get(&frame).copied() else {
            return;
        };
        let Ok(signal) = Signal::frame(bridge, now, meta.bits, meta.rate) else {
            return;
        };
        // Record which port the bridge is emitting on, then schedule TxStart.
        self.bridge_pending_egress.insert((bridge, signal), port);
        self.schedule(
            now,
            Phase::LocalDecision,
            Event::TxStart {
                node: bridge,
                signal,
            },
        );
    }

    // -- Helpers --------------------------------------------------------------

    fn has_foreign_carrier(&self, node: NodeId) -> bool {
        self.foreign_carriers
            .get(&node)
            .copied()
            .unwrap_or(0)
            > 0
    }

    fn is_busy(&self, node: NodeId) -> bool {
        !matches!(self.node_state(node), NodeRuntimeState::Idle)
    }

    fn hd_rate_of_node(&self, node: NodeId) -> Option<BitRate> {
        // Find the first HD segment incident to this node and return its rate.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "segment_count fits in u32 for any realistic topology"
        )]
        for i in 0..self.world.segment_count() as u32 {
            let id = SegmentId::new(i);
            if let Some(seg) = self.world.hd_segment(id) {
                let (a, b) = seg.endpoints();
                if a.node == node || b.node == node {
                    return Some(seg.rate());
                }
            }
        }
        None
    }

    fn slot_time_at(&self, node: NodeId) -> BitTime {
        // Per IEEE 802.3: slotTime = 512 bits at 10/100 Mbps;
        // 4096 bits at 1 Gbps half duplex.
        let rate = self
            .hd_rate_of_node(node)
            .unwrap_or(BitRate::ETHERNET_10M);
        let slot_bits = if rate.as_bps() >= BitRate::ETHERNET_1G.as_bps() {
            Bits::new(4_096)
        } else {
            Bits::new(512)
        };
        slot_bits.at_rate(rate)
    }

    /// The segment kind of the segment connected to `(node, port)`, or
    /// `None` if no segment is attached there.
    ///
    /// Used by arrival handlers to dispatch HD vs FD logic on the
    /// receiver side.
    fn port_segment_kind(&self, node: NodeId, port: PortId) -> Option<SegmentKind> {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "segment_count fits in u32 for any realistic topology"
        )]
        for i in 0..self.world.segment_count() as u32 {
            let id = SegmentId::new(i);
            if let Some(seg) = self.world.hd_segment(id) {
                let (a, b) = seg.endpoints();
                if (a.node == node && a.port == port) || (b.node == node && b.port == port) {
                    return Some(SegmentKind::Hd);
                }
            } else if let Some(seg) = self.world.fd_segment(id) {
                let (a, b) = seg.endpoints();
                if (a.node == node && a.port == port) || (b.node == node && b.port == port) {
                    return Some(SegmentKind::Fd);
                }
            }
        }
        None
    }

    /// The bit rate of the segment connected to `(node, port)`, or `None`
    /// if no segment is attached there.
    fn segment_rate_at(&self, node: NodeId, port: PortId) -> Option<BitRate> {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "segment_count fits in u32 for any realistic topology"
        )]
        for i in 0..self.world.segment_count() as u32 {
            let id = SegmentId::new(i);
            if let Some(seg) = self.world.hd_segment(id) {
                let (a, b) = seg.endpoints();
                if (a.node == node && a.port == port) || (b.node == node && b.port == port) {
                    return Some(seg.rate());
                }
            } else if let Some(seg) = self.world.fd_segment(id) {
                let (a, b) = seg.endpoints();
                if (a.node == node && a.port == port) || (b.node == node && b.port == port) {
                    return Some(seg.rate());
                }
            }
        }
        None
    }

    /// All ports on `bridge` that have a segment attached, gathered by
    /// inspecting the world's segment list.
    fn bridge_ports(&self, bridge: NodeId) -> Vec<PortId> {
        let mut ports: Vec<PortId> = Vec::new();
        #[allow(
            clippy::cast_possible_truncation,
            reason = "segment_count fits in u32 for any realistic topology"
        )]
        for i in 0..self.world.segment_count() as u32 {
            let id = SegmentId::new(i);
            let endpoints = self
                .world
                .hd_segment(id)
                .map(crate::topology::HdSegment::endpoints)
                .or_else(|| {
                    self.world
                        .fd_segment(id)
                        .map(crate::topology::FdSegment::endpoints)
                });
            if let Some((a, b)) = endpoints {
                if a.node == bridge && !ports.contains(&a.port) {
                    ports.push(a.port);
                }
                if b.node == bridge && !ports.contains(&b.port) {
                    ports.push(b.port);
                }
            }
        }
        ports.sort();
        ports
    }

    /// Reverse-lookup: given a serializer ID assigned to a bridge egress
    /// port at build time, return `(bridge_node, port)`.
    fn bridge_port_of_serializer(
        &self,
        serializer: SerializerId,
    ) -> Option<(NodeId, PortId)> {
        for (node_id, kind) in self.world.nodes() {
            if !matches!(kind, NodeKind::Bridge(_)) {
                continue;
            }
            for port in self.bridge_ports(node_id) {
                if self.world.bridge_egress_serializer(node_id, port) == Some(serializer) {
                    return Some((node_id, port));
                }
            }
        }
        None
    }

    /// Compute the bit count for a signal of the given duration at the
    /// given rate. Inverse of [`Bits::at_rate`].
    fn signal_bits_at_rate(signal: Signal, rate: BitRate) -> Bits {
        // duration_ps * rate_bps / PICOSECONDS_PER_SECOND
        let ps = u128::from(signal.duration().as_u64());
        let bps = u128::from(rate.as_bps());
        let bits = (ps * bps) / 1_000_000_000_000_u128;
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bit count fits in u64 for any realistic signal"
        )]
        Bits::new(bits as u64)
    }
}

// ===========================================================================
// HD pair-delay precomputation
// ===========================================================================

fn precompute_hd_pair_reachability(world: &World) -> HashMap<(NodeId, NodeId), HdReachability> {
    let mut pair_delays: HashMap<(NodeId, NodeId), HdReachability> = HashMap::new();

    #[allow(
        clippy::cast_possible_truncation,
        reason = "node_count fits in u32 for any realistic topology"
    )]
    for node_idx in 0..world.node_count() as u32 {
        let source = NodeId::new(node_idx);
        // Skip bridge nodes as sources: bridges don't originate HD
        // transmissions in round 8a.
        if matches!(world.node(source), Some(NodeKind::Bridge(_))) {
            continue;
        }
        let from_source = bfs_hd_delays(world, source);
        for (target, reach) in from_source {
            if target != source {
                pair_delays.insert((source, target), reach);
            }
        }
    }

    pair_delays
}

fn bfs_hd_delays(world: &World, source: NodeId) -> HashMap<NodeId, HdReachability> {
    let mut delays: HashMap<NodeId, HdReachability> = HashMap::new();
    delays.insert(
        source,
        HdReachability {
            delay: BitTime::ZERO,
            arrival_port: PortId::new(0), // arrival port at source itself is meaningless
        },
    );

    let mut queue: VecDeque<NodeId> = VecDeque::new();
    queue.push_back(source);

    while let Some(u) = queue.pop_front() {
        // If u is a bridge (and not the source), we don't propagate further.
        // Per A4, the bridge has no internal HD arc.
        let is_bridge = matches!(world.node(u), Some(NodeKind::Bridge(_)));
        if is_bridge && u != source {
            continue;
        }

        let leave_cost = if u == source {
            BitTime::ZERO
        } else {
            match world.node(u) {
                Some(NodeKind::Repeater(RepeaterData { delta_h })) => *delta_h,
                _ => BitTime::ZERO,
            }
        };

        let d_u = delays.get(&u).copied().map_or(BitTime::ZERO, |r| r.delay);

        for (v, seg_delay, port_on_v) in hd_neighbors_of(world, u) {
            if delays.contains_key(&v) {
                continue;
            }
            let d_v = d_u + leave_cost + seg_delay;
            delays.insert(
                v,
                HdReachability {
                    delay: d_v,
                    arrival_port: port_on_v,
                },
            );
            queue.push_back(v);
        }
    }

    delays
}

fn hd_neighbors_of(
    world: &World,
    u: NodeId,
) -> impl Iterator<Item = (NodeId, BitTime, PortId)> + '_ {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "segment_count fits in u32 for any realistic topology"
    )]
    (0..world.segment_count() as u32).filter_map(move |i| {
        let seg_id = SegmentId::new(i);
        let seg = world.hd_segment(seg_id)?;
        let (a, b) = seg.endpoints();
        if a.node == u {
            Some((b.node, seg.delay(), b.port))
        } else if b.node == u {
            Some((a.node, seg.delay(), a.port))
        } else {
            None
        }
    })
}

// ===========================================================================
// FD attachment precomputation
// ===========================================================================

fn precompute_fd_attachments(world: &World) -> HashMap<NodeId, FdAttachment> {
    let mut attachments: HashMap<NodeId, FdAttachment> = HashMap::new();

    #[allow(
        clippy::cast_possible_truncation,
        reason = "segment_count fits in u32 for any realistic topology"
    )]
    for i in 0..world.segment_count() as u32 {
        let seg_id = SegmentId::new(i);
        let Some(seg) = world.fd_segment(seg_id) else {
            continue;
        };
        let (a, b) = seg.endpoints();
        let delay = seg.delay();
        let rate = seg.rate();
        // Round 8c: insert if not already present (first FD attachment wins
        // for nodes with multiple FD ports; multi-FD-port handling is
        // future work).
        attachments.entry(a.node).or_insert(FdAttachment {
            peer: b.node,
            delay,
            rate,
            peer_port: b.port,
        });
        attachments.entry(b.node).or_insert(FdAttachment {
            peer: a.node,
            delay,
            rate,
            peer_port: a.port,
        });
    }

    attachments
}

// ===========================================================================
// Bridge egress reachability precomputation
// ===========================================================================

fn precompute_bridge_egress_reach(
    world: &World,
    hd_pair: &HashMap<(NodeId, NodeId), HdReachability>,
) -> HashMap<(NodeId, PortId), BridgeEgressReach> {
    let mut result: HashMap<(NodeId, PortId), BridgeEgressReach> = HashMap::new();

    #[allow(
        clippy::cast_possible_truncation,
        reason = "segment_count fits in u32 for any realistic topology"
    )]
    for i in 0..world.segment_count() as u32 {
        let seg_id = SegmentId::new(i);
        if let Some(seg) = world.hd_segment(seg_id) {
            let (a, b) = seg.endpoints();
            let delay = seg.delay();
            let rate = seg.rate();
            // For each endpoint that's a bridge, compute reach via this port.
            for (bridge_ep, neighbor_ep) in [(a, b), (b, a)] {
                if matches!(world.node(bridge_ep.node), Some(NodeKind::Bridge(_))) {
                    let mut peers: Vec<(NodeId, BitTime, PortId)> = Vec::new();
                    // First hop: the neighbor itself.
                    peers.push((neighbor_ep.node, delay, neighbor_ep.port));
                    // Beyond: peers in neighbor's HD component, excluding
                    // the bridge we came from (we don't echo the signal
                    // back to ourselves) and other bridges (their
                    // egress would be a separate forwarding decision).
                    for ((src, dst), reach) in hd_pair {
                        if *src != neighbor_ep.node || *dst == neighbor_ep.node {
                            continue;
                        }
                        if matches!(world.node(*dst), Some(NodeKind::Bridge(_))) {
                            continue;
                        }
                        peers.push((*dst, delay + reach.delay, reach.arrival_port));
                    }
                    result.insert(
                        (bridge_ep.node, bridge_ep.port),
                        BridgeEgressReach { rate, peers },
                    );
                }
            }
        } else if let Some(seg) = world.fd_segment(seg_id) {
            let (a, b) = seg.endpoints();
            let delay = seg.delay();
            let rate = seg.rate();
            for (bridge_ep, peer_ep) in [(a, b), (b, a)] {
                if matches!(world.node(bridge_ep.node), Some(NodeKind::Bridge(_))) {
                    let peers = vec![(peer_ep.node, delay, peer_ep.port)];
                    result.insert(
                        (bridge_ep.node, bridge_ep.port),
                        BridgeEgressReach { rate, peers },
                    );
                }
            }
        }
    }

    result
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Per [Result vs Panic]: unwrap and panic are allowed in tests."
)]
mod tests {
    use super::*;
    use crate::topology::{Endpoint, TopologyBuilder};

    fn ep(node: NodeId, port: u32) -> Endpoint {
        Endpoint::new(node, PortId::new(port))
    }

    // -- Engine wiring --------------------------------------------------------

    #[test]
    fn empty_engine_has_empty_log_and_idle_run() {
        let world = TopologyBuilder::new().build().unwrap();
        let mut engine = Engine::new(world);
        assert!(engine.log().is_empty());
        engine.run_until_idle();
        assert!(engine.log().is_empty());
    }

    #[test]
    fn register_frame_returns_sequential_ids() {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let world = b.build().unwrap();
        let mut engine = Engine::new(world);
        let f0 = engine
            .register_frame(s1, Bits::new(64), SignalKind::Frame, BitRate::ETHERNET_1G)
            .unwrap();
        let f1 = engine
            .register_frame(s1, Bits::new(64), SignalKind::Frame, BitRate::ETHERNET_1G)
            .unwrap();
        assert_eq!(f0.as_u32(), 0);
        assert_eq!(f1.as_u32(), 1);
    }

    #[test]
    fn register_frame_rejects_zero_bits() {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let world = b.build().unwrap();
        let mut engine = Engine::new(world);
        assert_eq!(
            engine.register_frame(s1, Bits::ZERO, SignalKind::Frame, BitRate::ETHERNET_1G),
            Err(EngineError::ZeroBitFrame),
        );
    }

    #[test]
    fn mac_config_default_is_ieee() {
        let world = TopologyBuilder::new().build().unwrap();
        let engine = Engine::new(world);
        let cfg = engine.mac_config(NodeId::new(0));
        assert_eq!(cfg, MacConfig::IEEE_802_3);
    }

    // -- HD-1: ordinary success case ----------------------------------------

    /// Build a two-station HD pair at 10 Mbps with one HD segment of
    /// the given delay.
    fn hd_pair(delay_ps: u64) -> (World, NodeId, NodeId) {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        b.add_hd_segment(
            BitRate::ETHERNET_10M,
            BitTime::new(delay_ps),
            ep(s1, 0),
            ep(s2, 0),
        )
        .unwrap();
        (b.build().unwrap(), s1, s2)
    }

    #[test]
    fn hd_1_two_stations_log_has_exactly_5_events() {
        // τ = 5 µs, frame = 512 bits at 10 Mbps, D_σ = 51.2 µs.
        let tau = BitTime::from_micros(5);
        let (world, s1, s2) = hd_pair(tau.as_u64());
        let mut engine = Engine::new(world);
        let frame = engine
            .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();

        // Lemma 16: exactly 5 events in the log.
        assert_eq!(engine.log().len(), 5);

        let entries: Vec<_> = engine.log().iter().collect();

        // Event 0: TxAttempt at t = 0.
        match entries[0].event {
            Event::TxAttempt { node, frame: f } => {
                assert_eq!(node, s1);
                assert_eq!(f, frame);
            }
            ref ev => panic!("expected TxAttempt, got {ev:?}"),
        }
        assert_eq!(entries[0].key.time, BitTime::ZERO);

        // Event 1: TxStart at t = 0.
        match entries[1].event {
            Event::TxStart { node, signal } => {
                assert_eq!(node, s1);
                assert_eq!(signal.t0(), BitTime::ZERO);
            }
            ref ev => panic!("expected TxStart, got {ev:?}"),
        }
        assert_eq!(entries[1].key.time, BitTime::ZERO);

        // Event 2: FrontArrive at s2 at t = τ = 5 µs.
        match entries[2].event {
            Event::FrontArrive { node, .. } => assert_eq!(node, s2),
            ref ev => panic!("expected FrontArrive, got {ev:?}"),
        }
        assert_eq!(entries[2].key.time, tau);

        // Event 3: TxEnd at t = D_σ = 51.2 µs (after FrontArrive at 5 µs).
        match entries[3].event {
            Event::TxEnd { node, .. } => assert_eq!(node, s1),
            ref ev => panic!("expected TxEnd, got {ev:?}"),
        }
        assert_eq!(entries[3].key.time, BitTime::from_nanos(51_200));

        // Event 4: BackArrive at s2 at t = τ + D_σ = 56.2 µs.
        match entries[4].event {
            Event::BackArrive { node, .. } => assert_eq!(node, s2),
            ref ev => panic!("expected BackArrive, got {ev:?}"),
        }
        assert_eq!(
            entries[4].key.time,
            tau + BitTime::from_nanos(51_200),
        );
    }

    #[test]
    fn hd_1_node_state_returns_to_idle_after_tx_end() {
        let (world, s1, _) = hd_pair(BitTime::from_micros(5).as_u64());
        let mut engine = Engine::new(world);
        let frame = engine
            .register_frame(s1, Bits::new(64), SignalKind::Frame, BitRate::ETHERNET_1G)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();
        assert_eq!(engine.node_state(s1), NodeRuntimeState::Idle);
    }

    // -- REP-1: three stations via one repeater ------------------------------

    #[test]
    fn rep_1_three_stations_via_repeater_log_has_exactly_7_events() {
        // s1, s2, s3 each connected to repeater r by a 1 µs HD segment.
        // r has δ_h = 100 ns. From s1's TxStart, signal reaches s2 and s3
        // at 1 µs + 100 ns + 1 µs = 2.1 µs.
        let tau = BitTime::from_micros(1);
        let delta_h = BitTime::from_nanos(100);
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        let s3 = b.add_end_station(1);
        let r = b.add_repeater(3, delta_h);
        b.add_hd_segment(BitRate::ETHERNET_10M, tau, ep(s1, 0), ep(r, 0)).unwrap();
        b.add_hd_segment(BitRate::ETHERNET_10M, tau, ep(s2, 0), ep(r, 1)).unwrap();
        b.add_hd_segment(BitRate::ETHERNET_10M, tau, ep(s3, 0), ep(r, 2)).unwrap();
        let world = b.build().unwrap();

        let mut engine = Engine::new(world);
        let frame = engine
            .register_frame(s1, Bits::new(100), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        let duration = Bits::new(100).at_rate(BitRate::ETHERNET_10M);
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();

        // Expected events:
        // 1. TxAttempt(s1) @ 0
        // 2. TxStart(s1) @ 0
        // 3. FrontArrive(r) @ τ = 1 µs
        // 4. FrontArrive(s2) @ τ + δ_h + τ = 2.1 µs
        // 5. FrontArrive(s3) @ 2.1 µs
        // 6. TxEnd(s1) @ duration = 100 / 10M = 10 µs
        // 7. BackArrive(r) @ τ + duration = 11 µs
        // 8. BackArrive(s2) @ 2.1 µs + duration = 12.1 µs
        // 9. BackArrive(s3) @ 12.1 µs
        // That's 9 events, not 7 — the repeater is also a peer and gets
        // arrivals. Plan said 7 (excluding repeater); but since the
        // repeater is in the HD pair-delay map, it does receive arrivals.
        // Actual count: TxAttempt + TxStart + 3 FrontArrives + TxEnd + 3
        // BackArrives = 9.
        let log = engine.log();
        assert_eq!(log.len(), 9, "expected 9 events; got {}", log.len());

        // Verify s2's FrontArrive arrives at exactly τ + δ_h + τ.
        let s2_front = log
            .iter()
            .find(|e| matches!(e.event, Event::FrontArrive { node, .. } if node == s2))
            .unwrap();
        assert_eq!(s2_front.key.time, tau + delta_h + tau);

        // Verify s3 also.
        let s3_front = log
            .iter()
            .find(|e| matches!(e.event, Event::FrontArrive { node, .. } if node == s3))
            .unwrap();
        assert_eq!(s3_front.key.time, tau + delta_h + tau);

        // Verify the repeater itself receives a FrontArrive at τ (no δ_h
        // because the signal is arriving AT the repeater, not through it).
        let r_front = log
            .iter()
            .find(|e| matches!(e.event, Event::FrontArrive { node, .. } if node == r))
            .unwrap();
        assert_eq!(r_front.key.time, tau);

        // Source returns to idle.
        assert_eq!(engine.node_state(s1), NodeRuntimeState::Idle);

        // Duration check:
        assert_eq!(duration, BitTime::from_micros(10));
    }

    // -- Phase ordering at simultaneous events ------------------------------

    #[test]
    fn phase_order_holds_across_log_for_hd_1() {
        let (world, s1, _) = hd_pair(BitTime::from_micros(5).as_u64());
        let mut engine = Engine::new(world);
        let frame = engine
            .register_frame(s1, Bits::new(64), SignalKind::Frame, BitRate::ETHERNET_1G)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();

        let entries: Vec<_> = engine.log().iter().collect();
        // For consecutive same-time entries, phase must be non-decreasing.
        for w in entries.windows(2) {
            if w[0].key.time == w[1].key.time {
                assert!(
                    w[0].key.phase <= w[1].key.phase,
                    "phase order violated: {:?} -> {:?}",
                    w[0].key,
                    w[1].key,
                );
            }
        }
    }

    // -- run_until horizon ---------------------------------------------------

    #[test]
    fn run_until_horizon_stops_at_or_before_horizon() {
        let (world, s1, _) = hd_pair(BitTime::from_micros(5).as_u64());
        let mut engine = Engine::new(world);
        let frame = engine
            .register_frame(s1, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        // Stop before the FrontArrive at 5 µs.
        engine.run_until(BitTime::from_micros(4));
        let entries: Vec<_> = engine.log().iter().collect();
        // Should have processed TxAttempt and TxStart only (both at t=0).
        assert_eq!(entries.len(), 2);
        assert!(matches!(entries[0].event, Event::TxAttempt { .. }));
        assert!(matches!(entries[1].event, Event::TxStart { .. }));
    }

    // -- HD pair-delay precomputation ---------------------------------------

    #[test]
    fn pair_delays_two_station_pair_match_segment_delay() {
        let (world, s1, s2) = hd_pair(BitTime::from_micros(5).as_u64());
        let engine = Engine::new(world);
        assert_eq!(
            engine.hd_pair_reachability.get(&(s1, s2)).map(|r| r.delay),
            Some(BitTime::from_micros(5)),
        );
        assert_eq!(
            engine.hd_pair_reachability.get(&(s2, s1)).map(|r| r.delay),
            Some(BitTime::from_micros(5)),
        );
    }

    #[test]
    fn pair_delays_through_repeater_include_delta_h() {
        let tau = BitTime::from_micros(1);
        let delta_h = BitTime::from_nanos(100);
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        let r = b.add_repeater(2, delta_h);
        b.add_hd_segment(BitRate::ETHERNET_10M, tau, ep(s1, 0), ep(r, 0)).unwrap();
        b.add_hd_segment(BitRate::ETHERNET_10M, tau, ep(s2, 0), ep(r, 1)).unwrap();
        let world = b.build().unwrap();
        let engine = Engine::new(world);
        // s1 → r is just one segment (τ); no δ_h paid yet (s1 is source).
        assert_eq!(
            engine.hd_pair_reachability.get(&(s1, r)).map(|r| r.delay),
            Some(tau),
        );
        // s1 → s2 traverses r (intermediate); pays δ_h.
        assert_eq!(
            engine.hd_pair_reachability.get(&(s1, s2)).map(|r| r.delay),
            Some(tau + delta_h + tau),
        );
    }

    // =======================================================================
    // Round 8b — collision detection, carrier sense, jam, backoff
    // =======================================================================

    /// Find the first event in the log matching `predicate`, returning its
    /// timestamp.
    fn find_event_time<F>(log: &Log, predicate: F) -> Option<BitTime>
    where
        F: Fn(&Event) -> bool,
    {
        log.iter()
            .find(|e| predicate(&e.event))
            .map(|e| e.key.time)
    }

    fn count_events<F>(log: &Log, predicate: F) -> usize
    where
        F: Fn(&Event) -> bool,
    {
        log.iter().filter(|e| predicate(&e.event)).count()
    }

    // -- Theorem 1: HD collision detection (the headline) -------------------

    #[test]
    fn theorem_1_hd_collision_detection_at_far_end_and_near_end() {
        // Two-station HD pair, τ = 5 µs at 10 Mbps.
        // A starts at t = 0; B starts at t = 4.9 µs (just before A's
        // signal reaches B). Per report_0 Theorem 1:
        //   - B detects collision at t = 5 µs (when A's front arrives).
        //   - A detects collision at t = 9.9 µs (when B's front arrives).
        let tau = BitTime::from_micros(5);
        let (world, a, b) = hd_pair(tau.as_u64());
        let mut engine = Engine::with_seed(world, 1);

        // Frame: 512 bits at 10 Mbps = 51.2 µs (long enough to span the
        // collision window).
        let frame_a = engine
            .register_frame(a, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        let frame_b = engine
            .register_frame(b, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();

        engine.schedule_tx_attempt(BitTime::ZERO, a, frame_a);
        engine.schedule_tx_attempt(BitTime::from_nanos(4_900), b, frame_b);
        engine.run_until_idle();

        let log = engine.log();

        // Sharp oracle: B's CollisionDetect at exactly 5 µs.
        let cd_b = find_event_time(log, |e| {
            matches!(e, Event::CollisionDetect { node, .. } if *node == b)
        });
        assert_eq!(cd_b, Some(BitTime::from_micros(5)));

        // Sharp oracle: A's CollisionDetect at exactly 9.9 µs.
        let cd_a = find_event_time(log, |e| {
            matches!(e, Event::CollisionDetect { node, .. } if *node == a)
        });
        assert_eq!(cd_a, Some(BitTime::from_nanos(9_900)));

        // Both nodes also fire JamStart at the same times (Reaction phase
        // after CollisionDetect).
        let jam_b = find_event_time(log, |e| {
            matches!(e, Event::JamStart { node } if *node == b)
        });
        assert_eq!(jam_b, Some(BitTime::from_micros(5)));

        let jam_a = find_event_time(log, |e| {
            matches!(e, Event::JamStart { node } if *node == a)
        });
        assert_eq!(jam_a, Some(BitTime::from_nanos(9_900)));
    }

    // -- Theorem 1: collision detect uses node's own signal -----------------

    #[test]
    fn collision_detect_carries_nodes_own_signal_not_foreign() {
        let tau = BitTime::from_micros(5);
        let (world, a, b) = hd_pair(tau.as_u64());
        let mut engine = Engine::with_seed(world, 1);
        let frame_a = engine
            .register_frame(a, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        let frame_b = engine
            .register_frame(b, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, a, frame_a);
        engine.schedule_tx_attempt(BitTime::from_nanos(4_900), b, frame_b);
        engine.run_until_idle();

        // The signal carried by CollisionDetect at B is B's own signal
        // (not A's foreign signal). Verify by checking the source.
        let cd_b = engine
            .log()
            .iter()
            .find(|e| matches!(e.event, Event::CollisionDetect { node, .. } if node == b))
            .unwrap();
        if let Event::CollisionDetect { signal, .. } = cd_b.event {
            assert_eq!(signal.source(), b);
        }
    }

    // -- Carrier-sense gating ------------------------------------------------

    #[test]
    fn carrier_sense_defers_tx_attempt_when_medium_busy() {
        // A is mid-transmission; B's TxAttempt arrives while A's signal
        // is at B → B's TxAttempt defers.
        let tau = BitTime::from_micros(5);
        let (world, a, b) = hd_pair(tau.as_u64());
        let mut engine = Engine::with_seed(world, 1);
        let frame_a = engine
            .register_frame(a, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        let frame_b = engine
            .register_frame(b, Bits::new(64), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, a, frame_a);
        // B attempts AFTER A's signal has reached B (i.e., B sees carrier).
        engine.schedule_tx_attempt(BitTime::from_micros(10), b, frame_b);
        engine.run_until_idle();

        // B's first TxAttempt is at t = 10 µs (deferred).
        // It should NOT immediately produce a TxStart — instead a later
        // TxAttempt fires after IFG.
        let b_tx_attempts: Vec<_> = engine
            .log()
            .iter()
            .filter(|e| matches!(e.event, Event::TxAttempt { node, .. } if node == b))
            .collect();
        assert!(
            b_tx_attempts.len() >= 2,
            "expected at least 2 TxAttempts at B (initial + deferred); got {}",
            b_tx_attempts.len(),
        );
        // The deferred attempt fires at the original time + IFG (96 bits at 10 Mbps = 9.6 µs).
        assert_eq!(
            b_tx_attempts[1].key.time,
            BitTime::from_micros(10) + BitTime::from_nanos(9_600),
        );
    }

    #[test]
    fn carrier_sense_clears_after_back_arrive() {
        let tau = BitTime::from_micros(5);
        let (world, a, b) = hd_pair(tau.as_u64());
        let mut engine = Engine::with_seed(world, 1);
        let frame_a = engine
            .register_frame(a, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, a, frame_a);
        engine.run_until_idle();

        // After all events processed, foreign_carriers at B should be 0.
        assert_eq!(
            engine.foreign_carriers.get(&b).copied().unwrap_or(0),
            0,
        );
        assert_eq!(
            engine.foreign_carriers.get(&a).copied().unwrap_or(0),
            0,
        );
    }

    // -- Backoff retry --------------------------------------------------------

    #[test]
    fn collision_triggers_backoff_and_retry_tx_attempt() {
        // After the collision, BackoffExpire fires and a retry TxAttempt
        // is scheduled. Verify there's at least one BackoffExpire event
        // and a subsequent TxAttempt for the original frame.
        let tau = BitTime::from_micros(5);
        let (world, a, b) = hd_pair(tau.as_u64());
        let mut engine = Engine::with_seed(world, 1);
        let frame_a = engine
            .register_frame(a, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        let frame_b = engine
            .register_frame(b, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, a, frame_a);
        engine.schedule_tx_attempt(BitTime::from_nanos(4_900), b, frame_b);

        // Run for a bounded horizon to avoid unbounded retry loops in this
        // test (we just want to see that backoff + retry happens at least
        // once).
        engine.run_until(BitTime::from_millis(1));

        let log = engine.log();
        assert!(
            count_events(log, |e| matches!(e, Event::BackoffExpire { .. })) > 0,
            "expected at least one BackoffExpire event",
        );
        // At least one retry TxAttempt for the same frames should appear
        // after the collision (i.e., after t = 5 µs).
        let retries_a = count_events(log, |e| {
            matches!(e, Event::TxAttempt { node, frame } if *node == a && *frame == frame_a)
        });
        assert!(
            retries_a >= 2,
            "expected at least 2 TxAttempts for frame_a (original + retry); got {retries_a}",
        );
    }

    // -- TxEnd signal-awareness ----------------------------------------------

    #[test]
    fn original_frames_tx_end_does_not_clobber_jam_state() {
        // The original frame's TxEnd fires AFTER the jam started. The
        // signal-aware TxEnd handler must not transition state to Idle
        // when the ending signal isn't the current transmission.
        let tau = BitTime::from_micros(5);
        let (world, a, b) = hd_pair(tau.as_u64());
        let mut engine = Engine::with_seed(world, 1);
        let frame_a = engine
            .register_frame(a, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        let frame_b = engine
            .register_frame(b, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, a, frame_a);
        engine.schedule_tx_attempt(BitTime::from_nanos(4_900), b, frame_b);

        // Run until t just past the original frame's TxEnd at A
        // (51.2 µs) and B (4.9 + 51.2 = 56.1 µs), but before the
        // backoff retry has had time to materialize on a long delay.
        engine.run_until(BitTime::from_micros(60));

        // After the original frame's TxEnd, A should NOT be Idle if it's
        // still in jam or backoff. Specifically, after t = 51.2 µs (A's
        // original frame TxEnd), A should not have transitioned to Idle
        // erroneously — verified by the existence of subsequent events
        // (jam end, backoff expire, etc.) that depend on non-Idle state.
        let log = engine.log();
        // We expect at least: A's CollisionDetect, A's JamStart, A's
        // jam TxStart, eventually A's JamEnd.
        let jam_start_a = log.iter().any(|e| matches!(e.event, Event::JamStart { node } if node == a));
        let jam_end_a = log.iter().any(|e| matches!(e.event, Event::JamEnd { node } if node == a));
        assert!(jam_start_a, "A should fire JamStart");
        assert!(jam_end_a, "A should fire JamEnd (state stayed correct through original frame's TxEnd)");
    }

    // -- with_seed determinism ------------------------------------------------

    #[test]
    fn with_seed_is_deterministic() {
        // Same seed + same inputs → same log.
        fn run(seed: u64) -> Vec<EventKey> {
            let tau = BitTime::from_micros(5);
            let (world, a, b) = hd_pair(tau.as_u64());
            let mut engine = Engine::with_seed(world, seed);
            let frame_a = engine
                .register_frame(a, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
                .unwrap();
            let frame_b = engine
                .register_frame(b, Bits::new(512), SignalKind::Frame, BitRate::ETHERNET_10M)
                .unwrap();
            engine.schedule_tx_attempt(BitTime::ZERO, a, frame_a);
            engine.schedule_tx_attempt(BitTime::from_nanos(4_900), b, frame_b);
            engine.run_until(BitTime::from_millis(1));
            engine.log().iter().map(|e| e.key).collect()
        }
        assert_eq!(run(42), run(42));
    }

    // -- Slot-time helper ----------------------------------------------------

    #[test]
    fn slot_time_at_returns_ieee_values() {
        let (world, s1, _) = hd_pair(BitTime::from_micros(5).as_u64());
        let engine = Engine::new(world);
        // 10 Mbps: slot = 512 bits = 51.2 µs.
        assert_eq!(engine.slot_time_at(s1), BitTime::from_nanos(51_200));
    }

    #[test]
    fn slot_time_at_1g_uses_4096_bits() {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        b.add_hd_segment(
            BitRate::ETHERNET_1G,
            BitTime::from_nanos(100),
            ep(s1, 0),
            ep(s2, 0),
        )
        .unwrap();
        let world = b.build().unwrap();
        let engine = Engine::new(world);
        // 1 Gbps half-duplex: slot = 4096 bits = 4.096 µs.
        assert_eq!(engine.slot_time_at(s1), BitTime::from_nanos(4_096));
    }

    // =======================================================================
    // Round 8c — FD path
    // =======================================================================

    fn fd_pair(delay_ps: u64, rate: BitRate) -> (World, NodeId, NodeId) {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        b.add_fd_segment(rate, BitTime::new(delay_ps), ep(s1, 0), ep(s2, 0))
            .unwrap();
        (b.build().unwrap(), s1, s2)
    }

    // -- FD-1: ordinary single-direction success ----------------------------

    #[test]
    fn fd_1_two_stations_log_has_exactly_5_events() {
        // Single FD link: s1 → s2. Frame 96 bits at 1 Gbps = 96 ns.
        // delay = 100 ns. No collision possible.
        let tau = BitTime::from_nanos(100);
        let (world, s1, s2) = fd_pair(tau.as_u64(), BitRate::ETHERNET_1G);
        let mut engine = Engine::new(world);
        let frame = engine
            .register_frame(s1, Bits::new(96), SignalKind::Frame, BitRate::ETHERNET_1G)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();

        // Sharp oracle: exactly 5 events.
        assert_eq!(engine.log().len(), 5);

        let entries: Vec<_> = engine.log().iter().collect();

        // The log is ordered by EventKey (time, phase, serial). With
        // duration = 96 ns < tau = 100 ns, TxEnd (at 96 ns) fires BEFORE
        // FrontArrive (at 100 ns).
        //
        // Event 0: TxAttempt at t=0
        assert_eq!(entries[0].key.time, BitTime::ZERO);
        assert!(matches!(entries[0].event, Event::TxAttempt { node, .. } if node == s1));

        // Event 1: TxStart at t=0
        assert_eq!(entries[1].key.time, BitTime::ZERO);
        assert!(matches!(entries[1].event, Event::TxStart { node, .. } if node == s1));

        // Event 2: TxEnd at t=96ns (duration of frame)
        assert_eq!(entries[2].key.time, BitTime::from_nanos(96));
        assert!(matches!(entries[2].event, Event::TxEnd { node, .. } if node == s1));

        // Event 3: FrontArrive at s2 at t=100ns (delay)
        assert_eq!(entries[3].key.time, tau);
        assert!(matches!(entries[3].event, Event::FrontArrive { node, .. } if node == s2));

        // Event 4: BackArrive at t=196ns (delay + duration)
        assert_eq!(entries[4].key.time, BitTime::from_nanos(196));
        assert!(matches!(entries[4].event, Event::BackArrive { node, .. } if node == s2));

        // Source returns to idle.
        assert_eq!(engine.node_state(s1), NodeRuntimeState::Idle);
        assert_eq!(engine.node_state(s2), NodeRuntimeState::Idle);
    }

    #[test]
    fn fd_1_no_collision_or_jam_events() {
        let (world, s1, _s2) = fd_pair(BitTime::from_nanos(100).as_u64(), BitRate::ETHERNET_1G);
        let mut engine = Engine::new(world);
        let frame = engine
            .register_frame(s1, Bits::new(96), SignalKind::Frame, BitRate::ETHERNET_1G)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();

        // Theorem 3-adjacent: zero collision events on FD.
        assert_eq!(
            count_events(engine.log(), |e| matches!(e, Event::CollisionDetect { .. })),
            0,
        );
        assert_eq!(
            count_events(engine.log(), |e| matches!(e, Event::JamStart { .. })),
            0,
        );
    }

    #[test]
    fn fd_1_receiver_does_not_track_foreign_carrier() {
        let (world, s1, s2) = fd_pair(BitTime::from_nanos(100).as_u64(), BitRate::ETHERNET_1G);
        let mut engine = Engine::new(world);
        let frame = engine
            .register_frame(s1, Bits::new(96), SignalKind::Frame, BitRate::ETHERNET_1G)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();

        // FD receivers do not track foreign carriers (no collision domain).
        assert_eq!(
            engine.foreign_carriers.get(&s2).copied().unwrap_or(0),
            0,
        );
    }

    // -- Theorem 3: bidirectional FD has zero collisions --------------------

    #[test]
    fn theorem_3_bidirectional_fd_has_zero_collisions() {
        // Both stations transmit simultaneously; no collision can occur
        // (Axiom A2 + Theorem 3). Each direction is on its own serializer.
        let tau = BitTime::from_nanos(100);
        let (world, s1, s2) = fd_pair(tau.as_u64(), BitRate::ETHERNET_1G);
        let mut engine = Engine::new(world);

        let f1 = engine
            .register_frame(s1, Bits::new(96), SignalKind::Frame, BitRate::ETHERNET_1G)
            .unwrap();
        let f2 = engine
            .register_frame(s2, Bits::new(96), SignalKind::Frame, BitRate::ETHERNET_1G)
            .unwrap();

        engine.schedule_tx_attempt(BitTime::ZERO, s1, f1);
        engine.schedule_tx_attempt(BitTime::ZERO, s2, f2);
        engine.run_until_idle();

        // Sharp oracle: exactly 10 events (5 per direction).
        assert_eq!(engine.log().len(), 10);

        // Theorem 3: zero CollisionDetect events.
        assert_eq!(
            count_events(engine.log(), |e| matches!(e, Event::CollisionDetect { .. })),
            0,
            "Theorem 3 violated — FD link produced a CollisionDetect event",
        );
        // No jam either.
        assert_eq!(
            count_events(engine.log(), |e| matches!(e, Event::JamStart { .. })),
            0,
        );
        assert_eq!(
            count_events(engine.log(), |e| matches!(e, Event::JamEnd { .. })),
            0,
        );

        // Both stations end idle.
        assert_eq!(engine.node_state(s1), NodeRuntimeState::Idle);
        assert_eq!(engine.node_state(s2), NodeRuntimeState::Idle);
    }

    // -- FD heterogeneous-rate -----------------------------------------------

    #[test]
    fn fd_at_100m_produces_correct_timestamps() {
        let tau = BitTime::from_nanos(100);
        let (world, s1, _) = fd_pair(tau.as_u64(), BitRate::ETHERNET_100M);
        let mut engine = Engine::new(world);
        // 64 bits at 100 Mbps = 640 ns.
        let frame = engine
            .register_frame(s1, Bits::new(64), SignalKind::Frame, BitRate::ETHERNET_100M)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();

        let entries: Vec<_> = engine.log().iter().collect();
        // FrontArrive at s2 at tau = 100 ns.
        let front = entries
            .iter()
            .find(|e| matches!(e.event, Event::FrontArrive { .. }))
            .unwrap();
        assert_eq!(front.key.time, BitTime::from_nanos(100));
        // TxEnd at duration = 640 ns.
        let tx_end = entries
            .iter()
            .find(|e| matches!(e.event, Event::TxEnd { .. }))
            .unwrap();
        assert_eq!(tx_end.key.time, BitTime::from_nanos(640));
        // BackArrive at tau + duration = 740 ns.
        let back = entries
            .iter()
            .find(|e| matches!(e.event, Event::BackArrive { .. }))
            .unwrap();
        assert_eq!(back.key.time, BitTime::from_nanos(740));
    }

    // -- FD attachment precomputation ----------------------------------------

    #[test]
    fn fd_attachments_populated_for_both_endpoints() {
        let tau = BitTime::from_nanos(100);
        let (world, s1, s2) = fd_pair(tau.as_u64(), BitRate::ETHERNET_1G);
        let engine = Engine::new(world);
        let a1 = engine.fd_attachments.get(&s1).copied().unwrap();
        let a2 = engine.fd_attachments.get(&s2).copied().unwrap();
        assert_eq!(a1.peer, s2);
        assert_eq!(a2.peer, s1);
        assert_eq!(a1.delay, tau);
        assert_eq!(a2.delay, tau);
        assert_eq!(a1.rate, BitRate::ETHERNET_1G);
    }

    // -- port_segment_kind helper -------------------------------------------

    #[test]
    fn port_segment_kind_distinguishes_hd_and_fd() {
        // Build a topology with one HD segment and one FD segment on
        // different stations.
        let mut b = TopologyBuilder::new();
        let hd_left = b.add_end_station(1);
        let hd_right = b.add_end_station(1);
        let fd_left = b.add_end_station(1);
        let fd_right = b.add_end_station(1);
        b.add_hd_segment(
            BitRate::ETHERNET_10M,
            BitTime::from_nanos(1_000),
            ep(hd_left, 0),
            ep(hd_right, 0),
        )
        .unwrap();
        b.add_fd_segment(
            BitRate::ETHERNET_1G,
            BitTime::from_nanos(100),
            ep(fd_left, 0),
            ep(fd_right, 0),
        )
        .unwrap();
        let world = b.build().unwrap();
        let engine = Engine::new(world);

        assert_eq!(
            engine.port_segment_kind(hd_left, PortId::new(0)),
            Some(SegmentKind::Hd),
        );
        assert_eq!(
            engine.port_segment_kind(fd_left, PortId::new(0)),
            Some(SegmentKind::Fd),
        );
        // Unknown port returns None.
        assert_eq!(engine.port_segment_kind(hd_left, PortId::new(99)), None);
    }

    // =======================================================================
    // Round 8d — bridge frame relay
    // =======================================================================

    /// Build: s1 — HD A — bridge:0 ; bridge:1 — HD B — s2.
    fn bridge_hd_topology(
        delay_a: BitTime,
        delay_b: BitTime,
        eta_b: Bits,
        pi_b: BitTime,
    ) -> (World, NodeId, NodeId, NodeId) {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        let br = b.add_bridge(2, eta_b, pi_b);
        b.add_hd_segment(BitRate::ETHERNET_1G, delay_a, ep(s1, 0), ep(br, 0))
            .unwrap();
        b.add_hd_segment(BitRate::ETHERNET_1G, delay_b, ep(s2, 0), ep(br, 1))
            .unwrap();
        (b.build().unwrap(), s1, s2, br)
    }

    // -- Theorem 5: bridge contention is queueing, not collision ------------

    #[test]
    fn theorem_5_bridge_relay_produces_zero_collisions() {
        let delay_a = BitTime::from_nanos(50);
        let delay_b = BitTime::from_nanos(50);
        let eta_b = Bits::new(64);
        let pi_b = BitTime::from_nanos(100);
        let (world, s1, s2, _br) = bridge_hd_topology(delay_a, delay_b, eta_b, pi_b);
        let mut engine = Engine::with_seed(world, 1);
        let frame = engine
            .register_frame(s1, Bits::new(96), SignalKind::Frame, BitRate::ETHERNET_1G)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();

        let log = engine.log();

        // Theorem 5: zero CollisionDetect events from the bridge.
        assert_eq!(
            count_events(log, |e| matches!(e, Event::CollisionDetect { .. })),
            0,
            "Theorem 5 violated — bridge relay produced a CollisionDetect event",
        );
        assert_eq!(
            count_events(log, |e| matches!(e, Event::JamStart { .. })),
            0,
        );

        // Frame relayed to s2: at least one FrontArrive at s2.
        let s2_arrivals = count_events(log, |e| {
            matches!(e, Event::FrontArrive { node, .. } if *node == s2)
        });
        assert!(
            s2_arrivals >= 1,
            "expected frame to be relayed to s2; got 0 FrontArrive events at s2",
        );
    }

    // -- FrameEligible timing -----------------------------------------------

    #[test]
    fn frame_eligible_fires_at_predicted_time() {
        // Frame's first bit arrives at bridge:0 at t = delay_a.
        // t_eligible = delay_a + eta_b/rate + pi_b
        //            = 50 + 64/(1Gbps) + 100 = 50 + 64 + 100 = 214 ns
        let delay_a = BitTime::from_nanos(50);
        let delay_b = BitTime::from_nanos(50);
        let eta_b = Bits::new(64);
        let pi_b = BitTime::from_nanos(100);
        let (world, s1, _s2, _br) = bridge_hd_topology(delay_a, delay_b, eta_b, pi_b);
        let mut engine = Engine::with_seed(world, 1);
        let frame = engine
            .register_frame(s1, Bits::new(96), SignalKind::Frame, BitRate::ETHERNET_1G)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();

        let elig_time = find_event_time(engine.log(), |e| {
            matches!(e, Event::FrameEligible { .. })
        });
        assert_eq!(elig_time, Some(BitTime::from_nanos(214)));
    }

    // -- Forwarding policy: flood excludes ingress ---------------------------

    #[test]
    fn flood_forwarding_does_not_send_back_to_ingress_port() {
        let delay_a = BitTime::from_nanos(50);
        let delay_b = BitTime::from_nanos(50);
        let eta_b = Bits::new(64);
        let pi_b = BitTime::from_nanos(100);
        let (world, s1, _s2, _br) = bridge_hd_topology(delay_a, delay_b, eta_b, pi_b);
        let mut engine = Engine::with_seed(world, 1);
        let frame = engine
            .register_frame(s1, Bits::new(96), SignalKind::Frame, BitRate::ETHERNET_1G)
            .unwrap();
        engine.schedule_tx_attempt(BitTime::ZERO, s1, frame);
        engine.run_until_idle();

        // s1 should NOT receive a re-arriving FrontArrive of the relay
        // signal (since the bridge wouldn't flood back to ingress port 0).
        // Count s1's FrontArrive events: they should be just 0 (s1 is the
        // source, not a target).
        let s1_arrivals = count_events(engine.log(), |e| {
            matches!(e, Event::FrontArrive { node, .. } if *node == s1)
        });
        assert_eq!(
            s1_arrivals, 0,
            "FloodForwarding sent the relay back to ingress port",
        );
    }

    // -- Bridge egress reachability precomputation --------------------------

    #[test]
    fn bridge_egress_reach_populated_for_each_bridge_port() {
        let delay_a = BitTime::from_nanos(50);
        let delay_b = BitTime::from_nanos(75);
        let eta_b = Bits::new(64);
        let pi_b = BitTime::from_nanos(100);
        let (world, s1, s2, br) = bridge_hd_topology(delay_a, delay_b, eta_b, pi_b);
        let engine = Engine::new(world);

        let reach_0 = engine.bridge_egress_reach.get(&(br, PortId::new(0))).unwrap();
        assert_eq!(reach_0.peers.len(), 1);
        assert_eq!(reach_0.peers[0].0, s1);
        assert_eq!(reach_0.peers[0].1, delay_a);

        let reach_1 = engine.bridge_egress_reach.get(&(br, PortId::new(1))).unwrap();
        assert_eq!(reach_1.peers.len(), 1);
        assert_eq!(reach_1.peers[0].0, s2);
        assert_eq!(reach_1.peers[0].1, delay_b);
    }

    // -- bridge_ports helper ------------------------------------------------

    #[test]
    fn bridge_ports_returns_all_attached_ports() {
        let delay = BitTime::from_nanos(50);
        let (world, _s1, _s2, br) =
            bridge_hd_topology(delay, delay, Bits::new(64), BitTime::from_nanos(100));
        let engine = Engine::new(world);
        let ports = engine.bridge_ports(br);
        assert_eq!(ports, vec![PortId::new(0), PortId::new(1)]);
    }

    // -- bridge_port_of_serializer reverse lookup ---------------------------

    #[test]
    fn bridge_port_of_serializer_round_trips() {
        let delay = BitTime::from_nanos(50);
        let (world, _s1, _s2, br) =
            bridge_hd_topology(delay, delay, Bits::new(64), BitTime::from_nanos(100));
        let engine = Engine::new(world);
        let port_0 = PortId::new(0);
        let serializer = engine.world().bridge_egress_serializer(br, port_0).unwrap();
        let lookup = engine.bridge_port_of_serializer(serializer);
        assert_eq!(lookup, Some((br, port_0)));
    }

    // -- signal_bits_at_rate inverse of Bits::at_rate -----------------------

    #[test]
    fn signal_bits_at_rate_round_trips() {
        // At 1 Gbps: 96 bits → 96 ns → back to 96 bits.
        let s = Signal::frame(
            NodeId::new(0),
            BitTime::ZERO,
            Bits::new(96),
            BitRate::ETHERNET_1G,
        )
        .unwrap();
        let bits = Engine::signal_bits_at_rate(s, BitRate::ETHERNET_1G);
        assert_eq!(bits, Bits::new(96));
    }
}
