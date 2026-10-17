//! Bounded, sharded work queue. One author's messages always go to the same worker,
//! so they are processed in order. When full, lowest-tier work is shed first.

use crate::model::Tier;
use crate::pipeline::{IncomingMessage, Pipeline};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;

/// How long a Quarantine-tier submit may wait for queue room before the message is
/// shed. Bounded so a stuck worker can never hang serenity's event loop.
const SUBMIT_WAIT: Duration = Duration::from_millis(50);

pub struct Dispatcher {
    txs: Vec<mpsc::Sender<IncomingMessage>>,
    pub shed: Arc<AtomicU64>,
}

impl Dispatcher {
    pub fn start(pipeline: Arc<Pipeline>, workers: usize, queue_size: usize) -> Self {
        let per = (queue_size / workers).max(1);
        let mut txs = vec![];
        for _ in 0..workers {
            let (tx, mut rx) = mpsc::channel::<IncomingMessage>(per);
            let p = pipeline.clone();
            tokio::spawn(async move {
                while let Some(m) = rx.recv().await {
                    p.handle(m, chrono::Utc::now()).await;
                }
            });
            txs.push(tx);
        }
        Dispatcher { txs, shed: Arc::new(AtomicU64::new(0)) }
    }

    fn shard(&self, m: &IncomingMessage) -> usize {
        ((m.guild_id ^ m.author_id.rotate_left(17)) as usize) % self.txs.len()
    }

    fn count_shed(&self) {
        let n = self.shed.fetch_add(1, Ordering::Relaxed) + 1;
        if n == 1 || n.is_multiple_of(100) {
            tracing::warn!("queue full; shed {n} messages so far");
        }
    }

    /// `base_tier` is the cheap, profile-only tier used to decide what may be shed.
    pub async fn submit(&self, m: IncomingMessage, base_tier: Tier) {
        let tx = &self.txs[self.shard(&m)];
        match tx.try_send(m) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(m)) => {
                if base_tier == Tier::Quarantine {
                    // Quarantine is shed last: wait a bounded time for room, but never
                    // block the caller (the gateway event loop) indefinitely.
                    if tokio::time::timeout(SUBMIT_WAIT, tx.send(m)).await.is_err() {
                        self.count_shed();
                    }
                } else {
                    self.count_shed();
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => tracing::error!("worker queue closed"),
        }
    }
}

#[cfg(test)]
mod tests {
    fn shard_for(n: usize, g: u64, a: u64) -> usize {
        ((g ^ a.rotate_left(17)) as usize) % n
    }

    #[test]
    fn same_author_same_shard() {
        for a in [1u64, 99, 123456789012345678] {
            assert_eq!(shard_for(8, 5, a), shard_for(8, 5, a));
            assert!(shard_for(8, 5, a) < 8);
        }
    }

    // Regression S5 (fixed): submitting to a full queue waits at most SUBMIT_WAIT
    // (50ms) for Quarantine-tier messages before shedding, so the gateway task never
    // hangs. The test bounds submit at 100ms; spawned + timed out so a hang would
    // fail rather than stall the suite.
    #[tokio::test]
    async fn s5_submit_to_full_queue_returns_within_bounds() {
        use super::Dispatcher;
        use crate::model::{MemberProfile, Tier};
        use crate::pipeline::IncomingMessage;
        use std::sync::atomic::AtomicU64;
        use std::sync::Arc;
        use tokio::sync::mpsc;

        fn msg(id: u64) -> IncomingMessage {
            let now = chrono::Utc::now();
            IncomingMessage {
                guild_id: 1,
                channel_id: 1,
                message_id: id,
                author_id: 7,
                content: "x".into(),
                created_at: now,
                image_urls: vec![],
                distinct_mentions: 0,
                mentions_everyone_text: false,
                can_mention_everyone: false,
                is_edit: false,
                profile: MemberProfile {
                    guild_id: 1,
                    user_id: 7,
                    account_created: now,
                    joined_at: None,
                    is_trusted: false,
                    has_default_avatar: false,
                    role_count: 0,
                },
            }
        }

        // A queue of depth 1 with no consumer: once full it stays full.
        let (tx, _rx) = mpsc::channel(1);
        let d = Arc::new(Dispatcher { txs: vec![tx], shed: Arc::new(AtomicU64::new(0)) });
        d.submit(msg(1), Tier::Quarantine).await; // fills the single slot

        let d2 = d.clone();
        let blocked = tokio::spawn(async move { d2.submit(msg(2), Tier::Quarantine).await });
        let r = tokio::time::timeout(std::time::Duration::from_millis(100), blocked).await;
        assert!(r.is_ok(), "submit to a full queue blocked the caller for over 100ms");
    }
}
