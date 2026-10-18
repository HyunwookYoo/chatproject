use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    fn unix_seconds(&self) -> u64;
    fn monotonic_ms(&self) -> u64;
}

pub struct SystemClock {
    origin: Instant,
}

impl Default for SystemClock {
    fn default() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Clock for SystemClock {
    fn unix_seconds(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn monotonic_ms(&self) -> u64 {
        self.origin.elapsed().as_millis().min(u64::MAX as u128) as u64
    }
}

#[cfg(any(test, all(feature = "test-hooks", debug_assertions)))]
pub use fake::FakeClock;

#[cfg(any(test, all(feature = "test-hooks", debug_assertions)))]
mod fake {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::Clock;

    pub struct FakeClock {
        unix_seconds: AtomicU64,
        monotonic_ms: AtomicU64,
    }

    impl FakeClock {
        pub fn new(unix_seconds: u64, monotonic_ms: u64) -> Self {
            Self {
                unix_seconds: AtomicU64::new(unix_seconds),
                monotonic_ms: AtomicU64::new(monotonic_ms),
            }
        }

        pub fn set_unix_seconds(&self, value: u64) {
            self.unix_seconds.store(value, Ordering::SeqCst);
        }

        pub fn advance_monotonic_ms(&self, elapsed: u64) {
            self.monotonic_ms.fetch_add(elapsed, Ordering::SeqCst);
        }
    }

    impl Clock for FakeClock {
        fn unix_seconds(&self) -> u64 {
            self.unix_seconds.load(Ordering::SeqCst)
        }

        fn monotonic_ms(&self) -> u64 {
            self.monotonic_ms.load(Ordering::SeqCst)
        }
    }
}
