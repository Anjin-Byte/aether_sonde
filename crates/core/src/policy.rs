//! Backoff, jam, and inter-frame-gap policies.
//!
//! Pure value types parameterizing the engine's collision-recovery and
//! transmission-spacing behavior. Per [Pure Core Effectful Edges], the
//! types in this module take any randomness as a function parameter — they
//! do not source entropy. The engine (round 8) is responsible for holding
//! the RNG and passing values through to [`BackoffPolicy::next_delay`].
//!
//! Three policy types, each addressing a separate concern:
//!
//! * [`BackoffPolicy`] — truncated binary exponential backoff (BEB) per
//!   IEEE 802.3 §"backoff" and `report_0.md` §"Binary exponential backoff".
//! * [`JamPolicy`] — duration of the collision-enforcement jam.
//! * [`IfgPolicy`] — minimum inter-frame gap between successive frames on
//!   the same channel.
//!
//! Each policy carries an `IEEE_802_3` constant pulled directly from the
//! standard's clause text, so a caller can use the canonical configuration
//! in one expression.

use crate::time::{BitRate, BitTime, Bits};

// ===========================================================================
// BackoffPolicy
// ===========================================================================

/// Truncated binary exponential backoff (BEB) policy.
///
/// Per IEEE 802.3 §"backoff" and `report_0.md` Theorem 14, after the `n`-th
/// collision the station picks a uniform random integer
/// `R ∈ {0, 1, ..., 2^m_n - 1}` where `m_n = min(n, backoff_limit)`, and
/// waits `R · slot_time` before retrying. After `attempt_limit` collisions,
/// the transmission is aborted.
///
/// The IEEE-canonical configuration is exposed as
/// [`BackoffPolicy::IEEE_802_3`] (`attempt_limit = 16`, `backoff_limit = 10`).
///
/// # Examples
///
/// ```
/// use aether_sonde::policy::BackoffPolicy;
///
/// let p = BackoffPolicy::IEEE_802_3;
/// // After 1 collision, the window is 2 slots.
/// assert_eq!(p.window_size(1), Some(2));
/// // After 16 collisions the policy aborts.
/// assert_eq!(p.window_size(16), None);
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BackoffPolicy {
    attempt_limit: u32,
    backoff_limit: u32,
}

impl BackoffPolicy {
    /// IEEE 802.3 canonical configuration: `attempt_limit = 16`,
    /// `backoff_limit = 10`.
    pub const IEEE_802_3: Self = Self {
        attempt_limit: 16,
        backoff_limit: 10,
    };

    /// Construct a `BackoffPolicy`.
    ///
    /// # Errors
    ///
    /// - [`BackoffPolicyError::ZeroAttemptLimit`] if `attempt_limit == 0`.
    /// - [`BackoffPolicyError::ZeroBackoffLimit`] if `backoff_limit == 0`.
    /// - [`BackoffPolicyError::BackoffLimitTooLarge`] if `backoff_limit >= 32`
    ///   (which would overflow the `1u32 << m_n` window computation).
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::policy::BackoffPolicy;
    /// let p = BackoffPolicy::new(8, 5).unwrap();
    /// assert_eq!(p.attempt_limit(), 8);
    /// assert_eq!(p.backoff_limit(), 5);
    /// ```
    pub const fn new(
        attempt_limit: u32,
        backoff_limit: u32,
    ) -> Result<Self, BackoffPolicyError> {
        if attempt_limit == 0 {
            return Err(BackoffPolicyError::ZeroAttemptLimit);
        }
        if backoff_limit == 0 {
            return Err(BackoffPolicyError::ZeroBackoffLimit);
        }
        if backoff_limit >= 32 {
            return Err(BackoffPolicyError::BackoffLimitTooLarge);
        }
        Ok(Self {
            attempt_limit,
            backoff_limit,
        })
    }

    /// The maximum number of attempts before the transmission is aborted.
    #[must_use]
    pub const fn attempt_limit(self) -> u32 {
        self.attempt_limit
    }

    /// The exponent cap. `m_n = min(n, backoff_limit)`.
    #[must_use]
    pub const fn backoff_limit(self) -> u32 {
        self.backoff_limit
    }

    /// The size of the backoff window (number of candidate slots) for the
    /// `n`-th retry attempt.
    ///
    /// Returns `Some(2^m_n)` where `m_n = min(n, backoff_limit)`, or `None`
    /// if `n >= attempt_limit` (the transmission must be aborted).
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::policy::BackoffPolicy;
    /// let p = BackoffPolicy::IEEE_802_3;
    /// assert_eq!(p.window_size(1), Some(2));
    /// assert_eq!(p.window_size(10), Some(1024));
    /// assert_eq!(p.window_size(15), Some(1024)); // clamped at backoff_limit
    /// assert_eq!(p.window_size(16), None);
    /// ```
    #[must_use]
    pub const fn window_size(self, n_collisions: u32) -> Option<u32> {
        if n_collisions >= self.attempt_limit {
            return None;
        }
        let m = if n_collisions < self.backoff_limit {
            n_collisions
        } else {
            self.backoff_limit
        };
        Some(1u32 << m)
    }

    /// The number of slots to wait for the `n`-th retry, given a uniform
    /// random `random` value: `random % window_size(n)`.
    ///
    /// # Errors
    ///
    /// Returns [`BackoffAborted`] if `n >= attempt_limit`.
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::policy::BackoffPolicy;
    /// let p = BackoffPolicy::IEEE_802_3;
    /// assert_eq!(p.slot_count(1, 0), Ok(0));
    /// assert_eq!(p.slot_count(1, 5), Ok(1));   // 5 % 2 = 1
    /// assert_eq!(p.slot_count(10, 2_000), Ok(2_000 % 1024));
    /// ```
    pub const fn slot_count(
        self,
        n_collisions: u32,
        random: u32,
    ) -> Result<u32, BackoffAborted> {
        match self.window_size(n_collisions) {
            Some(window) => Ok(random % window),
            None => Err(BackoffAborted {
                attempts: n_collisions,
                limit: self.attempt_limit,
            }),
        }
    }

    /// The backoff delay in [`BitTime`] for the `n`-th retry, computed as
    /// `slot_count(n, random) * slot_time`.
    ///
    /// # Errors
    ///
    /// Returns [`BackoffAborted`] if `n >= attempt_limit`.
    ///
    /// # Panics
    ///
    /// Panics if `slot_count * slot_time` overflows `u64` picoseconds.
    /// For IEEE rates and the 1024-slot maximum window, this cannot occur:
    /// 1024 × 51.2 µs ≈ 52 ms ≪ 213 days.
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::policy::BackoffPolicy;
    /// use aether_sonde::time::BitTime;
    ///
    /// let p = BackoffPolicy::IEEE_802_3;
    /// // n=1, slot_time=51.2 µs, random=1 → 1 slot → 51_200 ns.
    /// assert_eq!(
    ///     p.next_delay(1, BitTime::from_nanos(51_200), 1),
    ///     Ok(BitTime::from_nanos(51_200)),
    /// );
    /// ```
    #[track_caller]
    pub fn next_delay(
        self,
        n_collisions: u32,
        slot_time: BitTime,
        random: u32,
    ) -> Result<BitTime, BackoffAborted> {
        let slots = self.slot_count(n_collisions, random)?;
        let slots_u64 = u64::from(slots);
        let delay_ps = slot_time
            .as_u64()
            .checked_mul(slots_u64)
            .unwrap_or_else(|| {
                // RATIONALE: at IEEE rates and the 1024-slot maximum window,
                // the product is bounded by ~52 ms in picoseconds, which is
                // well below u64::MAX. Overflow indicates a non-IEEE
                // configuration whose slot_time is unrealistically large.
                #[allow(clippy::panic)]
                {
                    panic!(
                        "BackoffPolicy::next_delay overflow: slot_time={} ps × {} slots",
                        slot_time.as_u64(),
                        slots,
                    )
                }
            });
        Ok(BitTime::new(delay_ps))
    }
}

// ---------------------------------------------------------------------------
// BackoffPolicy errors
// ---------------------------------------------------------------------------

/// Errors returned by [`BackoffPolicy::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BackoffPolicyError {
    /// `attempt_limit == 0`.
    ZeroAttemptLimit,
    /// `backoff_limit == 0`.
    ZeroBackoffLimit,
    /// `backoff_limit >= 32` (would overflow `1u32 << backoff_limit`).
    BackoffLimitTooLarge,
}

impl core::fmt::Display for BackoffPolicyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ZeroAttemptLimit => f.write_str("BackoffPolicy attempt_limit must be > 0"),
            Self::ZeroBackoffLimit => f.write_str("BackoffPolicy backoff_limit must be > 0"),
            Self::BackoffLimitTooLarge => {
                f.write_str("BackoffPolicy backoff_limit must be < 32 (window-size overflow)")
            }
        }
    }
}

impl core::error::Error for BackoffPolicyError {}

/// Indicates a transmission has exceeded its [`BackoffPolicy::attempt_limit`]
/// and must be aborted.
///
/// The struct fields record the observed attempt count and the configured
/// limit so the engine (round 8) can log the abort condition meaningfully.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BackoffAborted {
    /// The number of collisions seen so far.
    pub attempts: u32,
    /// The configured `attempt_limit`.
    pub limit: u32,
}

impl core::fmt::Display for BackoffAborted {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(
            f,
            "backoff aborted: {} attempts reached limit {}",
            self.attempts, self.limit,
        )
    }
}

impl core::error::Error for BackoffAborted {}

// ===========================================================================
// JamPolicy
// ===========================================================================

/// Collision-enforcement jam policy.
///
/// A jam is a finite-length signal emitted after a collision is detected,
/// to ensure all participating transmitters observe the collision before
/// they finish sending. Per IEEE 802.3, the canonical `jamSize` is 32 bits
/// at every standard rate.
///
/// # Examples
///
/// ```
/// use aether_sonde::policy::JamPolicy;
/// use aether_sonde::time::{BitRate, BitTime};
///
/// // 32 bits at 10 Mbps = 3.2 µs.
/// assert_eq!(
///     JamPolicy::IEEE_802_3.duration_at(BitRate::ETHERNET_10M),
///     BitTime::from_nanos(3_200),
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct JamPolicy {
    bits: Bits,
}

impl JamPolicy {
    /// IEEE 802.3 canonical configuration: `jamSize = 32 bits`.
    pub const IEEE_802_3: Self = Self {
        bits: Bits::new(32),
    };

    /// Construct a `JamPolicy` with a custom jam size in bits.
    ///
    /// # Errors
    ///
    /// Returns [`JamPolicyError::ZeroBits`] if `bits == Bits::ZERO`. A
    /// zero-bit jam is meaningless — it would not enforce the collision —
    /// so the constructor rejects it at the boundary.
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::policy::JamPolicy;
    /// use aether_sonde::time::Bits;
    /// assert!(JamPolicy::new(Bits::new(48)).is_ok());
    /// assert!(JamPolicy::new(Bits::ZERO).is_err());
    /// ```
    pub const fn new(bits: Bits) -> Result<Self, JamPolicyError> {
        if bits.as_u64() == 0 {
            return Err(JamPolicyError::ZeroBits);
        }
        Ok(Self { bits })
    }

    /// The configured jam size in bits.
    #[must_use]
    pub const fn bits(self) -> Bits {
        self.bits
    }

    /// The jam duration at the given rate: `bits.at_rate(rate)`.
    ///
    /// # Panics
    ///
    /// See [`Bits::at_rate`] — overflow is not possible for any realistic
    /// jam size and rate.
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::policy::JamPolicy;
    /// use aether_sonde::time::{BitRate, BitTime};
    /// assert_eq!(
    ///     JamPolicy::IEEE_802_3.duration_at(BitRate::ETHERNET_1G),
    ///     BitTime::from_nanos(32),
    /// );
    /// ```
    #[must_use]
    pub const fn duration_at(self, rate: BitRate) -> BitTime {
        self.bits.at_rate(rate)
    }
}

/// Errors returned by [`JamPolicy::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum JamPolicyError {
    /// `bits == Bits::ZERO`. A zero-bit jam cannot enforce a collision.
    ZeroBits,
}

impl core::fmt::Display for JamPolicyError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::ZeroBits => f.write_str("JamPolicy must have at least 1 bit"),
        }
    }
}

impl core::error::Error for JamPolicyError {}

// ===========================================================================
// IfgPolicy
// ===========================================================================

/// Inter-frame gap policy: the minimum quiet time required between
/// successive frames on the same channel.
///
/// IEEE 802.3 specifies `interFrameGap = 96 bits` at every rate. A zero-bit
/// IFG is allowed as the "no enforced minimum gap" configuration, which
/// has a coherent (if unusual) interpretation in specialized topologies.
///
/// # Examples
///
/// ```
/// use aether_sonde::policy::IfgPolicy;
/// use aether_sonde::time::{BitRate, BitTime};
///
/// // 96 bits at 1 Gbps = 96 ns.
/// assert_eq!(
///     IfgPolicy::IEEE_802_3.duration_at(BitRate::ETHERNET_1G),
///     BitTime::from_nanos(96),
/// );
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct IfgPolicy {
    bits: Bits,
}

impl IfgPolicy {
    /// IEEE 802.3 canonical configuration: `interFrameGap = 96 bits`.
    pub const IEEE_802_3: Self = Self {
        bits: Bits::new(96),
    };

    /// Construct an `IfgPolicy` with a custom IFG in bits.
    ///
    /// `Bits::ZERO` is accepted and means "no minimum gap"; the resulting
    /// [`IfgPolicy::duration_at`] returns `BitTime::ZERO` at any rate.
    ///
    /// # Examples
    ///
    /// ```
    /// use aether_sonde::policy::IfgPolicy;
    /// use aether_sonde::time::{BitRate, BitTime, Bits};
    ///
    /// let strict = IfgPolicy::new(Bits::new(128));
    /// assert_eq!(strict.bits(), Bits::new(128));
    ///
    /// let none = IfgPolicy::new(Bits::ZERO);
    /// assert_eq!(none.duration_at(BitRate::ETHERNET_1G), BitTime::ZERO);
    /// ```
    #[must_use]
    pub const fn new(bits: Bits) -> Self {
        Self { bits }
    }

    /// The configured IFG in bits.
    #[must_use]
    pub const fn bits(self) -> Bits {
        self.bits
    }

    /// The IFG duration at the given rate: `bits.at_rate(rate)`.
    ///
    /// # Panics
    ///
    /// See [`Bits::at_rate`] — overflow is not possible for any realistic
    /// IFG and rate.
    #[must_use]
    pub const fn duration_at(self, rate: BitRate) -> BitTime {
        self.bits.at_rate(rate)
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

    // -- BackoffPolicy: window_size --------------------------------------------

    #[test]
    fn backoff_window_size_for_low_collisions() {
        // Sharp oracle: report_0 §"Binary exponential backoff" / IEEE 802.3 §"backoff".
        // window_size(n) = 2^min(n, backoff_limit)
        let p = BackoffPolicy::IEEE_802_3;
        assert_eq!(p.window_size(1), Some(2));
        assert_eq!(p.window_size(2), Some(4));
        assert_eq!(p.window_size(3), Some(8));
        assert_eq!(p.window_size(4), Some(16));
        assert_eq!(p.window_size(5), Some(32));
        assert_eq!(p.window_size(10), Some(1024));
    }

    #[test]
    fn backoff_window_size_clamps_at_backoff_limit() {
        // n > backoff_limit: m clamps; window stays at 2^backoff_limit.
        let p = BackoffPolicy::IEEE_802_3;
        assert_eq!(p.window_size(11), Some(1024));
        assert_eq!(p.window_size(15), Some(1024));
    }

    #[test]
    fn backoff_window_size_returns_none_at_or_beyond_attempt_limit() {
        let p = BackoffPolicy::IEEE_802_3;
        assert_eq!(p.window_size(16), None);
        assert_eq!(p.window_size(20), None);
        assert_eq!(p.window_size(u32::MAX), None);
    }

    // -- BackoffPolicy: slot_count --------------------------------------------

    #[test]
    fn backoff_slot_count_modulo_arithmetic() {
        let p = BackoffPolicy::IEEE_802_3;
        // n=1, window=2
        assert_eq!(p.slot_count(1, 0), Ok(0));
        assert_eq!(p.slot_count(1, 1), Ok(1));
        assert_eq!(p.slot_count(1, 5), Ok(1)); // 5 % 2
        // n=10, window=1024
        assert_eq!(p.slot_count(10, 2_000), Ok(2_000 % 1024));
    }

    #[test]
    fn backoff_slot_count_aborts_at_attempt_limit() {
        let p = BackoffPolicy::IEEE_802_3;
        let aborted = p.slot_count(16, 0).unwrap_err();
        assert_eq!(aborted.attempts, 16);
        assert_eq!(aborted.limit, 16);
    }

    // -- BackoffPolicy: next_delay --------------------------------------------

    #[test]
    fn backoff_next_delay_is_slot_count_times_slot_time() {
        let p = BackoffPolicy::IEEE_802_3;
        // 10 Mbps half duplex: slot_time = 51.2 µs.
        let slot = BitTime::from_nanos(51_200);
        // n=1, random=1 → 1 slot → 51.2 µs.
        assert_eq!(p.next_delay(1, slot, 1), Ok(slot));
        // n=1, random=0 → 0 slots → 0 ps.
        assert_eq!(p.next_delay(1, slot, 0), Ok(BitTime::ZERO));
    }

    #[test]
    fn backoff_next_delay_aborts_at_attempt_limit() {
        let p = BackoffPolicy::IEEE_802_3;
        assert!(p.next_delay(16, BitTime::from_nanos(51_200), 0).is_err());
    }

    // -- BackoffPolicy: pairwise re-collision probability ----------------------

    /// Per `report_0.md` Theorem 14, the probability that two stations
    /// independently choosing `R ∈ {0, .., 2^m_n - 1}` pick the same value
    /// is `2^(-m_n)`. We assert this **deterministically** by counting the
    /// `R1 == R2` cases in the enumerated `R1 × R2` grid (the diagonal),
    /// which equals `window_size`. The total grid is `window_size^2`. So
    /// `P = window_size / (window_size * window_size) = 1 / window_size = 2^(-m_n)`.
    #[test]
    fn backoff_pairwise_recollision_probability_matches_theorem_14() {
        let p = BackoffPolicy::IEEE_802_3;

        // Exhaustively check small windows: n=1 (window=2), n=2 (window=4),
        // n=3 (window=8). For each, count the collisions on the diagonal.
        for n in 1u32..=5 {
            let window = p.window_size(n).unwrap();
            let mut diagonal = 0u32;
            let mut total = 0u32;
            for r1 in 0..window {
                for r2 in 0..window {
                    if r1 == r2 {
                        diagonal += 1;
                    }
                    total += 1;
                }
            }
            assert_eq!(diagonal, window, "diagonal count for n={n}");
            assert_eq!(total, window * window, "grid size for n={n}");
            // P(R1=R2) = diagonal / total = window / window^2 = 1 / window
            // = 2^(-m_n). Sharp oracle: exact integer ratio.
            assert_eq!(
                u64::from(diagonal) * u64::from(window),
                u64::from(total),
                "ratio identity for n={n}",
            );
        }
    }

    // -- BackoffPolicy: construction validation -------------------------------

    #[test]
    fn backoff_new_rejects_zero_attempt_limit() {
        assert_eq!(
            BackoffPolicy::new(0, 10),
            Err(BackoffPolicyError::ZeroAttemptLimit),
        );
    }

    #[test]
    fn backoff_new_rejects_zero_backoff_limit() {
        assert_eq!(
            BackoffPolicy::new(16, 0),
            Err(BackoffPolicyError::ZeroBackoffLimit),
        );
    }

    #[test]
    fn backoff_new_rejects_too_large_backoff_limit() {
        assert_eq!(
            BackoffPolicy::new(16, 32),
            Err(BackoffPolicyError::BackoffLimitTooLarge),
        );
    }

    #[test]
    fn backoff_new_accepts_valid_configuration() {
        let p = BackoffPolicy::new(8, 5).unwrap();
        assert_eq!(p.attempt_limit(), 8);
        assert_eq!(p.backoff_limit(), 5);
        // m clamps at 5, so window_size(7) = 2^5 = 32 (since 7 < 8 = limit).
        assert_eq!(p.window_size(7), Some(32));
    }

    #[test]
    fn ieee_802_3_constants_are_correct() {
        let p = BackoffPolicy::IEEE_802_3;
        assert_eq!(p.attempt_limit(), 16);
        assert_eq!(p.backoff_limit(), 10);
    }

    // -- JamPolicy ------------------------------------------------------------

    #[test]
    fn jam_ieee_802_3_durations_match_ieee_at_each_rate() {
        // Sharp oracle: jamSize = 32 bits at every rate.
        let j = JamPolicy::IEEE_802_3;
        assert_eq!(j.bits(), Bits::new(32));
        assert_eq!(
            j.duration_at(BitRate::ETHERNET_10M),
            BitTime::from_nanos(3_200),
        );
        assert_eq!(
            j.duration_at(BitRate::ETHERNET_100M),
            BitTime::from_nanos(320),
        );
        assert_eq!(j.duration_at(BitRate::ETHERNET_1G), BitTime::from_nanos(32));
    }

    #[test]
    fn jam_new_accepts_positive_bits() {
        let j = JamPolicy::new(Bits::new(48)).unwrap();
        assert_eq!(j.bits(), Bits::new(48));
    }

    #[test]
    fn jam_new_rejects_zero_bits() {
        assert_eq!(JamPolicy::new(Bits::ZERO), Err(JamPolicyError::ZeroBits));
    }

    // -- IfgPolicy ------------------------------------------------------------

    #[test]
    fn ifg_ieee_802_3_durations_match_ieee_at_each_rate() {
        // Sharp oracle: interFrameGap = 96 bits at every rate.
        let g = IfgPolicy::IEEE_802_3;
        assert_eq!(g.bits(), Bits::new(96));
        assert_eq!(
            g.duration_at(BitRate::ETHERNET_10M),
            BitTime::from_nanos(9_600),
        );
        assert_eq!(
            g.duration_at(BitRate::ETHERNET_100M),
            BitTime::from_nanos(960),
        );
        assert_eq!(g.duration_at(BitRate::ETHERNET_1G), BitTime::from_nanos(96));
    }

    #[test]
    fn ifg_new_accepts_zero_as_no_minimum_gap() {
        let g = IfgPolicy::new(Bits::ZERO);
        assert_eq!(g.bits(), Bits::ZERO);
        assert_eq!(g.duration_at(BitRate::ETHERNET_1G), BitTime::ZERO);
    }

    #[test]
    fn ifg_new_accepts_arbitrary_bit_count() {
        let g = IfgPolicy::new(Bits::new(128));
        assert_eq!(g.bits(), Bits::new(128));
    }

    // -- Errors implement standard traits -------------------------------------

    #[test]
    fn errors_implement_error_trait() {
        let _: &dyn core::error::Error = &BackoffPolicyError::ZeroAttemptLimit;
        let _: &dyn core::error::Error = &BackoffAborted {
            attempts: 16,
            limit: 16,
        };
        let _: &dyn core::error::Error = &JamPolicyError::ZeroBits;
    }

    #[test]
    fn errors_display_useful_messages() {
        assert!(format!("{}", BackoffPolicyError::ZeroAttemptLimit).contains("attempt_limit"));
        assert!(format!("{}", BackoffPolicyError::ZeroBackoffLimit).contains("backoff_limit"));
        assert!(format!("{}", BackoffPolicyError::BackoffLimitTooLarge).contains("32"));
        assert!(
            format!(
                "{}",
                BackoffAborted {
                    attempts: 16,
                    limit: 16
                }
            )
            .contains("16")
        );
        assert!(format!("{}", JamPolicyError::ZeroBits).contains("1 bit"));
    }
}
