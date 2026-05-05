//! Ethernet frame types: addresses, ethertype, VLAN tag, payload.
//!
//! The simulator models a frame at MAC-layer granularity — the
//! semantic fields a link-layer device inspects (destination MAC,
//! source MAC, ethertype, VLAN tag) plus a payload that is
//! opaque-by-default but extensible. Byte-level wire layout
//! (preamble, SFD, FCS bytes) is not modeled; CRC validity is not
//! tracked. When a future device family needs that detail, the type
//! extends additively.
//!
//! # Backwards compatibility
//!
//! Existing engine APIs that take a `bits: Bits` argument
//! (`Engine::register_frame`) construct a [`Frame::opaque`] internally
//! whose [`Frame::wire_length`] returns the same `bits` value. Tests
//! that only care about timing — the bulk of the round-3 and earlier
//! suite — see no API change.

use crate::time::Bits;

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// MacAddress
// ---------------------------------------------------------------------------

/// IEEE 802 MAC address — six octets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(transparent))]
pub struct MacAddress(pub [u8; 6]);

impl MacAddress {
    /// The all-ones broadcast address `ff:ff:ff:ff:ff:ff`.
    pub const BROADCAST: Self = Self([0xff; 6]);
    /// The all-zeros sentinel address (used by [`Frame::opaque`]).
    pub const ZERO: Self = Self([0; 6]);

    /// Construct from a raw 6-byte array.
    #[must_use]
    pub const fn new(octets: [u8; 6]) -> Self {
        Self(octets)
    }

    /// The underlying octets.
    #[must_use]
    pub const fn as_octets(self) -> [u8; 6] {
        self.0
    }

    /// True iff this is the all-ones broadcast address.
    #[must_use]
    pub const fn is_broadcast(self) -> bool {
        self.0[0] == 0xff
            && self.0[1] == 0xff
            && self.0[2] == 0xff
            && self.0[3] == 0xff
            && self.0[4] == 0xff
            && self.0[5] == 0xff
    }

    /// True iff the I/G bit is set (low bit of the first octet).
    /// Includes the broadcast address.
    #[must_use]
    pub const fn is_multicast(self) -> bool {
        self.0[0] & 0x01 != 0
    }

    /// True iff the address is unicast (I/G bit clear).
    #[must_use]
    pub const fn is_unicast(self) -> bool {
        self.0[0] & 0x01 == 0
    }
}

// ---------------------------------------------------------------------------
// EtherType
// ---------------------------------------------------------------------------

/// IEEE 802.3 `EtherType` / length field.
///
/// Values ≥ `0x0600` are `EtherType` discriminants (Ethernet II framing);
/// values `< 0x0600` are length fields (IEEE 802.3 framing). The
/// simulator does not enforce this distinction; it stores whatever
/// 16-bit value the frame was constructed with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Default)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(transparent))]
pub struct EtherType(pub u16);

impl EtherType {
    /// IPv4 (0x0800).
    pub const IPV4: Self = Self(0x0800);
    /// ARP (0x0806).
    pub const ARP: Self = Self(0x0806);
    /// IPv6 (0x86DD).
    pub const IPV6: Self = Self(0x86DD);
    /// 802.1Q VLAN tag (0x8100).
    pub const VLAN: Self = Self(0x8100);
    /// Sentinel used by [`Frame::opaque`] for tests/scenarios that
    /// don't care about the ethertype.
    pub const OPAQUE: Self = Self(0x0000);
}

// ---------------------------------------------------------------------------
// VlanTag (802.1Q)
// ---------------------------------------------------------------------------

/// 802.1Q VLAN tag: priority (PCP), drop-eligible (DEI), VLAN ID (VID).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct VlanTag {
    /// Priority Code Point (3 bits, 0–7).
    pub priority: u8,
    /// Drop-Eligible Indicator (1 bit).
    pub drop_eligible: bool,
    /// VLAN ID (12 bits, 0–4095). 0 and 4095 are reserved per 802.1Q.
    pub vid: u16,
}

// ---------------------------------------------------------------------------
// FramePayload
// ---------------------------------------------------------------------------

/// The payload of a [`Frame`]. Sealed: future typed payloads
/// (`Arp(ArpPacket)`, `Icmp(IcmpPacket)`, etc.) get added as new
/// variants when a device family needs to inspect them.
///
/// `Opaque` carries a bit count instead of a byte string because the
/// simulator currently models propagation timing only; payload
/// contents are not yet a behavioral input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize), serde(tag = "type"))]
pub enum FramePayload {
    /// "Just bits" — used by tests and existing scenarios that don't
    /// care about contents. The variant exists so we don't pretend
    /// to have a payload structure when we don't.
    Opaque {
        /// Total wire length of the frame in bits, including header
        /// and any framing overhead the test cares to model. The
        /// engine derives signal duration from this value.
        bits: Bits,
    },
}

// ---------------------------------------------------------------------------
// Frame
// ---------------------------------------------------------------------------

/// An Ethernet II / 802.3 / 802.1Q frame.
///
/// Modeled at MAC-layer granularity; byte-level wire layout
/// (preamble, SFD, FCS) is not represented. Round 4 introduces
/// `Frame` to support stateful link-layer devices that read headers
/// (e.g., a learning switch); future rounds add typed payload
/// variants when device families need them.
///
/// `Copy` is derived in round 4 because all current payload variants
/// are `Copy`. When non-`Copy` payload variants are added (e.g., a
/// payload carrying an owned `Vec<u8>`), `Copy` will be dropped here
/// and `FrameMetadata` will be migrated accordingly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
pub struct Frame {
    /// Destination MAC address.
    pub destination: MacAddress,
    /// Source MAC address.
    pub source: MacAddress,
    /// `EtherType` / length field.
    pub ethertype: EtherType,
    /// Optional 802.1Q VLAN tag.
    pub vlan: Option<VlanTag>,
    /// Frame payload.
    pub payload: FramePayload,
}

impl Frame {
    /// Construct an opaque-payload frame with anonymous addressing.
    ///
    /// Used by the engine to wrap callers of the legacy
    /// `Engine::register_frame(node, bits, kind, rate)` API. The
    /// resulting frame has `MacAddress::ZERO` for both endpoints,
    /// `EtherType::OPAQUE`, no VLAN tag, and an opaque payload of
    /// `bits` bits — preserving the prior wire-length semantics
    /// exactly.
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::frame::{Frame, MacAddress};
    /// use aether_sonde::time::Bits;
    /// let f = Frame::opaque(Bits::new(512));
    /// assert_eq!(f.destination, MacAddress::ZERO);
    /// assert_eq!(f.source, MacAddress::ZERO);
    /// assert_eq!(f.wire_length(), Bits::new(512));
    /// ```
    #[must_use]
    pub const fn opaque(bits: Bits) -> Self {
        Self {
            destination: MacAddress::ZERO,
            source: MacAddress::ZERO,
            ethertype: EtherType::OPAQUE,
            vlan: None,
            payload: FramePayload::Opaque { bits },
        }
    }

    /// Construct an Ethernet II frame between explicit MAC addresses
    /// with an opaque payload sized at `wire_bits`.
    ///
    /// `wire_bits` is the total wire length the engine will use for
    /// signal duration. For tests that don't model header overhead
    /// separately, pass the full frame size you want on the wire.
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::frame::{EtherType, Frame, MacAddress};
    /// use aether_sonde::time::Bits;
    /// let f = Frame::ethernet(
    ///     MacAddress::new([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]),
    ///     MacAddress::new([0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f]),
    ///     EtherType::IPV4,
    ///     Bits::new(1500 * 8),
    /// );
    /// assert_eq!(f.ethertype, EtherType::IPV4);
    /// ```
    #[must_use]
    pub const fn ethernet(
        destination: MacAddress,
        source: MacAddress,
        ethertype: EtherType,
        wire_bits: Bits,
    ) -> Self {
        Self {
            destination,
            source,
            ethertype,
            vlan: None,
            payload: FramePayload::Opaque { bits: wire_bits },
        }
    }

    /// Add an 802.1Q VLAN tag, returning the modified frame.
    #[must_use]
    pub const fn with_vlan(mut self, tag: VlanTag) -> Self {
        self.vlan = Some(tag);
        self
    }

    /// Total wire length of the frame in bits.
    ///
    /// For the round-4 opaque-payload model, this returns the bit
    /// count stored in the payload. When typed payload variants are
    /// added in future rounds, this method will compute the total
    /// from headers + payload + any framing overhead the variant
    /// chooses to model.
    #[must_use]
    pub const fn wire_length(&self) -> Bits {
        match self.payload {
            FramePayload::Opaque { bits } => bits,
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
    fn mac_address_broadcast_constants() {
        assert!(MacAddress::BROADCAST.is_broadcast());
        assert!(MacAddress::BROADCAST.is_multicast());
        assert!(!MacAddress::BROADCAST.is_unicast());
        assert!(!MacAddress::ZERO.is_broadcast());
        assert!(!MacAddress::ZERO.is_multicast());
        assert!(MacAddress::ZERO.is_unicast());
    }

    #[test]
    fn mac_address_unicast_vs_multicast_via_ig_bit() {
        // I/G bit is the low bit of the first octet.
        let unicast = MacAddress::new([0x00, 0x11, 0x22, 0x33, 0x44, 0x55]);
        let multicast = MacAddress::new([0x01, 0x00, 0x5e, 0x00, 0x00, 0x01]);
        assert!(unicast.is_unicast());
        assert!(!unicast.is_multicast());
        assert!(multicast.is_multicast());
        assert!(!multicast.is_unicast());
    }

    #[test]
    fn ethertype_constants() {
        assert_eq!(EtherType::IPV4.0, 0x0800);
        assert_eq!(EtherType::ARP.0, 0x0806);
        assert_eq!(EtherType::IPV6.0, 0x86DD);
        assert_eq!(EtherType::VLAN.0, 0x8100);
        assert_eq!(EtherType::OPAQUE.0, 0x0000);
    }

    #[test]
    fn opaque_frame_round_trip_preserves_bits() {
        let f = Frame::opaque(Bits::new(512));
        assert_eq!(f.wire_length(), Bits::new(512));
        assert_eq!(f.destination, MacAddress::ZERO);
        assert_eq!(f.source, MacAddress::ZERO);
        assert_eq!(f.ethertype, EtherType::OPAQUE);
        assert!(f.vlan.is_none());
    }

    #[test]
    fn ethernet_frame_carries_addresses() {
        let dst = MacAddress::new([0x01, 0x02, 0x03, 0x04, 0x05, 0x06]);
        let src = MacAddress::new([0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff]);
        let f = Frame::ethernet(dst, src, EtherType::IPV4, Bits::new(1500 * 8));
        assert_eq!(f.destination, dst);
        assert_eq!(f.source, src);
        assert_eq!(f.ethertype, EtherType::IPV4);
        assert!(f.vlan.is_none());
        assert_eq!(f.wire_length(), Bits::new(1500 * 8));
    }

    #[test]
    fn vlan_tag_attaches_via_with_vlan() {
        let f = Frame::ethernet(
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
        assert_eq!(f.vlan.unwrap().priority, 5);
        assert_eq!(f.vlan.unwrap().vid, 100);
        assert!(!f.vlan.unwrap().drop_eligible);
    }

    #[test]
    fn frame_is_copy_in_round_4() {
        let a = Frame::opaque(Bits::new(64));
        let b = a; // Copy
        // `a` is still usable post-copy because Frame: Copy.
        assert_eq!(a, b);
        assert_eq!(a.wire_length(), Bits::new(64));
    }
}
