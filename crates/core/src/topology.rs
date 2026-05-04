//! Topology types and the `TopologyBuilder` → `World` state transition.
//!
//! Per design.md §3.c.7, topology construction is a one-way state
//! transition: callers populate a [`TopologyBuilder`] (mutable, partial),
//! then call [`TopologyBuilder::build`] to obtain an immutable [`World`]
//! or a typed [`BuildError`]. The engine (round 8) consumes only `World`
//! values; there is no API that lets the engine mutate the topology.
//!
//! # Validated axioms
//!
//! `build()` enforces the axioms from design.md §2.a that require a global
//! view of the topology:
//!
//! * **A2** (FD single-transmitter) — eagerly: an FD segment's two
//!   endpoints must lie on distinct nodes.
//! * **A4** (bridge axiom — no internal HD arcs) — structurally: bridges
//!   carry no internal-arc field, plus a check that no segment's two
//!   endpoints both lie on the same bridge.
//! * **A7** (unique-path within HD components) — algorithmically: each
//!   HD-connected component (with bridge ports treated as terminating
//!   leaves) must be a tree (`E == V − 1`).
//! * Per-port single-attachment.
//! * `delay > 0` per segment (cousin of I5).
//!
//! # Deferred
//!
//! Full diameter validation (Theorem 2 / Corollary 8) requires the
//! engine's slot-time configuration; deferred to round 8. Bridge frame
//! relay logic (forwarding `Φ_b`) is in round 6.

use crate::resource::{CollisionId, SerializerId};
use crate::signal::NodeId;
use crate::time::{BitRate, BitTime, Bits};

use std::collections::{HashMap, VecDeque};

// ===========================================================================
// ID types
// ===========================================================================

/// Identifier of a port on a node.
///
/// Ports are 0-indexed within each node. A node with `port_count == n` has
/// ports `PortId(0)` through `PortId(n - 1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PortId(u32);

impl PortId {
    /// Construct a `PortId` from a raw `u32`.
    #[must_use]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// The underlying `u32`.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Identifier of a segment in a [`World`].
///
/// HD and FD segments share a single ID space. Use [`World::segment_kind`]
/// to determine which kind a given `SegmentId` resolves to, or query
/// [`World::hd_segment`] / [`World::fd_segment`] directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SegmentId(u32);

impl SegmentId {
    /// Construct a `SegmentId` from a raw `u32`.
    #[must_use]
    pub const fn new(id: u32) -> Self {
        Self(id)
    }

    /// The underlying `u32`.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

// ===========================================================================
// Endpoint and Direction
// ===========================================================================

/// A reference to a specific port on a specific node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Endpoint {
    /// The node this endpoint is on.
    pub node: NodeId,
    /// The port on `node`.
    pub port: PortId,
}

impl Endpoint {
    /// Construct an `Endpoint`.
    #[must_use]
    pub const fn new(node: NodeId, port: PortId) -> Self {
        Self { node, port }
    }
}

/// Direction of an FD serializer.
///
/// FD segments are point-to-point; each segment's two endpoints are `a`
/// and `b`. `Direction::AtoB` selects the serializer that transmits from
/// `a` toward `b`; `Direction::BtoA` selects the reverse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Direction {
    /// From the first endpoint (`a`) to the second (`b`).
    AtoB,
    /// From the second endpoint (`b`) to the first (`a`).
    BtoA,
}

// ===========================================================================
// Segment types
// ===========================================================================

/// A half-duplex shared-medium segment.
///
/// HD segments may be members of larger collision domains formed via
/// repeater interconnection. Per axiom A7, each HD-connected component
/// must be a tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HdSegment {
    rate: BitRate,
    delay: BitTime,
    endpoints: (Endpoint, Endpoint),
}

impl HdSegment {
    /// The segment's bit rate.
    #[must_use]
    pub const fn rate(&self) -> BitRate {
        self.rate
    }

    /// The one-way propagation delay (including PHY margin).
    #[must_use]
    pub const fn delay(&self) -> BitTime {
        self.delay
    }

    /// The segment's two endpoints.
    #[must_use]
    pub const fn endpoints(&self) -> (Endpoint, Endpoint) {
        self.endpoints
    }
}

/// A full-duplex point-to-point segment.
///
/// Per axiom A2, an FD segment's two endpoints must lie on distinct nodes;
/// each direction has a single legal injector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FdSegment {
    rate: BitRate,
    delay: BitTime,
    endpoints: (Endpoint, Endpoint),
}

impl FdSegment {
    /// The segment's bit rate.
    #[must_use]
    pub const fn rate(&self) -> BitRate {
        self.rate
    }

    /// The one-way propagation delay (including PHY margin).
    #[must_use]
    pub const fn delay(&self) -> BitTime {
        self.delay
    }

    /// The segment's two endpoints.
    #[must_use]
    pub const fn endpoints(&self) -> (Endpoint, Endpoint) {
        self.endpoints
    }
}

/// Discriminator for [`SegmentId`] kind queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SegmentKind {
    /// The segment is an [`HdSegment`].
    Hd,
    /// The segment is an [`FdSegment`].
    Fd,
}

// ===========================================================================
// Node kinds
// ===========================================================================

/// Configuration for an end-station node.
///
/// Round 5 placeholder; round 8 (engine) attaches MAC policies externally
/// rather than storing them on this struct, so the same topology can be
/// reused with different policy configurations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EndStationData;

/// Configuration for a repeater (hub) node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RepeaterData {
    /// Re-emit delay `δ_h` per axiom A3.
    pub delta_h: BitTime,
}

/// Configuration for a bridge (frame-relay) node.
///
/// In round 5, this carries only the parameters needed for topology
/// validation and event-time computation. The forwarding relation `Φ_b`
/// is the subject of round 6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BridgeData {
    /// Decode threshold `η_b` — the number of bits that must arrive before
    /// the frame is eligible for forwarding (e.g., the full frame for
    /// store-and-forward, or a header threshold for cut-through).
    pub decode_threshold: Bits,
    /// Processing delay `π_b` between decode-eligibility and egress-queue
    /// admission.
    pub processing_delay: BitTime,
}

/// The kind of a node, with kind-specific configuration data.
///
/// Per design.md §3.c.5, this enum is publicly exhaustive: external
/// consumers exhaustively match the variants and benefit from compile-time
/// breakage when new kinds are added.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeKind {
    /// An end station (data source/sink).
    EndStation(EndStationData),
    /// A repeater/hub (PHY-layer interconnection per A3).
    Repeater(RepeaterData),
    /// A bridge/switch (MAC-sublayer interconnection per A4).
    Bridge(BridgeData),
}

// ===========================================================================
// BuildError
// ===========================================================================

/// Errors returned by [`TopologyBuilder`] methods.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BuildError {
    /// An endpoint references a non-existent node.
    UnknownNode {
        /// The unknown node ID.
        node: NodeId,
    },
    /// An endpoint references a port that does not exist on the named node.
    UnknownPort {
        /// The node whose port count was exceeded.
        node: NodeId,
        /// The out-of-range port.
        port: PortId,
    },
    /// A segment endpoint conflicts with a segment already attached to
    /// that (node, port).
    PortAlreadyConnected {
        /// The node carrying the conflict.
        node: NodeId,
        /// The port carrying the conflict.
        port: PortId,
        /// The segment already on that port.
        existing_segment: SegmentId,
    },
    /// A segment's two endpoints lie on the same node.
    EndpointsOnSameNode {
        /// The shared node.
        node: NodeId,
    },
    /// A segment was constructed with `delay == BitTime::ZERO`.
    ZeroDelay,
    /// An HD-connected component is not a tree (cycle or multi-path).
    ///
    /// Violates axiom A7. The `component_root` field names a representative
    /// node from the offending component to aid diagnostics.
    UniquePathViolated {
        /// A representative node from the offending HD component.
        component_root: NodeId,
    },
    /// A catch-all for round-5 validation gaps.
    InvalidConfig(&'static str),
}

impl core::fmt::Display for BuildError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownNode { node } => {
                write!(f, "unknown node: {node:?}")
            }
            Self::UnknownPort { node, port } => {
                write!(f, "unknown port on {node:?}: {port:?}")
            }
            Self::PortAlreadyConnected {
                node,
                port,
                existing_segment,
            } => {
                write!(
                    f,
                    "port already connected: {node:?} {port:?} is on {existing_segment:?}",
                )
            }
            Self::EndpointsOnSameNode { node } => {
                write!(f, "segment endpoints on same node: {node:?}")
            }
            Self::ZeroDelay => f.write_str("segment delay must be > 0"),
            Self::UniquePathViolated { component_root } => {
                write!(
                    f,
                    "axiom A7 violated: HD component containing {component_root:?} is not a tree",
                )
            }
            Self::InvalidConfig(msg) => write!(f, "invalid topology configuration: {msg}"),
        }
    }
}

impl core::error::Error for BuildError {}

// ===========================================================================
// TopologyBuilder
// ===========================================================================

#[derive(Debug, Clone, Copy)]
struct NodeBuilderRecord {
    kind: NodeKind,
    port_count: u32,
}

#[derive(Debug, Clone, Copy)]
enum SegmentBuilderRecord {
    Hd(HdSegment),
    Fd(FdSegment),
}

impl SegmentBuilderRecord {
    fn endpoints(&self) -> (Endpoint, Endpoint) {
        match self {
            Self::Hd(s) => s.endpoints,
            Self::Fd(s) => s.endpoints,
        }
    }
}

/// Mutable builder for a [`World`].
///
/// Add nodes and segments, then call [`TopologyBuilder::build`] to obtain
/// the immutable `World`. See module-level docs for which validations are
/// eager (per `add_*` call) vs deferred to `build()`.
#[derive(Debug, Clone, Default)]
pub struct TopologyBuilder {
    nodes: Vec<NodeBuilderRecord>,
    segments: Vec<SegmentBuilderRecord>,
}

impl TopologyBuilder {
    /// Construct an empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn next_node_id(&self) -> NodeId {
        // Cast width: u32 is more than enough for any realistic topology.
        #[allow(clippy::cast_possible_truncation)]
        NodeId::new(self.nodes.len() as u32)
    }

    fn next_segment_id(&self) -> SegmentId {
        #[allow(clippy::cast_possible_truncation)]
        SegmentId::new(self.segments.len() as u32)
    }

    /// Add an end-station node with the given port count.
    pub fn add_end_station(&mut self, port_count: u32) -> NodeId {
        let id = self.next_node_id();
        self.nodes.push(NodeBuilderRecord {
            kind: NodeKind::EndStation(EndStationData),
            port_count,
        });
        id
    }

    /// Add a repeater (hub) node with the given port count and re-emit
    /// delay `δ_h`.
    pub fn add_repeater(&mut self, port_count: u32, delta_h: BitTime) -> NodeId {
        let id = self.next_node_id();
        self.nodes.push(NodeBuilderRecord {
            kind: NodeKind::Repeater(RepeaterData { delta_h }),
            port_count,
        });
        id
    }

    /// Add a bridge node with the given port count, decode threshold, and
    /// processing delay.
    pub fn add_bridge(
        &mut self,
        port_count: u32,
        decode_threshold: Bits,
        processing_delay: BitTime,
    ) -> NodeId {
        let id = self.next_node_id();
        self.nodes.push(NodeBuilderRecord {
            kind: NodeKind::Bridge(BridgeData {
                decode_threshold,
                processing_delay,
            }),
            port_count,
        });
        id
    }

    /// Add a half-duplex shared-medium segment.
    ///
    /// # Errors
    ///
    /// - [`BuildError::UnknownNode`] if either endpoint's node does not
    ///   exist.
    /// - [`BuildError::UnknownPort`] if either endpoint's port is out of
    ///   range for its node.
    /// - [`BuildError::EndpointsOnSameNode`] if both endpoints reference
    ///   the same node.
    /// - [`BuildError::ZeroDelay`] if `delay == BitTime::ZERO`.
    pub fn add_hd_segment(
        &mut self,
        rate: BitRate,
        delay: BitTime,
        a: Endpoint,
        b: Endpoint,
    ) -> Result<SegmentId, BuildError> {
        self.validate_endpoint(a)?;
        self.validate_endpoint(b)?;
        if a.node == b.node {
            return Err(BuildError::EndpointsOnSameNode { node: a.node });
        }
        if delay == BitTime::ZERO {
            return Err(BuildError::ZeroDelay);
        }
        let id = self.next_segment_id();
        self.segments.push(SegmentBuilderRecord::Hd(HdSegment {
            rate,
            delay,
            endpoints: (a, b),
        }));
        Ok(id)
    }

    /// Add a full-duplex point-to-point segment.
    ///
    /// # Errors
    ///
    /// Same as [`Self::add_hd_segment`].
    pub fn add_fd_segment(
        &mut self,
        rate: BitRate,
        delay: BitTime,
        a: Endpoint,
        b: Endpoint,
    ) -> Result<SegmentId, BuildError> {
        self.validate_endpoint(a)?;
        self.validate_endpoint(b)?;
        if a.node == b.node {
            return Err(BuildError::EndpointsOnSameNode { node: a.node });
        }
        if delay == BitTime::ZERO {
            return Err(BuildError::ZeroDelay);
        }
        let id = self.next_segment_id();
        self.segments.push(SegmentBuilderRecord::Fd(FdSegment {
            rate,
            delay,
            endpoints: (a, b),
        }));
        Ok(id)
    }

    fn validate_endpoint(&self, ep: Endpoint) -> Result<(), BuildError> {
        let Some(node) = self.nodes.get(ep.node.as_u32() as usize) else {
            return Err(BuildError::UnknownNode { node: ep.node });
        };
        if ep.port.as_u32() >= node.port_count {
            return Err(BuildError::UnknownPort {
                node: ep.node,
                port: ep.port,
            });
        }
        Ok(())
    }

    fn node_kind(&self, id: NodeId) -> Option<&NodeKind> {
        self.nodes
            .get(id.as_u32() as usize)
            .map(|n| &n.kind)
    }

    /// Validate the constructed topology and return an immutable [`World`].
    ///
    /// # Errors
    ///
    /// - [`BuildError::PortAlreadyConnected`] if any port is on more than
    ///   one segment.
    /// - [`BuildError::UniquePathViolated`] if any HD-connected component
    ///   is not a tree (axiom A7).
    //
    // RATIONALE for the lint allowances: this function is the
    // axiom-validation entry point. The `expect()` calls are
    // construction invariants that hold by virtue of vertex assignment
    // happening before BFS — splitting the function further would
    // scatter the invariant maintenance across helpers without making
    // any single check easier to reason about.
    #[allow(
        clippy::too_many_lines,
        clippy::expect_used,
        clippy::missing_panics_doc,
        reason = "axiom-validation entry point; expects are construction invariants that cannot fire under correct vertex assignment, so no real panic to document"
    )]
    pub fn build(self) -> Result<World, BuildError> {
        // 1. Per-port single-attachment.
        let mut port_to_segment: HashMap<(NodeId, PortId), SegmentId> = HashMap::new();
        for (idx, seg) in self.segments.iter().enumerate() {
            #[allow(clippy::cast_possible_truncation)]
            let seg_id = SegmentId::new(idx as u32);
            let (a, b) = seg.endpoints();
            for ep in [a, b] {
                let key = (ep.node, ep.port);
                if let Some(&existing) = port_to_segment.get(&key) {
                    return Err(BuildError::PortAlreadyConnected {
                        node: ep.node,
                        port: ep.port,
                        existing_segment: existing,
                    });
                }
                port_to_segment.insert(key, seg_id);
            }
        }

        // 2. A7: HD components must be trees.
        // Vertex assignment: end stations and repeaters collapse all their
        // ports into one vertex; bridge ports are each their own vertex.
        let (vertex_of, vertex_count) = self.assign_hd_vertices();

        // Build adjacency over HD segments only.
        let mut adj: Vec<Vec<usize>> = vec![Vec::new(); vertex_count];
        let mut hd_segment_indices: Vec<usize> = Vec::new();
        for (idx, seg) in self.segments.iter().enumerate() {
            if let SegmentBuilderRecord::Hd(h) = seg {
                let va = vertex_of[&(h.endpoints.0.node, h.endpoints.0.port)];
                let vb = vertex_of[&(h.endpoints.1.node, h.endpoints.1.port)];
                adj[va].push(vb);
                adj[vb].push(va);
                hd_segment_indices.push(idx);
            }
        }

        // BFS over each HD component; verify E = V - 1.
        // Track which collision-resource id each HD segment belongs to.
        let mut visited = vec![false; vertex_count];
        let mut vertex_to_component: Vec<Option<usize>> = vec![None; vertex_count];
        let mut hd_component_count: usize = 0;
        // Reverse map: vertex_of allows us to find the (node, port) pair
        // for a vertex when reporting errors. Build it lazily below.
        let mut vertex_to_node: HashMap<usize, NodeId> = HashMap::new();
        for (&(node, _port), &v) in &vertex_of {
            vertex_to_node.entry(v).or_insert(node);
        }

        for start in 0..vertex_count {
            if visited[start] {
                continue;
            }
            let component_id = hd_component_count;
            hd_component_count += 1;
            let mut queue: VecDeque<usize> = VecDeque::new();
            queue.push_back(start);
            visited[start] = true;
            vertex_to_component[start] = Some(component_id);
            let mut v_count: usize = 0;
            let mut edge_endpoints: usize = 0; // each edge counted twice

            while let Some(v) = queue.pop_front() {
                v_count += 1;
                for &u in &adj[v] {
                    edge_endpoints += 1;
                    if !visited[u] {
                        visited[u] = true;
                        vertex_to_component[u] = Some(component_id);
                        queue.push_back(u);
                    }
                }
            }

            let e_count = edge_endpoints / 2;
            if e_count + 1 != v_count {
                let component_root = *vertex_to_node.get(&start).expect("vertex registered");
                return Err(BuildError::UniquePathViolated { component_root });
            }
        }

        // Map each HD segment to its component's CollisionId.
        let collision_resources: Vec<CollisionId> = (0..hd_component_count)
            .map(|i| {
                #[allow(clippy::cast_possible_truncation)]
                CollisionId::new(i as u32)
            })
            .collect();

        let mut hd_segment_to_collision: HashMap<SegmentId, CollisionId> = HashMap::new();
        for &idx in &hd_segment_indices {
            let SegmentBuilderRecord::Hd(h) = &self.segments[idx] else {
                continue;
            };
            let v = vertex_of[&(h.endpoints.0.node, h.endpoints.0.port)];
            let comp = vertex_to_component[v].expect("vertex visited");
            #[allow(clippy::cast_possible_truncation)]
            let seg_id = SegmentId::new(idx as u32);
            hd_segment_to_collision.insert(seg_id, collision_resources[comp]);
        }

        // 3. FD serializer assignment: 2 per FD segment.
        let mut next_serializer: u32 = 0;
        let mut fd_serializers: HashMap<SegmentId, (SerializerId, SerializerId)> = HashMap::new();
        for (idx, seg) in self.segments.iter().enumerate() {
            if let SegmentBuilderRecord::Fd(_) = seg {
                let s_ab = SerializerId::new(next_serializer);
                next_serializer += 1;
                let s_ba = SerializerId::new(next_serializer);
                next_serializer += 1;
                #[allow(clippy::cast_possible_truncation)]
                let seg_id = SegmentId::new(idx as u32);
                fd_serializers.insert(seg_id, (s_ab, s_ba));
            }
        }

        // 4. Bridge egress serializer assignment: 1 per bridge port.
        let mut bridge_egress: HashMap<(NodeId, PortId), SerializerId> = HashMap::new();
        for (node_idx, node) in self.nodes.iter().enumerate() {
            if !matches!(node.kind, NodeKind::Bridge(_)) {
                continue;
            }
            #[allow(clippy::cast_possible_truncation)]
            let node_id = NodeId::new(node_idx as u32);
            for p in 0..node.port_count {
                let port_id = PortId::new(p);
                let s = SerializerId::new(next_serializer);
                next_serializer += 1;
                bridge_egress.insert((node_id, port_id), s);
            }
        }

        // 5. Materialize the World.
        let segments: Vec<SegmentRecord> = self
            .segments
            .into_iter()
            .map(|s| match s {
                SegmentBuilderRecord::Hd(h) => SegmentRecord::Hd(h),
                SegmentBuilderRecord::Fd(f) => SegmentRecord::Fd(f),
            })
            .collect();

        // Per-node ports vector: index = port number; Some(seg) if connected.
        let nodes: Vec<NodeRecord> = self
            .nodes
            .iter()
            .enumerate()
            .map(|(node_idx, n)| {
                #[allow(clippy::cast_possible_truncation)]
                let node_id = NodeId::new(node_idx as u32);
                let mut ports: Vec<Option<SegmentId>> = vec![None; n.port_count as usize];
                for (port_idx, port_slot) in ports.iter_mut().enumerate() {
                    #[allow(clippy::cast_possible_truncation)]
                    let port_id = PortId::new(port_idx as u32);
                    if let Some(&seg) = port_to_segment.get(&(node_id, port_id)) {
                        *port_slot = Some(seg);
                    }
                }
                NodeRecord { kind: n.kind, ports }
            })
            .collect();

        Ok(World {
            nodes,
            segments,
            collision_resources,
            hd_segment_to_collision,
            fd_serializers,
            bridge_egress,
        })
    }

    /// Build the vertex assignment for the HD propagation graph.
    ///
    /// Returns `(vertex_of, vertex_count)` where `vertex_of` maps each
    /// `(NodeId, PortId)` participating in an HD segment to a vertex
    /// index. End stations and repeaters collapse all their ports into
    /// a single vertex; bridge ports are each their own vertex.
    //
    // RATIONALE: the `expect()` is an `add_hd_segment` post-condition —
    // every HD segment endpoint references an existing node by validation
    // at the call site.
    #[allow(clippy::expect_used)]
    fn assign_hd_vertices(&self) -> (HashMap<(NodeId, PortId), usize>, usize) {
        let mut vertex_of: HashMap<(NodeId, PortId), usize> = HashMap::new();
        let mut node_to_vertex: HashMap<NodeId, usize> = HashMap::new();
        let mut next_vid: usize = 0;

        for seg in &self.segments {
            let SegmentBuilderRecord::Hd(h) = seg else {
                continue;
            };
            for ep in [h.endpoints.0, h.endpoints.1] {
                let kind = self
                    .node_kind(ep.node)
                    .expect("validated by add_hd_segment");
                if matches!(kind, NodeKind::Bridge(_)) {
                    vertex_of.entry((ep.node, ep.port)).or_insert_with(|| {
                        let v = next_vid;
                        next_vid += 1;
                        v
                    });
                } else {
                    let v = *node_to_vertex.entry(ep.node).or_insert_with(|| {
                        let v = next_vid;
                        next_vid += 1;
                        v
                    });
                    vertex_of.insert((ep.node, ep.port), v);
                }
            }
        }

        (vertex_of, next_vid)
    }
}

// ===========================================================================
// World
// ===========================================================================

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct NodeRecord {
    kind: NodeKind,
    ports: Vec<Option<SegmentId>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SegmentRecord {
    Hd(HdSegment),
    Fd(FdSegment),
}

/// A validated, immutable topology.
///
/// Constructed exclusively via [`TopologyBuilder::build`]. The engine
/// (round 8) consumes a `World` and produces an event log; there is no
/// API that mutates a `World` after construction (invariant I1).
#[derive(Debug, Clone)]
pub struct World {
    nodes: Vec<NodeRecord>,
    segments: Vec<SegmentRecord>,
    collision_resources: Vec<CollisionId>,
    hd_segment_to_collision: HashMap<SegmentId, CollisionId>,
    fd_serializers: HashMap<SegmentId, (SerializerId, SerializerId)>,
    bridge_egress: HashMap<(NodeId, PortId), SerializerId>,
}

impl World {
    /// The number of nodes in the topology.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// The number of segments in the topology.
    #[must_use]
    pub fn segment_count(&self) -> usize {
        self.segments.len()
    }

    /// The number of HD-connected (collision) components.
    #[must_use]
    pub fn collision_resource_count(&self) -> usize {
        self.collision_resources.len()
    }

    /// The total number of serializers (FD directions + bridge egresses).
    #[must_use]
    pub fn serializer_count(&self) -> usize {
        let fd = self.fd_serializers.len() * 2;
        let bridge = self.bridge_egress.len();
        fd + bridge
    }

    /// Look up a node by ID.
    #[must_use]
    pub fn node(&self, id: NodeId) -> Option<&NodeKind> {
        self.nodes
            .get(id.as_u32() as usize)
            .map(|n| &n.kind)
    }

    /// Iterate over all `(NodeId, &NodeKind)` pairs.
    pub fn nodes(&self) -> impl Iterator<Item = (NodeId, &NodeKind)> + '_ {
        self.nodes.iter().enumerate().map(|(i, n)| {
            #[allow(clippy::cast_possible_truncation)]
            (NodeId::new(i as u32), &n.kind)
        })
    }

    /// The kind of the segment with the given ID, or `None` if `id` is
    /// out of range.
    #[must_use]
    pub fn segment_kind(&self, id: SegmentId) -> Option<SegmentKind> {
        self.segments.get(id.as_u32() as usize).map(|s| match s {
            SegmentRecord::Hd(_) => SegmentKind::Hd,
            SegmentRecord::Fd(_) => SegmentKind::Fd,
        })
    }

    /// Look up an HD segment by ID.
    ///
    /// Returns `None` if `id` is out of range or refers to an FD segment.
    #[must_use]
    pub fn hd_segment(&self, id: SegmentId) -> Option<&HdSegment> {
        match self.segments.get(id.as_u32() as usize)? {
            SegmentRecord::Hd(s) => Some(s),
            SegmentRecord::Fd(_) => None,
        }
    }

    /// Look up an FD segment by ID.
    ///
    /// Returns `None` if `id` is out of range or refers to an HD segment.
    #[must_use]
    pub fn fd_segment(&self, id: SegmentId) -> Option<&FdSegment> {
        match self.segments.get(id.as_u32() as usize)? {
            SegmentRecord::Fd(s) => Some(s),
            SegmentRecord::Hd(_) => None,
        }
    }

    /// The collision resource an HD segment belongs to.
    ///
    /// Returns `None` if `id` is out of range or refers to an FD segment.
    #[must_use]
    pub fn collision_resource_of(&self, id: SegmentId) -> Option<CollisionId> {
        self.hd_segment_to_collision.get(&id).copied()
    }

    /// The serializer for an FD segment in the given direction.
    ///
    /// Returns `None` if `id` is out of range or refers to an HD segment.
    #[must_use]
    pub fn serializer_of(
        &self,
        id: SegmentId,
        direction: Direction,
    ) -> Option<SerializerId> {
        let (a_to_b, b_to_a) = self.fd_serializers.get(&id).copied()?;
        Some(match direction {
            Direction::AtoB => a_to_b,
            Direction::BtoA => b_to_a,
        })
    }

    /// The egress serializer for a bridge port, or `None` if the port is
    /// not on a bridge.
    #[must_use]
    pub fn bridge_egress_serializer(
        &self,
        node: NodeId,
        port: PortId,
    ) -> Option<SerializerId> {
        self.bridge_egress.get(&(node, port)).copied()
    }
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

    fn delay(ns: u64) -> BitTime {
        BitTime::from_nanos(ns)
    }

    fn ep(node: NodeId, port: u32) -> Endpoint {
        Endpoint::new(node, PortId::new(port))
    }

    // -- Construction success cases ------------------------------------------

    #[test]
    fn builds_single_fd_link() {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        let _seg = b
            .add_fd_segment(BitRate::ETHERNET_1G, delay(50), ep(s1, 0), ep(s2, 0))
            .unwrap();
        let world = b.build().unwrap();

        assert_eq!(world.node_count(), 2);
        assert_eq!(world.segment_count(), 1);
        assert_eq!(world.collision_resource_count(), 0);
        assert_eq!(world.serializer_count(), 2); // two FD directions
    }

    #[test]
    fn builds_two_station_hd_pair() {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        let seg = b
            .add_hd_segment(BitRate::ETHERNET_10M, delay(5_000), ep(s1, 0), ep(s2, 0))
            .unwrap();
        let world = b.build().unwrap();

        assert_eq!(world.collision_resource_count(), 1);
        assert_eq!(world.serializer_count(), 0);
        assert!(world.collision_resource_of(seg).is_some());
    }

    #[test]
    fn builds_three_station_hd_hub() {
        // 3 stations, 1 repeater, 3 HD segments — a tree (V=4, E=3).
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        let s3 = b.add_end_station(1);
        let r = b.add_repeater(3, delay(100));
        let seg1 = b
            .add_hd_segment(BitRate::ETHERNET_10M, delay(1_000), ep(s1, 0), ep(r, 0))
            .unwrap();
        let seg2 = b
            .add_hd_segment(BitRate::ETHERNET_10M, delay(1_000), ep(s2, 0), ep(r, 1))
            .unwrap();
        let seg3 = b
            .add_hd_segment(BitRate::ETHERNET_10M, delay(1_000), ep(s3, 0), ep(r, 2))
            .unwrap();
        let world = b.build().unwrap();

        assert_eq!(world.collision_resource_count(), 1);
        let cid = world.collision_resource_of(seg1).unwrap();
        assert_eq!(world.collision_resource_of(seg2), Some(cid));
        assert_eq!(world.collision_resource_of(seg3), Some(cid));
    }

    #[test]
    fn builds_mixed_topology_with_bridge() {
        // Two HD pairs joined by a bridge.
        // Component A: s1 — HD — bridge.port0
        // Component B: s2 — HD — bridge.port1
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        let bridge = b.add_bridge(2, Bits::new(64), delay(500));
        let seg_a = b
            .add_hd_segment(BitRate::ETHERNET_10M, delay(1_000), ep(s1, 0), ep(bridge, 0))
            .unwrap();
        let seg_b = b
            .add_hd_segment(BitRate::ETHERNET_10M, delay(1_000), ep(s2, 0), ep(bridge, 1))
            .unwrap();
        let world = b.build().unwrap();

        // Bridge ports terminate HD components; thus 2 collision domains.
        assert_eq!(world.collision_resource_count(), 2);
        assert_ne!(
            world.collision_resource_of(seg_a),
            world.collision_resource_of(seg_b),
            "bridge ports terminate HD components — distinct collision resources",
        );
        // Bridge has 2 egress serializers (one per port).
        assert!(world.bridge_egress_serializer(bridge, PortId::new(0)).is_some());
        assert!(world.bridge_egress_serializer(bridge, PortId::new(1)).is_some());
    }

    // -- A7 unique-path violations -------------------------------------------

    #[test]
    fn rejects_hd_cycle_three_stations_triangle() {
        // Three stations connected pairwise by HD segments — direct cycle.
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(2);
        let s2 = b.add_end_station(2);
        let s3 = b.add_end_station(2);
        b.add_hd_segment(BitRate::ETHERNET_10M, delay(1_000), ep(s1, 0), ep(s2, 0))
            .unwrap();
        b.add_hd_segment(BitRate::ETHERNET_10M, delay(1_000), ep(s2, 1), ep(s3, 0))
            .unwrap();
        b.add_hd_segment(BitRate::ETHERNET_10M, delay(1_000), ep(s3, 1), ep(s1, 1))
            .unwrap();
        let result = b.build();
        assert!(matches!(
            result,
            Err(BuildError::UniquePathViolated { .. }),
        ));
    }

    #[test]
    fn rejects_hd_diamond_two_paths() {
        // S1 → R1 → S2 and S1 → R2 → S2, forming a diamond.
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(2);
        let s2 = b.add_end_station(2);
        let r1 = b.add_repeater(2, delay(100));
        let r2 = b.add_repeater(2, delay(100));
        b.add_hd_segment(BitRate::ETHERNET_10M, delay(1_000), ep(s1, 0), ep(r1, 0))
            .unwrap();
        b.add_hd_segment(BitRate::ETHERNET_10M, delay(1_000), ep(r1, 1), ep(s2, 0))
            .unwrap();
        b.add_hd_segment(BitRate::ETHERNET_10M, delay(1_000), ep(s1, 1), ep(r2, 0))
            .unwrap();
        b.add_hd_segment(BitRate::ETHERNET_10M, delay(1_000), ep(r2, 1), ep(s2, 1))
            .unwrap();
        let result = b.build();
        assert!(matches!(
            result,
            Err(BuildError::UniquePathViolated { .. }),
        ));
    }

    #[test]
    fn allows_diamond_through_bridges() {
        // Same diamond shape, but the two paths are FD links through a
        // bridge — A7 doesn't apply across FD/bridge boundaries.
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(2);
        let s2 = b.add_end_station(2);
        let bridge = b.add_bridge(4, Bits::new(64), delay(500));
        b.add_fd_segment(BitRate::ETHERNET_1G, delay(100), ep(s1, 0), ep(bridge, 0))
            .unwrap();
        b.add_fd_segment(BitRate::ETHERNET_1G, delay(100), ep(bridge, 1), ep(s2, 0))
            .unwrap();
        b.add_fd_segment(BitRate::ETHERNET_1G, delay(100), ep(s1, 1), ep(bridge, 2))
            .unwrap();
        b.add_fd_segment(BitRate::ETHERNET_1G, delay(100), ep(bridge, 3), ep(s2, 1))
            .unwrap();
        // Should build successfully — A7 only constrains HD components.
        assert!(b.build().is_ok());
    }

    // -- Boundary errors -----------------------------------------------------

    #[test]
    fn rejects_unknown_node() {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        // NodeId(99) does not exist.
        let result = b.add_hd_segment(
            BitRate::ETHERNET_10M,
            delay(1_000),
            ep(s1, 0),
            Endpoint::new(NodeId::new(99), PortId::new(0)),
        );
        assert!(matches!(result, Err(BuildError::UnknownNode { .. })));
    }

    #[test]
    fn rejects_unknown_port() {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        // Port 5 doesn't exist on s2.
        let result = b.add_hd_segment(
            BitRate::ETHERNET_10M,
            delay(1_000),
            ep(s1, 0),
            ep(s2, 5),
        );
        assert!(matches!(result, Err(BuildError::UnknownPort { .. })));
    }

    #[test]
    fn rejects_port_already_connected() {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        let s3 = b.add_end_station(1);
        b.add_hd_segment(BitRate::ETHERNET_10M, delay(1_000), ep(s1, 0), ep(s2, 0))
            .unwrap();
        // s2 port 0 already used.
        b.add_hd_segment(BitRate::ETHERNET_10M, delay(1_000), ep(s3, 0), ep(s2, 0))
            .unwrap();
        let result = b.build();
        assert!(matches!(
            result,
            Err(BuildError::PortAlreadyConnected { .. }),
        ));
    }

    #[test]
    fn rejects_endpoints_on_same_node() {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(2);
        let result = b.add_hd_segment(
            BitRate::ETHERNET_10M,
            delay(1_000),
            ep(s1, 0),
            ep(s1, 1),
        );
        assert!(matches!(
            result,
            Err(BuildError::EndpointsOnSameNode { .. }),
        ));
    }

    #[test]
    fn rejects_zero_delay() {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        let result =
            b.add_hd_segment(BitRate::ETHERNET_10M, BitTime::ZERO, ep(s1, 0), ep(s2, 0));
        assert_eq!(result, Err(BuildError::ZeroDelay));
    }

    // -- Type-level distinction (runtime form) -------------------------------

    #[test]
    fn hd_segment_lookup_returns_none_for_fd_id() {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        let fd_id = b
            .add_fd_segment(BitRate::ETHERNET_1G, delay(100), ep(s1, 0), ep(s2, 0))
            .unwrap();
        let world = b.build().unwrap();
        assert!(world.hd_segment(fd_id).is_none());
        assert!(world.fd_segment(fd_id).is_some());
        assert_eq!(world.segment_kind(fd_id), Some(SegmentKind::Fd));
    }

    #[test]
    fn fd_segment_lookup_returns_none_for_hd_id() {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        let hd_id = b
            .add_hd_segment(BitRate::ETHERNET_10M, delay(1_000), ep(s1, 0), ep(s2, 0))
            .unwrap();
        let world = b.build().unwrap();
        assert!(world.fd_segment(hd_id).is_none());
        assert!(world.hd_segment(hd_id).is_some());
        assert_eq!(world.segment_kind(hd_id), Some(SegmentKind::Hd));
    }

    // -- Resource assignment -------------------------------------------------

    #[test]
    fn fd_directions_get_distinct_serializers() {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(1);
        let s2 = b.add_end_station(1);
        let fd = b
            .add_fd_segment(BitRate::ETHERNET_1G, delay(100), ep(s1, 0), ep(s2, 0))
            .unwrap();
        let world = b.build().unwrap();
        let s_ab = world.serializer_of(fd, Direction::AtoB).unwrap();
        let s_ba = world.serializer_of(fd, Direction::BtoA).unwrap();
        assert_ne!(s_ab, s_ba);
    }

    #[test]
    fn each_bridge_port_gets_its_own_serializer() {
        let mut b = TopologyBuilder::new();
        let bridge = b.add_bridge(3, Bits::new(64), delay(500));
        let world = b.build().unwrap();
        let s0 = world.bridge_egress_serializer(bridge, PortId::new(0)).unwrap();
        let s1 = world.bridge_egress_serializer(bridge, PortId::new(1)).unwrap();
        let s2 = world.bridge_egress_serializer(bridge, PortId::new(2)).unwrap();
        assert_ne!(s0, s1);
        assert_ne!(s1, s2);
        assert_ne!(s0, s2);
    }

    // -- Node accessors ------------------------------------------------------

    #[test]
    fn world_node_iterator_yields_all_nodes() {
        let mut b = TopologyBuilder::new();
        let _ = b.add_end_station(1);
        let _ = b.add_end_station(1);
        let _ = b.add_repeater(2, delay(100));
        let world = b.build().unwrap();
        let collected: Vec<_> = world.nodes().collect();
        assert_eq!(collected.len(), 3);
    }

    #[test]
    fn world_node_returns_correct_kind() {
        let mut b = TopologyBuilder::new();
        let s = b.add_end_station(1);
        let r = b.add_repeater(2, delay(100));
        let br = b.add_bridge(2, Bits::new(64), delay(500));
        let world = b.build().unwrap();
        assert!(matches!(world.node(s), Some(NodeKind::EndStation(_))));
        assert!(matches!(world.node(r), Some(NodeKind::Repeater(_))));
        assert!(matches!(world.node(br), Some(NodeKind::Bridge(_))));
    }

    // -- Errors -------------------------------------------------------------

    #[test]
    fn build_error_implements_error_trait_and_displays() {
        let err = BuildError::UnknownNode { node: NodeId::new(42) };
        let _: &dyn core::error::Error = &err;
        let msg = format!("{err}");
        assert!(msg.contains("unknown node"));
    }

    #[test]
    fn unique_path_violated_message_names_a_root() {
        let mut b = TopologyBuilder::new();
        let s1 = b.add_end_station(2);
        let s2 = b.add_end_station(2);
        b.add_hd_segment(BitRate::ETHERNET_10M, delay(1_000), ep(s1, 0), ep(s2, 0))
            .unwrap();
        b.add_hd_segment(BitRate::ETHERNET_10M, delay(1_000), ep(s1, 1), ep(s2, 1))
            .unwrap();
        let err = b.build().unwrap_err();
        assert!(matches!(err, BuildError::UniquePathViolated { .. }));
    }
}
