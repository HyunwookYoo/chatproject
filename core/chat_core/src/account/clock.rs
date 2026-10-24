use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    fn unix_seconds(&self) -> u64;
    fn monotonic_ms(&self) -> u64;
    fn wall_ms(&self) -> u64 {
        self.unix_seconds().saturating_mul(1000)
    }
    fn wake(&self) -> Option<std::sync::Arc<tokio::sync::Notify>> {
        None
    }
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
    fn wall_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .min(u64::MAX as u128) as u64
    }
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
    use std::sync::Mutex;

    use super::Clock;

    pub struct FakeClock {
        time: Mutex<(u64, u64)>,
        wake: std::sync::Arc<tokio::sync::Notify>,
    }

    impl FakeClock {
        pub fn new(unix_seconds: u64, monotonic_ms: u64) -> Self {
            Self {
                time: Mutex::new((unix_seconds, monotonic_ms)),
                wake: Default::default(),
            }
        }

        /// Commits both clocks together; failed arithmetic neither mutates nor wakes.
        pub fn advance_seconds(&self, seconds: u64) -> Option<(u64, u64)> {
            if seconds > 31_536_000 {
                return None;
            }
            let elapsed = seconds.checked_mul(1000)?;
            let mut time = self.time.lock().expect("clock lock poisoned");
            let wall = time.0.checked_add(seconds)?;
            wall.checked_mul(1000)?;
            let mono = time.1.checked_add(elapsed)?;
            *time = (wall, mono);
            drop(time);
            self.wake.notify_one();
            Some((wall, mono))
        }

        pub fn set_unix_seconds(&self, value: u64) {
            self.time.lock().expect("clock lock poisoned").0 = value;
            self.wake.notify_one();
        }

        pub fn advance_monotonic_ms(&self, elapsed: u64) {
            let mut time = self.time.lock().expect("clock lock poisoned");
            time.1 = time.1.wrapping_add(elapsed);
            drop(time);
            self.wake.notify_one();
        }
    }

    impl Clock for FakeClock {
        fn wake(&self) -> Option<std::sync::Arc<tokio::sync::Notify>> {
            Some(self.wake.clone())
        }
        fn unix_seconds(&self) -> u64 {
            self.time.lock().expect("clock lock poisoned").0
        }

        fn monotonic_ms(&self) -> u64 {
            self.time.lock().expect("clock lock poisoned").1
        }
    }
}
