//! Canonical time, bit count, and bit rate types.
//!
//! All time arithmetic in the simulator passes through these three types.
//! They collectively guarantee:
//!
//! * **No floats.** Integer arithmetic is exact for rational inputs; the
//!   simulator's reproducibility relies on this and floats would re-introduce
//!   precision risk at every comparison.
//! * **Heterogeneous-rate composition.** A single canonical absolute time
//!   unit (the picosecond) lets durations from different bit-rate segments
//!   compose. [`BitTime`] is `u64` picoseconds.
//! * **Explicit rate context.** The conversion from a bit count to a
//!   duration requires a rate. There is no implicit `From` across that
//!   boundary; callers always pass the rate in.
//!
//! # Why picoseconds
//!
//! Every standard Ethernet bit rate divides 1 picosecond exactly:
//!
//! | Rate         | bit-period |
//! |--------------|-----------:|
//! | 10 Mbps      |   100 ns   |
//! | 100 Mbps     |    10 ns   |
//! | 1 Gbps       |     1 ns   |
//! | 10 Gbps      |   100 ps   |
//!
//! All of these are integer picoseconds, so [`BitTime`] represents
//! bit-times at any standard rate without rounding.
//!
//! # Range
//!
//! `u64` picoseconds spans roughly 213 days. Arithmetic that overflows this
//! range panics with a message naming the operation; overflow indicates the
//! inputs are wrong, not that the type needs widening.

use core::num::NonZeroU64;
use core::ops::{Add, AddAssign, Sub, SubAssign};

/// Picoseconds per second. Conversion constant from a (bit count, rate)
/// pair to a [`BitTime`] duration.
const PICOSECONDS_PER_SECOND: u64 = 1_000_000_000_000;

/// Construct a [`NonZeroU64`] from a `u64` known by the caller to be nonzero.
///
/// Used for standard-rate constants whose input is a nonzero literal. The
/// `None` arm is statically dead at every call site in this module.
//
// RATIONALE: callers in this module pass only nonzero literals
// (e.g. ETHERNET_10M = 10_000_000), so the `None` arm is statically dead.
// Avoiding NonZeroU64::new_unchecked keeps the workspace-level
// `unsafe_code = "forbid"` rule intact.
#[allow(clippy::unreachable)]
const fn nz(n: u64) -> NonZeroU64 {
    match NonZeroU64::new(n) {
        Some(value) => value,
        None => unreachable!(),
    }
}

// ---------------------------------------------------------------------------
// BitTime
// ---------------------------------------------------------------------------

/// A time interval, measured in picoseconds.
///
/// `BitTime` is the canonical absolute time unit for the simulator. Names
/// like "front-arrival time", "slot time", and "inter-frame gap" are all
/// `BitTime` values. The unit is picoseconds because every standard Ethernet
/// bit rate divides 1 ps exactly, so 1 bit-period at any standard rate is
/// an integer number of picoseconds and arithmetic stays exact.
///
/// # Examples
///
/// ```
/// use aether_sonde::time::BitTime;
///
/// assert_eq!(BitTime::from_nanos(1).as_u64(), 1_000);
/// assert_eq!(BitTime::from_micros(1).as_u64(), 1_000_000);
/// assert!(BitTime::from_nanos(1) < BitTime::from_micros(1));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct BitTime(u64);

impl BitTime {
    /// Zero duration.
    pub const ZERO: Self = Self(0);

    /// Construct a `BitTime` from a raw picosecond count.
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::time::BitTime;
    /// assert_eq!(BitTime::new(500).as_u64(), 500);
    /// ```
    #[must_use]
    pub const fn new(picoseconds: u64) -> Self {
        Self(picoseconds)
    }

    /// Construct a `BitTime` from a nanosecond count.
    ///
    /// # Panics
    ///
    /// Panics if `nanoseconds * 1_000` overflows `u64` (`nanoseconds`
    /// > 18_446_744_073_709_551 ns ≈ 213 days).
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::time::BitTime;
    /// assert_eq!(BitTime::from_nanos(100), BitTime::new(100_000));
    /// ```
    //
    // RATIONALE for the panic: integer-time exactness is load-bearing. An
    // input beyond ~213 days of nanoseconds is a wrong input, not a
    // recoverable condition (per [Result vs Panic]).
    #[allow(clippy::panic)]
    #[must_use]
    #[track_caller]
    pub const fn from_nanos(nanoseconds: u64) -> Self {
        match nanoseconds.checked_mul(1_000) {
            Some(ps) => Self(ps),
            None => panic!("BitTime overflow in from_nanos"),
        }
    }

    /// Construct a `BitTime` from a microsecond count.
    ///
    /// # Panics
    ///
    /// Panics if `microseconds * 1_000_000` overflows `u64`.
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::time::BitTime;
    /// assert_eq!(BitTime::from_micros(1).as_u64(), 1_000_000);
    /// ```
    #[allow(clippy::panic)]
    #[must_use]
    #[track_caller]
    pub const fn from_micros(microseconds: u64) -> Self {
        match microseconds.checked_mul(1_000_000) {
            Some(ps) => Self(ps),
            None => panic!("BitTime overflow in from_micros"),
        }
    }

    /// Construct a `BitTime` from a millisecond count.
    ///
    /// # Panics
    ///
    /// Panics if `milliseconds * 1_000_000_000` overflows `u64`.
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::time::BitTime;
    /// assert_eq!(BitTime::from_millis(1).as_u64(), 1_000_000_000);
    /// ```
    #[allow(clippy::panic)]
    #[must_use]
    #[track_caller]
    pub const fn from_millis(milliseconds: u64) -> Self {
        match milliseconds.checked_mul(1_000_000_000) {
            Some(ps) => Self(ps),
            None => panic!("BitTime overflow in from_millis"),
        }
    }

    /// The underlying picosecond count.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

impl Add for BitTime {
    type Output = Self;

    #[inline]
    #[track_caller]
    fn add(self, rhs: Self) -> Self {
        match self.0.checked_add(rhs.0) {
            Some(sum) => Self(sum),
            None => {
                // RATIONALE: see bittime_overflow above. Arithmetic-level
                // panic is also a should-be-impossible condition under the
                // simulator's stated 213-day horizon.
                #[allow(clippy::panic)]
                {
                    panic!("BitTime overflow in addition: {} + {} ps", self.0, rhs.0)
                }
            }
        }
    }
}

impl AddAssign for BitTime {
    #[inline]
    #[track_caller]
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl Sub for BitTime {
    type Output = Self;

    #[inline]
    #[track_caller]
    fn sub(self, rhs: Self) -> Self {
        match self.0.checked_sub(rhs.0) {
            Some(diff) => Self(diff),
            None => {
                // RATIONALE: BitTime is an unsigned duration; underflow
                // means the caller subtracted t1 - t2 with t2 > t1, a
                // logic error. Per [Result vs Panic], panic.
                #[allow(clippy::panic)]
                {
                    panic!(
                        "BitTime underflow in subtraction: {} - {} ps",
                        self.0, rhs.0
                    )
                }
            }
        }
    }
}

impl SubAssign for BitTime {
    #[inline]
    #[track_caller]
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

// ---------------------------------------------------------------------------
// Bits
// ---------------------------------------------------------------------------

/// A bit count, used for frame lengths, jam sizes, and similar quantities.
///
/// `Bits` is rate-independent. Conversion to a [`BitTime`] duration requires
/// an explicit [`BitRate`] via [`Bits::at_rate`] — there is no implicit
/// conversion across the rate boundary.
///
/// # Examples
///
/// ```
/// use aether_sonde::time::{BitRate, BitTime, Bits};
///
/// // At 10 Mbps, 1 bit takes 100 ns.
/// assert_eq!(Bits::new(1).at_rate(BitRate::ETHERNET_10M), BitTime::from_nanos(100));
/// // At 1 Gbps, 1 bit takes 1 ns.
/// assert_eq!(Bits::new(1).at_rate(BitRate::ETHERNET_1G), BitTime::from_nanos(1));
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Bits(u64);

impl Bits {
    /// Zero bits.
    pub const ZERO: Self = Self(0);

    /// Construct a `Bits` value from a raw bit count.
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::time::Bits;
    /// assert_eq!(Bits::new(512).as_u64(), 512);
    /// ```
    #[must_use]
    pub const fn new(count: u64) -> Self {
        Self(count)
    }

    /// The underlying bit count.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// The duration to transmit this many bits at the given rate.
    ///
    /// Computed as `bits * 1e12 / rate_bps`, with a `u128` intermediate to
    /// avoid mid-computation overflow.
    ///
    /// # Panics
    ///
    /// Panics if the result would exceed `u64::MAX` picoseconds (~213 days).
    /// For any realistic combination of bit count and rate this cannot
    /// happen — at 10 Mbps, ~213 days corresponds to 1.84e14 bits, which
    /// is far beyond any plausible simulation input.
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::time::{BitRate, BitTime, Bits};
    ///
    /// // 512-bit minimum frame at 10 Mbps takes 51.2 µs.
    /// assert_eq!(
    ///     Bits::new(512).at_rate(BitRate::ETHERNET_10M),
    ///     BitTime::from_nanos(51_200),
    /// );
    /// ```
    #[must_use]
    #[track_caller]
    pub const fn at_rate(self, rate: BitRate) -> BitTime {
        // bits / (bits/sec) = seconds; * 1e12 = picoseconds.
        // u128 intermediate avoids overflow during multiplication.
        let numerator: u128 = (self.0 as u128) * (PICOSECONDS_PER_SECOND as u128);
        let ps_u128: u128 = numerator / (rate.0.get() as u128);
        assert!(
            ps_u128 <= u64::MAX as u128,
            "BitTime overflow: bit count too large for u64 picoseconds at this rate",
        );
        // RATIONALE: bound checked by the assert above; cast is lossless.
        #[allow(clippy::cast_possible_truncation)]
        BitTime(ps_u128 as u64)
    }
}

impl Add for Bits {
    type Output = Self;

    #[inline]
    #[track_caller]
    fn add(self, rhs: Self) -> Self {
        match self.0.checked_add(rhs.0) {
            Some(sum) => Self(sum),
            None => {
                #[allow(clippy::panic)]
                {
                    panic!("Bits overflow in addition: {} + {} bits", self.0, rhs.0)
                }
            }
        }
    }
}

impl AddAssign for Bits {
    #[inline]
    #[track_caller]
    fn add_assign(&mut self, rhs: Self) {
        *self = *self + rhs;
    }
}

impl Sub for Bits {
    type Output = Self;

    #[inline]
    #[track_caller]
    fn sub(self, rhs: Self) -> Self {
        match self.0.checked_sub(rhs.0) {
            Some(diff) => Self(diff),
            None => {
                #[allow(clippy::panic)]
                {
                    panic!("Bits underflow in subtraction: {} - {} bits", self.0, rhs.0)
                }
            }
        }
    }
}

impl SubAssign for Bits {
    #[inline]
    #[track_caller]
    fn sub_assign(&mut self, rhs: Self) {
        *self = *self - rhs;
    }
}

// ---------------------------------------------------------------------------
// BitRate
// ---------------------------------------------------------------------------

/// A bit rate, in bits per second. Always nonzero by construction.
///
/// # Examples
///
/// ```
/// use aether_sonde::time::BitRate;
///
/// assert_eq!(BitRate::ETHERNET_10M.as_bps(), 10_000_000);
/// assert_eq!(BitRate::ETHERNET_100M.as_bps(), 100_000_000);
/// assert_eq!(BitRate::ETHERNET_1G.as_bps(), 1_000_000_000);
/// assert_eq!(BitRate::ETHERNET_10G.as_bps(), 10_000_000_000);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BitRate(NonZeroU64);

impl BitRate {
    /// 10 Mbps half-duplex Ethernet.
    pub const ETHERNET_10M: Self = Self(nz(10_000_000));
    /// 100 Mbps Ethernet.
    pub const ETHERNET_100M: Self = Self(nz(100_000_000));
    /// 1 Gbps Ethernet.
    pub const ETHERNET_1G: Self = Self(nz(1_000_000_000));
    /// 10 Gbps Ethernet.
    pub const ETHERNET_10G: Self = Self(nz(10_000_000_000));

    /// Construct a `BitRate` from a known-nonzero bits-per-second value.
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::time::BitRate;
    /// use core::num::NonZeroU64;
    ///
    /// let rate = BitRate::new(NonZeroU64::new(2_500_000_000).unwrap());
    /// assert_eq!(rate.as_bps(), 2_500_000_000);
    /// ```
    #[must_use]
    pub const fn new(bits_per_second: NonZeroU64) -> Self {
        Self(bits_per_second)
    }

    /// Construct a `BitRate` from a raw bps value, returning `None` if zero.
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::time::BitRate;
    ///
    /// assert!(BitRate::from_bps(0).is_none());
    /// assert_eq!(BitRate::from_bps(1).map(BitRate::as_bps), Some(1));
    /// ```
    #[must_use]
    pub const fn from_bps(bits_per_second: u64) -> Option<Self> {
        match NonZeroU64::new(bits_per_second) {
            Some(rate) => Some(Self(rate)),
            None => None,
        }
    }

    /// The rate in bits per second.
    #[must_use]
    pub const fn as_bps(self) -> u64 {
        self.0.get()
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
    clippy::assertions_on_constants,
    reason = "Per [Result vs Panic]: unwrap and panic are allowed in tests."
)]
mod tests {
    use super::*;

    // -- BitTime --------------------------------------------------------------

    #[test]
    fn bittime_constructors_match_picosecond_unit() {
        // Sharp oracle (per design doc §4): exact integer values, not
        // approximate. Each constructor maps to a specific picosecond count.
        assert_eq!(BitTime::ZERO.as_u64(), 0);
        assert_eq!(BitTime::new(42).as_u64(), 42);
        assert_eq!(BitTime::from_nanos(1).as_u64(), 1_000);
        assert_eq!(BitTime::from_micros(1).as_u64(), 1_000_000);
        assert_eq!(BitTime::from_millis(1).as_u64(), 1_000_000_000);
    }

    #[test]
    fn bittime_addition_and_subtraction() {
        let a = BitTime::from_nanos(100);
        let b = BitTime::from_nanos(50);
        assert_eq!(a + b, BitTime::from_nanos(150));
        assert_eq!(a - b, BitTime::from_nanos(50));

        let mut c = a;
        c += b;
        assert_eq!(c, BitTime::from_nanos(150));
        c -= b;
        assert_eq!(c, BitTime::from_nanos(100));
    }

    #[test]
    fn bittime_ordering() {
        assert!(BitTime::from_nanos(1) < BitTime::from_micros(1));
        assert!(BitTime::from_micros(1) < BitTime::from_millis(1));
        assert_eq!(BitTime::from_nanos(1_000), BitTime::from_micros(1));
    }

    #[test]
    fn bittime_default_is_zero() {
        assert_eq!(BitTime::default(), BitTime::ZERO);
    }

    #[test]
    #[should_panic(expected = "BitTime overflow")]
    fn bittime_addition_overflow_panics() {
        let max = BitTime::new(u64::MAX);
        let _ = max + BitTime::new(1);
    }

    #[test]
    #[should_panic(expected = "BitTime underflow")]
    fn bittime_subtraction_underflow_panics() {
        let _ = BitTime::from_nanos(1) - BitTime::from_nanos(2);
    }

    #[test]
    #[should_panic(expected = "BitTime overflow in from_nanos")]
    fn bittime_from_nanos_overflow_panics() {
        let _ = BitTime::from_nanos(u64::MAX);
    }

    #[test]
    #[should_panic(expected = "BitTime overflow in from_micros")]
    fn bittime_from_micros_overflow_panics() {
        let _ = BitTime::from_micros(u64::MAX);
    }

    #[test]
    #[should_panic(expected = "BitTime overflow in from_millis")]
    fn bittime_from_millis_overflow_panics() {
        let _ = BitTime::from_millis(u64::MAX);
    }

    // -- Bits -----------------------------------------------------------------

    #[test]
    fn bits_constructors_and_accessors() {
        assert_eq!(Bits::ZERO.as_u64(), 0);
        assert_eq!(Bits::new(512).as_u64(), 512);
        assert_eq!(Bits::default(), Bits::ZERO);
    }

    #[test]
    fn bits_at_standard_ethernet_rates() {
        // Sharp oracle: closed-form bit-period values from IEEE 802.3.
        // 1 bit at rate R (bps) takes 1e12 / R picoseconds.
        assert_eq!(
            Bits::new(1).at_rate(BitRate::ETHERNET_10M),
            BitTime::new(100_000) // 100 ns
        );
        assert_eq!(
            Bits::new(1).at_rate(BitRate::ETHERNET_100M),
            BitTime::new(10_000) // 10 ns
        );
        assert_eq!(
            Bits::new(1).at_rate(BitRate::ETHERNET_1G),
            BitTime::new(1_000) // 1 ns
        );
        assert_eq!(
            Bits::new(1).at_rate(BitRate::ETHERNET_10G),
            BitTime::new(100) // 100 ps
        );
    }

    #[test]
    fn bits_at_rate_canonical_ieee_quantities() {
        // Canonical IEEE 802.3 slot times and frame lengths.
        // slotTime = 512 bit-times at 10 Mbps = 51.2 µs.
        assert_eq!(
            Bits::new(512).at_rate(BitRate::ETHERNET_10M),
            BitTime::from_nanos(51_200),
        );
        // jamSize = 32 bits at 100 Mbps = 320 ns.
        assert_eq!(
            Bits::new(32).at_rate(BitRate::ETHERNET_100M),
            BitTime::from_nanos(320),
        );
        // interFrameGap = 96 bits at 1 Gbps = 96 ns.
        assert_eq!(
            Bits::new(96).at_rate(BitRate::ETHERNET_1G),
            BitTime::from_nanos(96),
        );
        // slotTime = 4096 bit-times at 1 Gbps = 4.096 µs.
        assert_eq!(
            Bits::new(4_096).at_rate(BitRate::ETHERNET_1G),
            BitTime::from_nanos(4_096),
        );
    }

    #[test]
    fn bits_at_rate_zero_bits_is_zero_time() {
        assert_eq!(Bits::ZERO.at_rate(BitRate::ETHERNET_1G), BitTime::ZERO);
    }

    #[test]
    fn bits_addition_and_subtraction() {
        let a = Bits::new(100);
        let b = Bits::new(40);
        assert_eq!(a + b, Bits::new(140));
        assert_eq!(a - b, Bits::new(60));

        let mut c = a;
        c += b;
        assert_eq!(c, Bits::new(140));
        c -= b;
        assert_eq!(c, Bits::new(100));
    }

    #[test]
    #[should_panic(expected = "Bits overflow")]
    fn bits_addition_overflow_panics() {
        let _ = Bits::new(u64::MAX) + Bits::new(1);
    }

    #[test]
    #[should_panic(expected = "Bits underflow")]
    fn bits_subtraction_underflow_panics() {
        let _ = Bits::new(1) - Bits::new(2);
    }

    // -- BitRate --------------------------------------------------------------

    #[test]
    fn bitrate_standard_ethernet_constants() {
        // Sharp oracle: exact bps values from IEEE clause text.
        assert_eq!(BitRate::ETHERNET_10M.as_bps(), 10_000_000);
        assert_eq!(BitRate::ETHERNET_100M.as_bps(), 100_000_000);
        assert_eq!(BitRate::ETHERNET_1G.as_bps(), 1_000_000_000);
        assert_eq!(BitRate::ETHERNET_10G.as_bps(), 10_000_000_000);
    }

    #[test]
    fn bitrate_constants_are_strictly_ascending() {
        assert!(BitRate::ETHERNET_10M < BitRate::ETHERNET_100M);
        assert!(BitRate::ETHERNET_100M < BitRate::ETHERNET_1G);
        assert!(BitRate::ETHERNET_1G < BitRate::ETHERNET_10G);
    }

    #[test]
    fn bitrate_from_bps_rejects_zero() {
        assert!(BitRate::from_bps(0).is_none());
    }

    #[test]
    fn bitrate_from_bps_accepts_nonzero() {
        let rate = BitRate::from_bps(2_500_000_000).unwrap();
        assert_eq!(rate.as_bps(), 2_500_000_000);
    }

    #[test]
    fn bitrate_new_constructs_from_nonzerou64() {
        let nz = NonZeroU64::new(7_777_777).unwrap();
        let rate = BitRate::new(nz);
        assert_eq!(rate.as_bps(), 7_777_777);
    }
}
