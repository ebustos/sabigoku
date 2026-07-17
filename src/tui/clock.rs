//! Tick clock, debounce, and spinner-age primitives (04 §8, DESIGN 4.8).
//! Named consumers (search 300ms, cover settle 150ms, sync flush 3000ms) arrive
//! with their subsystems. Everything takes `now` as a parameter so tests never
//! sleep.

use std::time::{Duration, Instant};

pub const TICK: Duration = Duration::from_millis(100);
/// Spinner escalates to the slow color after this async_start age (DESIGN 4.8).
pub const SLOW_AFTER: Duration = Duration::from_millis(3000);

/// Deadline source for the loop's `recv_timeout`. After a stall it schedules
/// from `now`, deliberately skipping missed ticks: one late tick, never a burst.
#[derive(Debug)]
pub struct TickClock {
    next: Instant,
}

impl TickClock {
    pub fn new(now: Instant) -> Self {
        TickClock { next: now + TICK }
    }

    /// How long the loop may block waiting for events.
    pub fn timeout(&self, now: Instant) -> Duration {
        self.next.saturating_duration_since(now)
    }

    /// True once per elapsed deadline; advances the schedule.
    pub fn should_tick(&mut self, now: Instant) -> bool {
        if now < self.next {
            return false;
        }
        self.next = now + TICK;
        true
    }
}

/// One-shot deadline: arm on the triggering edit, fire at most once when the
/// deadline passes (04 §8: `now >= deadline`). Re-arming pushes the deadline.
#[derive(Debug, Default)]
pub struct Debounce {
    deadline: Option<Instant>,
}

impl Debounce {
    pub fn arm(&mut self, now: Instant, period: Duration) {
        self.deadline = Some(now + period);
    }

    pub fn disarm(&mut self) {
        self.deadline = None;
    }

    pub fn is_armed(&self) -> bool {
        self.deadline.is_some()
    }

    pub fn fire(&mut self, now: Instant) -> bool {
        match self.deadline {
            Some(deadline) if now >= deadline => {
                self.deadline = None;
                true
            }
            _ => false,
        }
    }
}

/// Age of an in-flight async operation, for spinner frame + slow escalation.
#[derive(Debug, Clone, Copy)]
pub struct AsyncStart {
    started: Instant,
}

impl AsyncStart {
    pub fn new(now: Instant) -> Self {
        AsyncStart { started: now }
    }

    /// Frame index into a spinner of `frames` glyphs, advancing once per tick.
    pub fn frame(&self, now: Instant, frames: usize) -> usize {
        if frames == 0 {
            return 0;
        }
        let elapsed = now.saturating_duration_since(self.started);
        (elapsed.as_millis() / TICK.as_millis()) as usize % frames
    }

    pub fn is_slow(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) >= SLOW_AFTER
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn tick_fires_on_deadline_and_reschedules() {
        let start = t0();
        let mut clock = TickClock::new(start);
        assert!(!clock.should_tick(start));
        assert!(clock.should_tick(start + TICK));
        assert!(!clock.should_tick(start + TICK));
        assert!(clock.should_tick(start + TICK * 2));
    }

    #[test]
    fn tick_skips_missed_deadlines_after_stall() {
        let start = t0();
        let mut clock = TickClock::new(start);
        let after_stall = start + TICK * 7;
        assert!(clock.should_tick(after_stall));
        assert!(!clock.should_tick(after_stall + TICK / 2));
        assert!(clock.should_tick(after_stall + TICK));
    }

    #[test]
    fn timeout_never_underflows() {
        let start = t0();
        let clock = TickClock::new(start);
        assert_eq!(clock.timeout(start + TICK * 3), Duration::ZERO);
        assert!(clock.timeout(start) <= TICK);
    }

    #[test]
    fn debounce_fires_once_at_deadline() {
        let start = t0();
        let period = Duration::from_millis(300);
        let mut debounce = Debounce::default();
        assert!(!debounce.fire(start));
        debounce.arm(start, period);
        assert!(!debounce.fire(start + period - Duration::from_millis(1)));
        assert!(debounce.fire(start + period));
        assert!(!debounce.fire(start + period));
        assert!(!debounce.is_armed());
    }

    #[test]
    fn rearm_pushes_the_deadline() {
        let start = t0();
        let period = Duration::from_millis(300);
        let mut debounce = Debounce::default();
        debounce.arm(start, period);
        debounce.arm(start + Duration::from_millis(200), period);
        assert!(!debounce.fire(start + period));
        assert!(debounce.fire(start + Duration::from_millis(500)));
    }

    #[test]
    fn disarm_prevents_fire() {
        let start = t0();
        let mut debounce = Debounce::default();
        debounce.arm(start, Duration::from_millis(300));
        debounce.disarm();
        assert!(!debounce.fire(start + Duration::from_secs(1)));
    }

    #[test]
    fn spinner_frames_advance_per_tick_and_wrap() {
        let start = t0();
        let spin = AsyncStart::new(start);
        assert_eq!(spin.frame(start, 4), 0);
        assert_eq!(spin.frame(start + TICK, 4), 1);
        assert_eq!(spin.frame(start + TICK * 5, 4), 1);
        assert_eq!(spin.frame(start, 0), 0);
    }

    #[test]
    fn spinner_goes_slow_at_3000ms() {
        let start = t0();
        let spin = AsyncStart::new(start);
        assert!(!spin.is_slow(start + SLOW_AFTER - Duration::from_millis(1)));
        assert!(spin.is_slow(start + SLOW_AFTER));
    }
}
