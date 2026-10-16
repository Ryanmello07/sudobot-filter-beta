//! Daily call caps, reserved share, and a per-provider circuit breaker.

use chrono::{DateTime, Duration, NaiveDate, Utc};
use std::sync::Mutex;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provider {
    Paxmod,
    Jev,
    Ocr,
}

impl Provider {
    pub fn name(self) -> &'static str {
        match self {
            Provider::Paxmod => "paxmod",
            Provider::Jev => "jev",
            Provider::Ocr => "ocr",
        }
    }
}

/// How important a call is for budget purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    /// Quarantine and ungraduated Fresh.
    High,
    /// Graduated Fresh, Established, context-feed-only.
    Low,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denied {
    OverCap,
    CircuitOpen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerEvent {
    Opened,
    Closed,
}

#[derive(Debug)]
enum BState {
    Closed { failures: u32 },
    Open { until: DateTime<Utc> },
    HalfOpen { probing: bool },
}

struct Inner {
    day: NaiveDate,
    calls: u64,
    failures_today: u64,
    exhausted: bool,
    breaker: BState,
}

pub struct Budget {
    cap: u64,
    reserved_share: f64,
    fail_threshold: u32,
    open_for: Duration,
    inner: Mutex<Inner>,
}

impl Budget {
    pub fn new(cap: u64, reserved_share: f64) -> Self {
        Self::with_breaker(cap, reserved_share, 5, Duration::seconds(60))
    }

    pub fn with_breaker(cap: u64, reserved_share: f64, fail_threshold: u32, open_for: Duration) -> Self {
        Budget {
            cap,
            reserved_share,
            fail_threshold,
            open_for,
            inner: Mutex::new(Inner {
                day: Utc::now().date_naive(),
                calls: 0,
                failures_today: 0,
                exhausted: false,
                breaker: BState::Closed { failures: 0 },
            }),
        }
    }

    fn roll_day(i: &mut Inner, now: DateTime<Utc>) {
        if now.date_naive() != i.day {
            i.day = now.date_naive();
            i.calls = 0;
            i.failures_today = 0;
            i.exhausted = false;
        }
    }

    /// Reserve a call slot. Counts the call on success.
    pub fn try_acquire(&self, prio: Priority, now: DateTime<Utc>) -> Result<(), Denied> {
        let mut i = self.inner.lock().unwrap();
        Self::roll_day(&mut i, now);

        if let BState::Open { until } = &i.breaker {
            if now >= *until {
                i.breaker = BState::HalfOpen { probing: false };
            } else {
                return Err(Denied::CircuitOpen);
            }
        }
        if let BState::HalfOpen { probing } = &mut i.breaker {
            if *probing {
                return Err(Denied::CircuitOpen);
            }
        }

        let limit = match prio {
            Priority::High => self.cap,
            Priority::Low => ((self.cap as f64) * (1.0 - self.reserved_share)).floor() as u64,
        };
        if i.exhausted || i.calls >= limit {
            return Err(Denied::OverCap);
        }
        i.calls += 1;
        if let BState::HalfOpen { probing } = &mut i.breaker {
            *probing = true;
        }
        Ok(())
    }

    pub fn record_success(&self) -> Option<BreakerEvent> {
        let mut i = self.inner.lock().unwrap();
        let was_open = !matches!(i.breaker, BState::Closed { .. });
        i.breaker = BState::Closed { failures: 0 };
        was_open.then_some(BreakerEvent::Closed)
    }

    pub fn record_failure(&self, now: DateTime<Utc>) -> Option<BreakerEvent> {
        let mut i = self.inner.lock().unwrap();
        i.failures_today += 1;
        match &mut i.breaker {
            BState::Closed { failures } => {
                *failures += 1;
                if *failures >= self.fail_threshold {
                    i.breaker = BState::Open { until: now + self.open_for };
                    return Some(BreakerEvent::Opened);
                }
                None
            }
            BState::HalfOpen { .. } => {
                i.breaker = BState::Open { until: now + self.open_for };
                None // already announced as open; reopening is silent
            }
            BState::Open { .. } => None,
        }
    }

    /// Provider said the plan quota is used up (HTTP 429): stop for the day.
    pub fn mark_exhausted(&self) {
        self.inner.lock().unwrap().exhausted = true;
    }

    /// A non-failure outcome that should free a half-open probe slot
    /// (for example a 4xx that says nothing about provider health).
    pub fn release_probe(&self) {
        let mut i = self.inner.lock().unwrap();
        if let BState::HalfOpen { probing } = &mut i.breaker {
            *probing = false;
        }
    }

    /// Seed today's call count after a restart.
    pub fn preload(&self, calls: u64) {
        self.inner.lock().unwrap().calls = calls;
    }

    pub fn usage(&self) -> (u64, u64, u64) {
        let i = self.inner.lock().unwrap();
        (i.calls, i.failures_today, self.cap)
    }

    pub fn breaker_open(&self) -> bool {
        !matches!(self.inner.lock().unwrap().breaker, BState::Closed { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    #[test]
    fn caps_and_reserve() {
        let b = Budget::new(10, 0.5);
        let t = now();
        for _ in 0..5 {
            assert!(b.try_acquire(Priority::Low, t).is_ok());
        }
        assert_eq!(b.try_acquire(Priority::Low, t), Err(Denied::OverCap));
        for _ in 0..5 {
            assert!(b.try_acquire(Priority::High, t).is_ok());
        }
        assert_eq!(b.try_acquire(Priority::High, t), Err(Denied::OverCap));
    }

    #[test]
    fn resets_next_day() {
        let b = Budget::new(1, 0.0);
        let t = now();
        assert!(b.try_acquire(Priority::High, t).is_ok());
        assert!(b.try_acquire(Priority::High, t).is_err());
        assert!(b.try_acquire(Priority::High, t + Duration::days(1)).is_ok());
    }

    #[test]
    fn exhausted_blocks_until_next_day() {
        let b = Budget::new(100, 0.0);
        let t = now();
        b.mark_exhausted();
        assert_eq!(b.try_acquire(Priority::High, t), Err(Denied::OverCap));
        assert!(b.try_acquire(Priority::High, t + Duration::days(1)).is_ok());
    }

    #[test]
    fn breaker_opens_after_threshold_and_announces_once() {
        let b = Budget::new(100, 0.0);
        let t = now();
        for n in 1..=5 {
            b.try_acquire(Priority::High, t).unwrap();
            let ev = b.record_failure(t);
            assert_eq!(ev, if n == 5 { Some(BreakerEvent::Opened) } else { None });
        }
        assert_eq!(b.try_acquire(Priority::High, t), Err(Denied::CircuitOpen));
        assert!(b.breaker_open());
    }

    #[test]
    fn success_resets_failure_streak() {
        let b = Budget::new(100, 0.0);
        let t = now();
        for _ in 0..4 {
            b.record_failure(t);
        }
        b.record_success();
        for _ in 0..4 {
            assert_eq!(b.record_failure(t), None);
        }
        assert!(!b.breaker_open());
    }

    #[test]
    fn half_open_allows_single_probe_then_closes_on_success() {
        let b = Budget::new(100, 0.0);
        let t = now();
        for _ in 0..5 {
            b.record_failure(t);
        }
        let later = t + Duration::seconds(61);
        assert!(b.try_acquire(Priority::High, later).is_ok()); // probe
        assert_eq!(b.try_acquire(Priority::High, later), Err(Denied::CircuitOpen));
        assert_eq!(b.record_success(), Some(BreakerEvent::Closed));
        assert!(b.try_acquire(Priority::High, later).is_ok());
    }

    #[test]
    fn failed_probe_reopens_silently() {
        let b = Budget::new(100, 0.0);
        let t = now();
        for _ in 0..5 {
            b.record_failure(t);
        }
        let later = t + Duration::seconds(61);
        b.try_acquire(Priority::High, later).unwrap();
        assert_eq!(b.record_failure(later), None);
        assert_eq!(b.try_acquire(Priority::High, later), Err(Denied::CircuitOpen));
    }

    #[test]
    fn release_probe_frees_slot() {
        let b = Budget::new(100, 0.0);
        let t = now();
        for _ in 0..5 {
            b.record_failure(t);
        }
        let later = t + Duration::seconds(61);
        b.try_acquire(Priority::High, later).unwrap();
        b.release_probe();
        assert!(b.try_acquire(Priority::High, later).is_ok());
    }
}
