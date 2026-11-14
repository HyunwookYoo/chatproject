//! In-memory token buckets (spec 9.3). Lost on restart by design: the proxy
//! keeps no state that outlives the process.

use std::collections::HashMap;
use std::hash::Hash;
use std::net::IpAddr;
use std::sync::{Mutex, PoisonError};

use sha2::{Digest, Sha256};

/// Rates per minute and burst sizes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Limits {
    /// `/push` items per handle.
    pub handle_per_minute: u32,
    pub handle_burst: u32,
    /// `/handle` requests per client IP.
    pub ip_per_minute: u32,
    pub ip_burst: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            handle_per_minute: 60,
            handle_burst: 20,
            ip_per_minute: 30,
            ip_burst: 30,
        }
    }
}

const MINUTE_MS: u64 = 60_000;
/// One token in bucket units. A bucket refills `per_minute` units per ms, so
/// `per_minute` tokens a minute, with integer arithmetic only.
const UNIT: u64 = MINUTE_MS;

struct Bucket {
    units: u64,
    at_ms: u64,
}

struct Buckets<K> {
    per_minute: u64,
    capacity: u64,
    map: HashMap<K, Bucket>,
    last_sweep_ms: u64,
}

impl<K: Hash + Eq> Buckets<K> {
    fn new(per_minute: u32, burst: u32) -> Self {
        Buckets {
            per_minute: u64::from(per_minute),
            capacity: u64::from(burst) * UNIT,
            map: HashMap::new(),
            last_sweep_ms: 0,
        }
    }

    fn take(&mut self, key: K, now_ms: u64) -> bool {
        self.sweep(now_ms);
        let (per_minute, capacity) = (self.per_minute, self.capacity);
        let b = self.map.entry(key).or_insert(Bucket {
            units: capacity,
            at_ms: now_ms,
        });
        b.units = refilled(b, now_ms, per_minute, capacity);
        b.at_ms = b.at_ms.max(now_ms);
        if b.units >= UNIT {
            b.units -= UNIT;
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
        let (per_minute, capacity) = (self.per_minute, self.capacity);
        self.map
            .retain(|_, b| refilled(b, now_ms, per_minute, capacity) < capacity);
    }
}

fn refilled(b: &Bucket, now_ms: u64, per_minute: u64, capacity: u64) -> u64 {
    let gained = now_ms.saturating_sub(b.at_ms).saturating_mul(per_minute);
    b.units.saturating_add(gained).min(capacity)
}

pub(crate) struct Limiter {
    handles: Mutex<Buckets<[u8; 32]>>,
    ips: Mutex<Buckets<IpAddr>>,
}

impl Limiter {
    pub(crate) fn new(limits: &Limits) -> Self {
        Limiter {
            handles: Mutex::new(Buckets::new(limits.handle_per_minute, limits.handle_burst)),
            ips: Mutex::new(Buckets::new(limits.ip_per_minute, limits.ip_burst)),
        }
    }

    /// Keyed by SHA-256 of the handle bytes, so the map holds no handle.
    pub(crate) fn allow_handle(&self, handle: &[u8], now_ms: u64) -> bool {
        let key: [u8; 32] = Sha256::digest(handle).into();
        self.handles
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take(key, now_ms)
    }

    pub(crate) fn allow_ip(&self, ip: IpAddr, now_ms: u64) -> bool {
        self.ips
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take(ip, now_ms)
    }
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
}
