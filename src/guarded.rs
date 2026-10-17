//! Budget, circuit breaker and one retry around a provider call.

use crate::budget::{BreakerEvent, Budget, Denied, Priority};
use crate::providers::ProviderError;
use chrono::{DateTime, Utc};
use std::future::Future;
use std::time::Duration;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardErr {
    Denied(Denied),
    Provider(ProviderError),
}

pub struct Guarded<T> {
    pub result: Result<T, GuardErr>,
    pub event: Option<BreakerEvent>,
    /// True when a provider call was actually made.
    pub called: bool,
}

/// `f` is called once, and once more after a 200 ms backoff on a retryable error.
pub async fn guarded<T, F, Fut>(budget: &Budget, prio: Priority, now: DateTime<Utc>, mut f: F) -> Guarded<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, ProviderError>>,
{
    if let Err(d) = budget.try_acquire(prio, now) {
        return Guarded { result: Err(GuardErr::Denied(d)), event: None, called: false };
    }
    let mut res = f().await;
    if let Err(e) = &res {
        if e.is_retryable() {
            tokio::time::sleep(Duration::from_millis(200)).await;
            res = f().await;
        }
    }
    match res {
        Ok(v) => {
            let event = budget.record_success();
            Guarded { result: Ok(v), event, called: true }
        }
        Err(e) => {
            let mut event = None;
            if e == ProviderError::Quota {
                budget.mark_exhausted();
                budget.release_probe();
            } else if e.is_health_failure() {
                event = budget.record_failure(Utc::now());
            } else {
                budget.release_probe();
            }
            Guarded { result: Err(GuardErr::Provider(e)), event, called: true }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn retries_once_on_server_error() {
        let b = Budget::new(10, 0.0);
        let n = AtomicU32::new(0);
        let g = guarded(&b, Priority::High, Utc::now(), || async {
            if n.fetch_add(1, Ordering::SeqCst) == 0 { Err(ProviderError::Server(503)) } else { Ok(7) }
        })
        .await;
        assert_eq!(g.result, Ok(7));
        assert_eq!(n.load(Ordering::SeqCst), 2);
        assert_eq!(b.usage().0, 1, "a retry is not a second budgeted call");
    }

    #[tokio::test]
    async fn no_retry_on_rejection_or_quota() {
        let b = Budget::new(10, 0.0);
        let n = AtomicU32::new(0);
        let g: Guarded<()> = guarded(&b, Priority::High, Utc::now(), || async {
            n.fetch_add(1, Ordering::SeqCst);
            Err(ProviderError::Rejected(401, "x".into()))
        })
        .await;
        assert!(matches!(g.result, Err(GuardErr::Provider(ProviderError::Rejected(..)))));
        assert_eq!(n.load(Ordering::SeqCst), 1);
        let g: Guarded<()> = guarded(&b, Priority::High, Utc::now(), || async { Err(ProviderError::Quota) }).await;
        assert!(g.result.is_err());
        let d: Guarded<()> = guarded(&b, Priority::High, Utc::now(), || async { Ok(()) }).await;
        assert_eq!(d.result, Err(GuardErr::Denied(Denied::OverCap)));
        assert!(!d.called);
    }

    #[tokio::test]
    async fn breaker_opens_and_denies() {
        let b = Budget::new(100, 0.0);
        let mut opened = false;
        for _ in 0..5 {
            let g: Guarded<()> = guarded(&b, Priority::High, Utc::now(), || async { Err(ProviderError::Malformed("x".into())) }).await;
            opened |= g.event == Some(BreakerEvent::Opened);
        }
        assert!(opened);
        let g: Guarded<()> = guarded(&b, Priority::High, Utc::now(), || async { Ok(()) }).await;
        assert_eq!(g.result, Err(GuardErr::Denied(Denied::CircuitOpen)));
    }
}
