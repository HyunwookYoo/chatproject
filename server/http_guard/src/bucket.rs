//! In-memory token buckets. Lost on restart by design: the servers keep no
//! rate-limit state that outlives the process.

use std::collections::HashMap;
use std::hash::Hash;
use std::net::{IpAddr, Ipv6Addr};

const MINUTE_MS: u64 = 60_000;
/// One token in bucket units. A bucket refills `per_minute` units per ms, so
/// `per_minute` tokens a minute, with integer arithmetic only.
const UNIT: u64 = MINUTE_MS;

struct Bucket {
    units: u64,
    at_ms: u64,
}

/// One token bucket per key: `per_minute` tokens a minute, at most `burst`
/// at once. A key not seen yet starts full.
pub struct Buckets<K> {
    per_minute: u64,
    capacity: u64,
    map: HashMap<K, Bucket>,
    last_sweep_ms: u64,
}

impl<K: Hash + Eq> Buckets<K> {
    pub fn new(per_minute: u32, burst: u32) -> Self {
        Buckets {
            per_minute: u64::from(per_minute),
            capacity: u64::from(burst) * UNIT,
            map: HashMap::new(),
            last_sweep_ms: 0,
        }
    }

    /// Takes one token from `key`'s bucket; `false` if it is empty.
    pub fn take(&mut self, key: K, now_ms: u64) -> bool {
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

/// One client for a per-client limit: an IPv4 address, or an IPv6 /64 (the
/// smallest block a provider hands out, and every address in it is the
/// holder's). IPv4-mapped IPv6 is the IPv4 address.
pub fn client_key(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V4(_) => ip,
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(Ipv6Addr::from_bits(v6.to_bits() & !u128::from(u64::MAX))),
        },
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
