//! "Is it time again?" checks that start overdue, without going back in time.
//!
//! A loop that does something every few seconds used to start its stamp at
//! `Instant::now() - period`, so the first check fires at once. That
//! subtraction panics when the machine has been up for less than `period`,
//! because the monotonic clock behind [`Instant`] starts at boot on Linux,
//! Windows and macOS (a fast boot plus autostart). A stamp is an
//! `Option<Instant>` instead: `None` means "never", which is always overdue.

use std::time::{Duration, Instant};

/// The time from `last` to `now`; [`Duration::MAX`] when `last` is `None`
/// (it never happened, so any period has passed).
pub fn since(last: Option<Instant>, now: Instant) -> Duration {
    last.map_or(Duration::MAX, |t| now.saturating_duration_since(t))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn never_is_always_overdue() {
        assert_eq!(since(None, Instant::now()), Duration::MAX);
    }

    #[test]
    fn measures_from_the_stamp() {
        let t = Instant::now();
        assert_eq!(since(Some(t), t + Duration::from_secs(3)), Duration::from_secs(3));
    }

    #[test]
    fn a_stamp_after_now_is_zero_not_a_panic() {
        let t = Instant::now();
        assert_eq!(since(Some(t + Duration::from_secs(1)), t), Duration::ZERO);
    }
}
