//! In-memory token buckets. Lost on restart by design: the servers keep no
//! rate-limit state that outlives the process.

use std::collections::HashMap;
use std::hash::Hash;

const MINUTE_MS: u64 = 60_000;
const HOUR_MS: u64 = 3_600_000;

struct Bucket {
    units: u64,
    at_ms: u64,
}

/// One token bucket per key: `rate` tokens per window, at most `burst` at
/// once. A key not seen yet starts full.
pub struct Buckets<K> {
    /// One token in bucket units: the window in ms. A bucket refills `rate`
    /// units per ms, so `rate` tokens a window, with integer arithmetic only.
    unit: u64,
    rate: u64,
    capacity: u64,
    map: HashMap<K, Bucket>,
    last_sweep_ms: u64,
}

impl<K: Hash + Eq> Buckets<K> {
    pub fn new(per_minute: u32, burst: u32) -> Self {
        Self::with_window(MINUTE_MS, per_minute, burst)
    }

    pub fn per_hour(per_hour: u32, burst: u32) -> Self {
        Self::with_window(HOUR_MS, per_hour, burst)
    }

    fn with_window(window_ms: u64, rate: u32, burst: u32) -> Self {
        Buckets {
            unit: window_ms,
            rate: u64::from(rate),
            capacity: u64::from(burst) * window_ms,
            map: HashMap::new(),
            last_sweep_ms: 0,
        }
    }

    /// Takes one token from `key`'s bucket; `false` if it is empty.
    pub fn take(&mut self, key: K, now_ms: u64) -> bool {
        self.take_n(key, 1, now_ms)
    }

    /// Takes `n` tokens from `key`'s bucket, or none; `false` if it holds fewer.
    pub fn take_n(&mut self, key: K, n: u64, now_ms: u64) -> bool {
        self.sweep(now_ms);
        let (unit, rate, capacity) = (self.unit, self.rate, self.capacity);
        let b = self.map.entry(key).or_insert(Bucket {
            units: capacity,
            at_ms: now_ms,
        });
        b.units = refilled(b, now_ms, rate, capacity);
        b.at_ms = b.at_ms.max(now_ms);
        let cost = n.saturating_mul(unit);
        if b.units >= cost {
            b.units -= cost;
            true
        } else {
            false
        }
    }

    /// Once a minute, drop buckets that have refilled: they hold nothing a
    /// fresh bucket would not.
    fn sweep(&mut self, now_ms: u64) {
        if now_ms.saturating_sub(self.last_sweep_ms) < MINUTE_MS {
            return;
        }
        self.last_sweep_ms = now_ms;
        let (rate, capacity) = (self.rate, self.capacity);
        self.map
            .retain(|_, b| refilled(b, now_ms, rate, capacity) < capacity);
    }
}

fn refilled(b: &Bucket, now_ms: u64, rate: u64, capacity: u64) -> u64 {
    let gained = now_ms.saturating_sub(b.at_ms).saturating_mul(rate);
    b.units.saturating_add(gained).min(capacity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sweep_drops_refilled_buckets_only() {
        let mut b = Buckets::new(1, 20);
        assert!(b.take(1u8, 60_000));
        for _ in 0..20 {
            b.take(2u8, 60_000);
        }
        // 61 s later bucket 1 is full again; bucket 2 has gained one token only.
        b.sweep(121_000);
        assert!(!b.map.contains_key(&1));
        assert!(b.map.contains_key(&2));
        assert_eq!(b.map.len(), 1);
    }

    #[test]
    fn refill_is_per_minute_and_capped_at_burst() {
        let mut b = Buckets::new(60, 2);
        assert!(b.take(0u8, 0));
        assert!(b.take(0u8, 0));
        assert!(!b.take(0u8, 0));
        assert!(!b.take(0u8, 999));
        assert!(b.take(0u8, 1_000));
        // An hour idle still allows only the burst.
        assert!(b.take(0u8, 3_601_000));
        assert!(b.take(0u8, 3_601_000));
        assert!(!b.take(0u8, 3_601_000));
    }

    #[test]
    fn take_n_takes_all_or_nothing() {
        let mut b = Buckets::per_hour(3_600, 10);
        assert!(b.take_n(0u8, 7, 0));
        assert!(!b.take_n(0u8, 4, 0));
        assert!(b.take_n(0u8, 3, 0));
        assert!(!b.take(0u8, 0));
        // One token a second: four seconds pay for four.
        assert!(!b.take_n(0u8, 4, 3_999));
        assert!(b.take_n(0u8, 4, 4_000));
        assert!(!b.take_n(0u8, 11, 3_600_000));
        assert!(b.take_n(0u8, 10, 3_600_000));
    }

    #[test]
    fn per_hour_refills_per_hour_and_caps_at_burst() {
        // 500 an hour is one token every 7.2 s, which no per-minute integer rate gives.
        let mut b = Buckets::per_hour(500, 2);
        assert!(b.take(0u8, 0));
        assert!(b.take(0u8, 0));
        assert!(!b.take(0u8, 0));
        assert!(!b.take(0u8, 7_199));
        assert!(b.take(0u8, 7_200));
        assert!(!b.take(0u8, 7_200));
        assert!(b.take(0u8, 3_600_000));
        assert!(b.take(0u8, 3_600_000));
        assert!(!b.take(0u8, 3_600_000));
    }
}
