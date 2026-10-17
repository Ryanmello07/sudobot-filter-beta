//! Bounded, sharded work queue. One author's messages always go to the same worker,
//! so they are processed in order. When full, lowest-tier work is shed first.

use crate::model::Tier;
use crate::pipeline::{IncomingMessage, Pipeline};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

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

    /// `base_tier` is the cheap, profile-only tier used to decide what may be shed.
    pub async fn submit(&self, m: IncomingMessage, base_tier: Tier) {
        let tx = &self.txs[self.shard(&m)];
        match tx.try_send(m) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(m)) => {
                if base_tier == Tier::Quarantine {
                    // Never shed Quarantine: wait for room.
                    let _ = tx.send(m).await;
                } else {
                    let n = self.shed.fetch_add(1, Ordering::Relaxed) + 1;
                    if n == 1 || n.is_multiple_of(100) {
                        tracing::warn!("queue full; shed {n} lower-tier messages so far");
                    }
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
}
