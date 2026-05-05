//! Bridge frame-relay primitives.
//!
//! Three pieces, each independently testable:
//!
//! 1. [`frame_eligibility_time`] — pure function computing when an ingress
//!    frame becomes eligible for egress queueing, per `report_1.md`
//!    Definition: `t_elig = t_firstbit_in + η_b / R_in + π_b`.
//! 2. [`EgressQueue<T>`] — generic FIFO with optional capacity, used per
//!    bridge egress port to hold frames waiting for the serializer.
//! 3. [`Forwarding<F>`] trait + [`FloodForwarding`] default — forwarding
//!    decisions for an ingress frame, parameterized over the frame type
//!    (engine round 8 will provide concrete frame types and possibly
//!    other forwarding implementations).
//!
//! The bridge module emits no events. Round 8 (engine) holds per-bridge
//! state, calls these primitives at ingress and egress times, and emits
//! the corresponding `FrameEligible`, `Enqueue`, `Dequeue`, and
//! `TxStart` / `TxEnd` events on the bridge's egress serializers.

use crate::time::{BitRate, BitTime, Bits};
use crate::topology::PortId;

use std::collections::VecDeque;

// ===========================================================================
// frame_eligibility_time
// ===========================================================================

/// The time at which an ingress frame becomes eligible for egress queueing
/// at a bridge.
///
/// Implements `t_elig = t_firstbit_in + η_b / R_in + π_b` from
/// `report_1.md` §"Formal primitives and axioms" (bridge definition).
///
/// # Cut-through vs. store-and-forward
///
/// - **Cut-through**: `eta_b` is a header threshold (e.g., 64 bits to read
///   the destination MAC). Eligibility fires before the frame finishes
///   arriving on the ingress port.
/// - **Store-and-forward**: `eta_b` is the full wire length of the frame.
///   Eligibility fires after the entire frame has arrived.
///
/// Both modes use the same formula; the bridge's [`crate::topology::BridgeData`]
/// `decode_threshold` field (= `eta_b`) selects the mode per bridge.
///
/// # Examples
///
/// ```
/// use aether_sonde::bridge::frame_eligibility_time;
/// use aether_sonde::time::{BitRate, BitTime, Bits};
///
/// // Cut-through: 64 bits at 1 Gbps = 64 ns; +500 ns processing = 564 ns.
/// assert_eq!(
///     frame_eligibility_time(
///         BitTime::ZERO,
///         Bits::new(64),
///         BitRate::ETHERNET_1G,
///         BitTime::from_nanos(500),
///     ),
///     BitTime::from_nanos(564),
/// );
/// ```
///
/// # Panics
///
/// Panics on `BitTime` overflow during the addition. For any realistic
/// combination of parameters this cannot happen — a sum of bridge timing
/// quantities at IEEE rates fits well within `u64` picoseconds (~213 days).
#[must_use]
#[track_caller]
pub fn frame_eligibility_time(
    t_first_bit_in: BitTime,
    eta_b: Bits,
    ingress_rate: BitRate,
    pi_b: BitTime,
) -> BitTime {
    t_first_bit_in + eta_b.at_rate(ingress_rate) + pi_b
}

// ===========================================================================
// EgressQueue<T>
// ===========================================================================

/// A FIFO queue of pending items at a bridge egress port.
///
/// Generic over the item type so the engine (round 8) can decide whether
/// to queue raw signals, framed transmissions, or higher-level frame
/// records. Items are dequeued in the order they were enqueued.
///
/// Optional capacity bounds the queue; on overflow, [`enqueue`] returns
/// `Err(QueueFull { item })` so the caller can drop, log, or reroute the
/// rejected item.
///
/// [`enqueue`]: EgressQueue::enqueue
///
/// # Examples
///
/// ```
/// use aether_sonde::bridge::EgressQueue;
///
/// let mut q = EgressQueue::<&'static str>::new();
/// q.enqueue("a").unwrap();
/// q.enqueue("b").unwrap();
/// assert_eq!(q.len(), 2);
/// assert_eq!(q.dequeue(), Some("a"));
/// assert_eq!(q.dequeue(), Some("b"));
/// assert_eq!(q.dequeue(), None);
/// ```
#[derive(Debug, Clone)]
pub struct EgressQueue<T> {
    items: VecDeque<T>,
    capacity: Option<usize>,
}

impl<T> Default for EgressQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> EgressQueue<T> {
    /// Construct an unbounded queue.
    #[must_use]
    pub fn new() -> Self {
        Self {
            items: VecDeque::new(),
            capacity: None,
        }
    }

    /// Construct a queue with a fixed maximum capacity.
    ///
    /// Capacity 0 makes the queue always-full: every [`enqueue`] returns
    /// `Err(QueueFull)`.
    ///
    /// [`enqueue`]: EgressQueue::enqueue
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::bridge::EgressQueue;
    ///
    /// let mut q = EgressQueue::<i32>::with_capacity(2);
    /// assert_eq!(q.capacity(), Some(2));
    /// q.enqueue(1).unwrap();
    /// q.enqueue(2).unwrap();
    /// assert!(q.enqueue(3).is_err());
    /// ```
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            items: VecDeque::with_capacity(capacity),
            capacity: Some(capacity),
        }
    }

    /// Enqueue an item at the tail.
    ///
    /// # Errors
    ///
    /// Returns [`QueueFull`] (carrying the rejected item) if the queue
    /// has a capacity and is already at it.
    pub fn enqueue(&mut self, item: T) -> Result<(), QueueFull<T>> {
        if let Some(cap) = self.capacity
            && self.items.len() >= cap
        {
            return Err(QueueFull { item });
        }
        self.items.push_back(item);
        Ok(())
    }

    /// Remove and return the head item, or `None` if empty.
    pub fn dequeue(&mut self) -> Option<T> {
        self.items.pop_front()
    }

    /// Return a reference to the head item without removing it.
    #[must_use]
    pub fn peek(&self) -> Option<&T> {
        self.items.front()
    }

    /// The current number of queued items.
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the queue is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// The configured capacity, or `None` if unbounded.
    #[must_use]
    pub fn capacity(&self) -> Option<usize> {
        self.capacity
    }
}

/// Returned by [`EgressQueue::enqueue`] when the queue is at capacity.
///
/// Carries the rejected item back to the caller so it can be logged,
/// dropped, or rerouted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QueueFull<T> {
    /// The item that was rejected.
    pub item: T,
}

impl<T> core::fmt::Display for QueueFull<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("egress queue is at capacity")
    }
}

impl<T: core::fmt::Debug> core::error::Error for QueueFull<T> {}

// ===========================================================================
// Forwarding<F>
// ===========================================================================

/// A bridge forwarding policy: given an ingress frame and the bridge's
/// port set, produce the egress ports.
///
/// Generic over the frame type so different bridges (and different
/// project rounds) can use different frame schemas. Round 6 ships
/// [`FloodForwarding`] as the simplest correct policy; learning,
/// VLAN-aware, and other policies arrive when a use case justifies them.
pub trait Forwarding<F> {
    /// The egress ports for `frame` arriving on `ingress`.
    ///
    /// `all_ports` is the bridge's full port set; the policy may consult
    /// it (as [`FloodForwarding`] does) or ignore it (as a static-table
    /// policy might).
    fn egress_ports(&self, frame: &F, ingress: PortId, all_ports: &[PortId]) -> Vec<PortId>;
}

/// Flood-everything forwarding: send to every port in `all_ports` except
/// `ingress`.
///
/// The simplest correct forwarding policy. Suitable for bridges that
/// don't yet have address-based forwarding state.
///
/// # Examples
///
/// ```
/// use aether_sonde::bridge::{FloodForwarding, Forwarding};
/// use aether_sonde::topology::PortId;
///
/// let policy = FloodForwarding;
/// let all = [PortId::new(0), PortId::new(1), PortId::new(2), PortId::new(3)];
/// let egress = policy.egress_ports(&(), PortId::new(1), &all);
/// assert_eq!(egress, vec![PortId::new(0), PortId::new(2), PortId::new(3)]);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FloodForwarding;

impl<F> Forwarding<F> for FloodForwarding {
    fn egress_ports(&self, _frame: &F, ingress: PortId, all_ports: &[PortId]) -> Vec<PortId> {
        all_ports
            .iter()
            .copied()
            .filter(|&p| p != ingress)
            .collect()
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

    // -- frame_eligibility_time ---------------------------------------------

    #[test]
    fn eligibility_time_cut_through_with_64_bit_threshold_at_1g() {
        // 64 bits at 1 Gbps = 64 ns. +500 ns processing = 564 ns.
        assert_eq!(
            frame_eligibility_time(
                BitTime::ZERO,
                Bits::new(64),
                BitRate::ETHERNET_1G,
                BitTime::from_nanos(500),
            ),
            BitTime::from_nanos(564),
        );
    }

    #[test]
    fn eligibility_time_store_and_forward_full_frame_at_1g() {
        // 12_000 bits at 1 Gbps = 12 µs. +500 ns processing = 12.5 µs.
        assert_eq!(
            frame_eligibility_time(
                BitTime::ZERO,
                Bits::new(12_000),
                BitRate::ETHERNET_1G,
                BitTime::from_nanos(500),
            ),
            BitTime::from_nanos(12_500),
        );
    }

    #[test]
    fn eligibility_time_heterogeneous_rate_at_10m_ingress() {
        // 512 bits at 10 Mbps = 51.2 µs. +1 µs processing = 52.2 µs.
        assert_eq!(
            frame_eligibility_time(
                BitTime::ZERO,
                Bits::new(512),
                BitRate::ETHERNET_10M,
                BitTime::from_nanos(1_000),
            ),
            BitTime::from_nanos(52_200),
        );
    }

    #[test]
    fn eligibility_time_zero_processing_delay() {
        // pi_b = 0 reduces to pure ingress decode time.
        assert_eq!(
            frame_eligibility_time(
                BitTime::ZERO,
                Bits::new(64),
                BitRate::ETHERNET_1G,
                BitTime::ZERO,
            ),
            BitTime::from_nanos(64),
        );
    }

    #[test]
    fn eligibility_time_t0_offset_is_added_linearly() {
        // For non-zero t_first_bit_in, result = t0 + (eta_b/R) + pi_b.
        let t0 = BitTime::from_micros(7);
        let eta = Bits::new(64);
        let rate = BitRate::ETHERNET_1G;
        let pi = BitTime::from_nanos(100);
        // 64 ns + 100 ns + 7000 ns = 7164 ns.
        assert_eq!(
            frame_eligibility_time(t0, eta, rate, pi),
            BitTime::from_nanos(7_164),
        );
    }

    // -- EgressQueue: FIFO discipline ----------------------------------------

    #[test]
    fn egress_queue_new_is_empty_and_unbounded() {
        let q = EgressQueue::<i32>::new();
        assert!(q.is_empty());
        assert_eq!(q.len(), 0);
        assert_eq!(q.capacity(), None);
        assert!(q.peek().is_none());
    }

    #[test]
    fn egress_queue_default_matches_new() {
        let a: EgressQueue<i32> = EgressQueue::default();
        let b: EgressQueue<i32> = EgressQueue::new();
        assert_eq!(a.is_empty(), b.is_empty());
        assert_eq!(a.capacity(), b.capacity());
    }

    #[test]
    fn egress_queue_fifo_order() {
        let mut q = EgressQueue::<i32>::new();
        q.enqueue(1).unwrap();
        q.enqueue(2).unwrap();
        q.enqueue(3).unwrap();
        assert_eq!(q.dequeue(), Some(1));
        assert_eq!(q.dequeue(), Some(2));
        assert_eq!(q.dequeue(), Some(3));
        assert_eq!(q.dequeue(), None);
    }

    #[test]
    fn egress_queue_len_tracks_count() {
        let mut q = EgressQueue::<i32>::new();
        assert_eq!(q.len(), 0);
        q.enqueue(1).unwrap();
        assert_eq!(q.len(), 1);
        q.enqueue(2).unwrap();
        assert_eq!(q.len(), 2);
        q.dequeue();
        assert_eq!(q.len(), 1);
        q.dequeue();
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn egress_queue_peek_does_not_consume() {
        let mut q = EgressQueue::<i32>::new();
        q.enqueue(42).unwrap();
        assert_eq!(q.peek(), Some(&42));
        assert_eq!(q.peek(), Some(&42));
        assert_eq!(q.len(), 1);
        assert_eq!(q.dequeue(), Some(42));
        assert!(q.peek().is_none());
    }

    // -- EgressQueue: capacity -----------------------------------------------

    #[test]
    fn egress_queue_unbounded_accepts_arbitrary_enqueues() {
        let mut q = EgressQueue::<i32>::new();
        for i in 0..1_000 {
            q.enqueue(i).unwrap();
        }
        assert_eq!(q.len(), 1_000);
    }

    #[test]
    fn egress_queue_with_capacity_rejects_at_limit() {
        let mut q = EgressQueue::<i32>::with_capacity(2);
        assert_eq!(q.capacity(), Some(2));
        q.enqueue(1).unwrap();
        q.enqueue(2).unwrap();
        let err = q.enqueue(99).unwrap_err();
        assert_eq!(err.item, 99);
        // Existing items still in queue.
        assert_eq!(q.len(), 2);
    }

    #[test]
    fn egress_queue_capacity_zero_always_full() {
        let mut q = EgressQueue::<i32>::with_capacity(0);
        let err = q.enqueue(1).unwrap_err();
        assert_eq!(err.item, 1);
        assert!(q.is_empty());
    }

    #[test]
    fn egress_queue_dequeue_then_re_enqueue_after_capacity_hit() {
        let mut q = EgressQueue::<i32>::with_capacity(2);
        q.enqueue(1).unwrap();
        q.enqueue(2).unwrap();
        // Full.
        assert!(q.enqueue(3).is_err());
        // Free a slot.
        assert_eq!(q.dequeue(), Some(1));
        // Now enqueue succeeds.
        q.enqueue(3).unwrap();
        assert_eq!(q.len(), 2);
        assert_eq!(q.dequeue(), Some(2));
        assert_eq!(q.dequeue(), Some(3));
    }

    // -- QueueFull error trait -----------------------------------------------

    #[test]
    fn queue_full_implements_error_trait_when_item_is_debug() {
        let err: QueueFull<i32> = QueueFull { item: 7 };
        let _: &dyn core::error::Error = &err;
        let msg = format!("{err}");
        assert!(msg.contains("capacity"));
    }

    // -- FloodForwarding -----------------------------------------------------

    #[test]
    fn flood_forwarding_excludes_ingress() {
        let policy = FloodForwarding;
        let all = [
            PortId::new(0),
            PortId::new(1),
            PortId::new(2),
            PortId::new(3),
        ];
        let egress = policy.egress_ports(&(), PortId::new(1), &all);
        assert_eq!(egress, vec![PortId::new(0), PortId::new(2), PortId::new(3)],);
    }

    #[test]
    fn flood_forwarding_preserves_order() {
        let policy = FloodForwarding;
        let all = [
            PortId::new(7),
            PortId::new(2),
            PortId::new(5),
            PortId::new(0),
        ];
        let egress = policy.egress_ports(&(), PortId::new(2), &all);
        assert_eq!(egress, vec![PortId::new(7), PortId::new(5), PortId::new(0)]);
    }

    #[test]
    fn flood_forwarding_works_with_arbitrary_frame_type() {
        // Generic F: a custom struct works as well as ().
        struct CustomFrame {
            _payload: [u8; 4],
        }
        let frame = CustomFrame {
            _payload: [1, 2, 3, 4],
        };
        let policy = FloodForwarding;
        let all = [PortId::new(0), PortId::new(1)];
        let egress = policy.egress_ports(&frame, PortId::new(0), &all);
        assert_eq!(egress, vec![PortId::new(1)]);
    }

    #[test]
    fn flood_forwarding_single_port_bridge_returns_empty() {
        let policy = FloodForwarding;
        let all = [PortId::new(0)];
        let egress = policy.egress_ports(&(), PortId::new(0), &all);
        assert!(egress.is_empty());
    }

    #[test]
    fn flood_forwarding_ingress_not_in_port_set_returns_all_ports() {
        // Defensive: if the ingress port isn't in all_ports (a topology
        // construction error), every port in all_ports is returned. The
        // caller should validate this case upstream; the policy doesn't
        // panic.
        let policy = FloodForwarding;
        let all = [PortId::new(0), PortId::new(1)];
        let egress = policy.egress_ports(&(), PortId::new(99), &all);
        assert_eq!(egress, vec![PortId::new(0), PortId::new(1)]);
    }
}
