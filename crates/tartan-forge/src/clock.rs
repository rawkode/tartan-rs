//! The forge's clock (design D12, *Time in tests*).
//!
//! Every identity and signing path that compares times reads [`Services::clock`] through
//! [`now_ms`], never `SystemTime::now()`. The `tartan` binary installs [`SystemClock`]; the test
//! harness installs its own clock, wall-clock time plus an offset it can advance, so tests of
//! hour-, day- and week-long windows run against the real durations. Rivet schedules fire on
//! real time, so a schedule meant for clock time `t` is registered at [`real_time_ms`]`(t)`.
//!
//! [`Services::clock`]: crate::Services::clock

use std::fmt::Debug;

/// A source of the current time.
pub trait Clock: Send + Sync + Debug {
    /// Milliseconds since the Unix epoch, as the forge sees it.
    fn now_ms(&self) -> i64;

    /// How far this clock runs ahead of real time, in milliseconds.
    fn offset_ms(&self) -> i64 {
        0
    }
}

/// Real time. The only clock the `tartan` binary installs; its offset is always zero.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        system_now_ms()
    }
}

/// Real time in milliseconds since the Unix epoch.
pub(crate) fn system_now_ms() -> i64 {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
}

/// The installed clock's time, or real time before services are installed (unit tests of
/// modules that never reach an actor).
pub(crate) fn now_ms() -> i64 {
    match crate::services::services() {
        Ok(services) => services.clock.now_ms(),
        Err(_) => SystemClock.now_ms(),
    }
}

/// The real time at which a schedule for clock time `clock_ms` must fire: `clock_ms` minus the
/// clock's offset, and never earlier than real now, so a schedule whose clock time has already
/// passed fires at once.
pub fn real_time_ms(clock: &dyn Clock, clock_ms: i64) -> i64 {
    clock_ms
        .saturating_sub(clock.offset_ms())
        .max(system_now_ms())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real time plus a fixed offset, as the test harness's clock is after an advance.
    #[derive(Debug)]
    struct Ahead(i64);

    impl Clock for Ahead {
        fn now_ms(&self) -> i64 {
            system_now_ms() + self.0
        }

        fn offset_ms(&self) -> i64 {
            self.0
        }
    }

    const HOUR_MS: i64 = 3_600_000;

    #[test]
    fn system_clock_schedules_on_its_own_time() {
        let later = SystemClock.now_ms() + HOUR_MS;
        assert_eq!(real_time_ms(&SystemClock, later), later);
    }

    #[test]
    fn a_clock_time_already_passed_after_an_advance_fires_at_once() {
        // A schedule for "now + 1 hour" registered before the clock was advanced 7 hours: its
        // clock time has passed, so it is due now in real time.
        let clock = Ahead(7 * HOUR_MS);
        let due = SystemClock.now_ms() + HOUR_MS;
        let before = system_now_ms();
        let at = real_time_ms(&clock, due);
        assert!(at >= before && at <= system_now_ms(), "{at} is not now");
    }

    #[test]
    fn a_future_clock_time_keeps_its_distance_in_real_time() {
        let clock = Ahead(7 * HOUR_MS);
        let due = clock.now_ms() + HOUR_MS;
        let at = real_time_ms(&clock, due);
        let expected = system_now_ms() + HOUR_MS;
        assert!((at - expected).abs() < 1000, "{at} is not an hour from now");
    }
}
