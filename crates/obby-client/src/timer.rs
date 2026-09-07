//! Deadline scheduling, with no clock of its own.
//!
//! Every instant here is a monotonic millisecond count the host measured itself. Nothing in this
//! file reads a clock, so a test drives the whole thing by feeding it numbers.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

/// A moment in time, as the host's two clocks see it.
///
/// Monotonic time drives every deadline, because it only ever moves forward; wall clock can jump
/// when a user resets their system clock or NTP steps it. Wall clock exists only to stamp a message
/// the server did not stamp itself with `server-time`, so this module never reads `unix_ms` at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Now {
    /// Milliseconds on a clock that never goes backward.
    pub monotonic_ms: u64,
    /// Milliseconds since the Unix epoch.
    pub unix_ms: u64,
}

/// The reference client's `PING` keepalive interval.
pub(crate) const PING_KEEPALIVE_MS: u64 = 30_000;

/// The reference client's `PONG` timeout: no reply this long after a `PING` means the link is dead.
pub(crate) const DEAD_LINK_MS: u64 = 10_000;

/// How long a typing indicator stands before it goes stale.
///
/// The `done` that would clear it can be lost, and a client that waits for one shows someone typing
/// forever, so the indicator expires on its own.
pub(crate) const TYPING_EXPIRY_MS: u64 = 6_000;

/// One named deadline a host can arm and later collect once it falls due.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub(crate) enum Deadline {
    /// Time to send the next `PING` to prove the link is still alive.
    PingKeepalive,
    /// No `PONG` answered in time: the link is dead and should be torn down.
    DeadLink,
    /// Time to attempt the next reconnect, per [`ReconnectBackoff`].
    Reconnect,
    /// A target's typing indicator expires, keyed by whatever the host uses to identify it (for
    /// instance a channel and nick pair).
    /// Someone composing a message, keyed by where and who.
    ///
    /// Two fields rather than one joined string: any separator we picked could appear in a nick or
    /// a channel, and two different pairs would then share one deadline.
    Typing(String, String),
}

/// A set of named deadlines, armed and drained by monotonic time alone.
///
/// A host calls [`Timers::set`] with an absolute instant computed from its own [`Now`], and later
/// calls [`Timers::expire`] with a fresh `Now` to collect whatever fell due since the last drain.
#[derive(Debug, Clone, Default)]
pub(crate) struct Timers {
    deadlines: BTreeMap<Deadline, u64>,
}

impl Timers {
    /// A scheduler with nothing armed.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Arm a deadline for this monotonic instant, replacing any previous one under the same key.
    pub(crate) fn set(&mut self, deadline: Deadline, at_monotonic_ms: u64) {
        self.deadlines.insert(deadline, at_monotonic_ms);
    }

    /// Disarm a deadline, if one was set.
    pub(crate) fn clear(&mut self, deadline: &Deadline) {
        self.deadlines.remove(deadline);
    }

    /// Every deadline that has fallen due by `now`, earliest first, removed from the schedule so a
    /// second call at the same instant will not return them again.
    pub(crate) fn expire(&mut self, now: Now) -> Vec<Deadline> {
        drain_due(&mut self.deadlines, now.monotonic_ms, |at| *at)
            .into_iter()
            .map(|(deadline, _)| deadline)
            .collect()
    }

    /// The earliest pending deadline, so a host can sleep exactly that long instead of polling.
    pub(crate) fn next(&self) -> Option<u64> {
        self.deadlines.values().min().copied()
    }
}

/// Exponential reconnect backoff: `base * 2^attempt`, capped, giving up after a bounded number of
/// attempts.
///
/// The core never adds randomness: the same sequence of calls always produces the same delays, which
/// is what makes it testable by feeding in attempt counts. A host that wants to avoid a reconnect
/// storm across many clients adds jitter on top of the delay [`ReconnectBackoff::next_delay_ms`]
/// returns, before arming [`Deadline::Reconnect`] with the result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReconnectBackoff {
    base_ms: u64,
    cap_ms: u64,
    max_attempts: u32,
    attempts: u32,
}

impl ReconnectBackoff {
    /// The reference client's starting delay, before any doubling.
    pub(crate) const DEFAULT_BASE_MS: u64 = 2_000;
    /// The reference client's ceiling: no delay grows past this.
    pub(crate) const DEFAULT_CAP_MS: u64 = 300_000;
    /// The reference client's give-up point.
    pub(crate) const DEFAULT_MAX_ATTEMPTS: u32 = 100;

    /// A backoff with its own base delay, cap and attempt limit.
    pub(crate) fn new(base_ms: u64, cap_ms: u64, max_attempts: u32) -> Self {
        Self {
            base_ms,
            cap_ms,
            max_attempts,
            attempts: 0,
        }
    }

    /// The delay before the next attempt, or `None` once `max_attempts` is exhausted.
    ///
    /// Each call advances the attempt counter, so the first call returns `base_ms`, the second
    /// `base_ms * 2`, and so on up to the cap, then `None` forever until [`Self::reset`].
    pub(crate) fn next_delay_ms(&mut self) -> Option<u64> {
        if self.attempts >= self.max_attempts {
            return None;
        }
        // `checked_shl` rather than `<<` because a bounded attempt count can still exceed 63 with a
        // generous `max_attempts`, and shifting that far is what would otherwise panic
        let factor = 1u64.checked_shl(self.attempts).unwrap_or(u64::MAX);
        let delay = self.base_ms.saturating_mul(factor).min(self.cap_ms);
        self.attempts += 1;
        Some(delay)
    }

    /// Start the sequence over, for after a connection succeeds.
    pub(crate) fn reset(&mut self) {
        self.attempts = 0;
    }
}

impl Default for ReconnectBackoff {
    /// The reference client's own values: a 2 second base doubling to a 5 minute cap, giving up
    /// after 100 attempts.
    fn default() -> Self {
        Self::new(
            Self::DEFAULT_BASE_MS,
            Self::DEFAULT_CAP_MS,
            Self::DEFAULT_MAX_ATTEMPTS,
        )
    }
}

/// Remove every entry whose deadline has passed, earliest first.
///
/// Deadlines are the value in the map rather than the key, so the map's own ordering is by whatever
/// names the entry and says nothing about when it falls due. Both the timer wheel and the labelled
/// commands need the same drain, and having it once is what keeps "due" meaning one thing.
pub(crate) fn drain_due<K, V>(
    map: &mut alloc::collections::BTreeMap<K, V>,
    now_ms: u64,
    deadline_of: impl Fn(&V) -> u64,
) -> Vec<(K, V)>
where
    K: Ord + Clone,
{
    let mut due: Vec<(u64, K)> = map
        .iter()
        .filter(|(_, value)| deadline_of(value) <= now_ms)
        .map(|(key, value)| (deadline_of(value), key.clone()))
        .collect();
    due.sort_by_key(|(at, _)| *at);
    due.into_iter()
        .filter_map(|(_, key)| map.remove(&key).map(|value| (key, value)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec;

    fn now(monotonic_ms: u64) -> Now {
        Now {
            monotonic_ms,
            unix_ms: 0,
        }
    }

    #[test]
    fn a_deadline_fires_exactly_at_its_instant_and_not_before() {
        let mut timers = Timers::new();
        timers.set(Deadline::PingKeepalive, 1_000);
        assert!(timers.expire(now(999)).is_empty());
        assert_eq!(timers.expire(now(1_000)), vec![Deadline::PingKeepalive]);
    }

    #[test]
    fn several_deadlines_expire_in_one_call_in_order() {
        let mut timers = Timers::new();
        timers.set(Deadline::DeadLink, 300);
        timers.set(Deadline::PingKeepalive, 100);
        timers.set(Deadline::Reconnect, 200);
        let due = timers.expire(now(1_000));
        assert_eq!(
            due,
            vec![
                Deadline::PingKeepalive,
                Deadline::Reconnect,
                Deadline::DeadLink
            ]
        );
    }

    #[test]
    fn expiring_drains_a_deadline_so_it_does_not_fire_twice() {
        let mut timers = Timers::new();
        timers.set(Deadline::DeadLink, 100);
        assert_eq!(timers.expire(now(200)), vec![Deadline::DeadLink]);
        assert!(timers.expire(now(200)).is_empty());
    }

    #[test]
    fn next_returns_the_earliest_pending_deadline() {
        let mut timers = Timers::new();
        assert_eq!(timers.next(), None);
        timers.set(Deadline::DeadLink, 500);
        timers.set(Deadline::PingKeepalive, 200);
        assert_eq!(timers.next(), Some(200));
    }

    #[test]
    fn clearing_a_deadline_disarms_it() {
        let mut timers = Timers::new();
        timers.set(Deadline::PingKeepalive, 100);
        timers.clear(&Deadline::PingKeepalive);
        assert!(timers.expire(now(1_000)).is_empty());
        assert_eq!(timers.next(), None);
    }

    #[test]
    fn per_key_typing_deadlines_are_independent() {
        let mut timers = Timers::new();
        timers.set(Deadline::Typing("#a".to_string(), "alice".to_string()), 100);
        timers.set(Deadline::Typing("#a".to_string(), "bob".to_string()), 200);
        assert_eq!(
            timers.expire(now(100)),
            vec![Deadline::Typing("#a".to_string(), "alice".to_string())]
        );
        assert_eq!(timers.next(), Some(200));
    }

    #[test]
    fn reconnect_backoff_doubles_then_caps_then_gives_up() {
        let mut backoff = ReconnectBackoff::new(1_000, 5_000, 4);
        assert_eq!(backoff.next_delay_ms(), Some(1_000));
        assert_eq!(backoff.next_delay_ms(), Some(2_000));
        assert_eq!(backoff.next_delay_ms(), Some(4_000));
        assert_eq!(
            backoff.next_delay_ms(),
            Some(5_000),
            "8000 would exceed the 5000 cap"
        );
        assert_eq!(
            backoff.next_delay_ms(),
            None,
            "the fifth attempt exceeds max_attempts"
        );
    }

    #[test]
    fn reconnect_backoff_resets_back_to_the_base_delay() {
        let mut backoff = ReconnectBackoff::new(1_000, 5_000, 4);
        assert_eq!(backoff.next_delay_ms(), Some(1_000));
        assert_eq!(backoff.next_delay_ms(), Some(2_000));
        backoff.reset();
        assert_eq!(
            backoff.next_delay_ms(),
            Some(1_000),
            "a connection that succeeded means the next failure starts over"
        );
    }

    #[test]
    fn default_reconnect_backoff_matches_the_reference_client() {
        let mut backoff = ReconnectBackoff::default();
        assert_eq!(backoff.next_delay_ms(), Some(2_000));
        assert_eq!(backoff.next_delay_ms(), Some(4_000));
    }
}
