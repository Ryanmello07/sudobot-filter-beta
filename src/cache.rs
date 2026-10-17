//! Small TTL cache.

use std::collections::HashMap;
use std::hash::Hash;
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct TtlCache<K, V> {
    ttl: Duration,
    max: usize,
    map: Mutex<HashMap<K, (Instant, V)>>,
}

impl<K: Hash + Eq + Clone, V: Clone> TtlCache<K, V> {
    pub fn new(ttl: Duration, max: usize) -> Self {
        Self { ttl, max, map: Mutex::new(HashMap::new()) }
    }

    pub fn get(&self, k: &K) -> Option<V> {
        let mut m = self.map.lock().unwrap();
        match m.get(k) {
            Some((at, v)) if at.elapsed() < self.ttl => Some(v.clone()),
            Some(_) => {
                m.remove(k);
                None
            }
            None => None,
        }
    }

    pub fn put(&self, k: K, v: V) {
        let mut m = self.map.lock().unwrap();
        if m.len() >= self.max {
            let ttl = self.ttl;
            m.retain(|_, (at, _)| at.elapsed() < ttl);
            if m.len() >= self.max {
                // still full: drop the oldest entry
                if let Some(oldest) = m.iter().min_by_key(|(_, (at, _))| *at).map(|(k, _)| k.clone()) {
                    m.remove(&oldest);
                }
            }
        }
        m.insert(k, (Instant::now(), v));
    }

    pub fn invalidate(&self, k: &K) {
        self.map.lock().unwrap().remove(k);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_miss_and_expiry() {
        let c = TtlCache::new(Duration::from_millis(30), 10);
        assert_eq!(c.get(&1), None);
        c.put(1, "a");
        assert_eq!(c.get(&1), Some("a"));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(c.get(&1), None);
    }

    #[test]
    fn evicts_oldest_when_full() {
        let c = TtlCache::new(Duration::from_secs(60), 2);
        c.put(1, 1);
        std::thread::sleep(Duration::from_millis(2));
        c.put(2, 2);
        std::thread::sleep(Duration::from_millis(2));
        c.put(3, 3);
        assert_eq!(c.get(&1), None);
        assert_eq!(c.get(&2), Some(2));
        assert_eq!(c.get(&3), Some(3));
    }

    #[test]
    fn invalidate_drops_entry() {
        let c = TtlCache::new(Duration::from_secs(60), 10);
        c.put(1, "a");
        c.invalidate(&1);
        assert_eq!(c.get(&1), None);
        c.invalidate(&1); // missing key: no-op
    }
}
