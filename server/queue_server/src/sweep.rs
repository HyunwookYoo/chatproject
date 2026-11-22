//! Hourly expiry (spec 9.3): entries older than the TTL, acked hashes past
//! their drop time, queues with no command for 30 days. In bounded steps, so
//! requests run between them.

use std::time::{Duration, Instant};

use tokio::time::MissedTickBehavior;

use crate::app::Clock;
use crate::log::{LogSink, Logger};
use crate::store::{Store, StoreError, SweepCounts};

pub const SWEEP_EVERY: Duration = Duration::from_secs(3_600);
/// Rows of each kind one store transaction deletes.
const SWEEP_BATCH: usize = 1_000;

/// Deletes everything expired at `now`, one bounded transaction at a time,
/// until a step finds nothing.
pub async fn sweep(store: &Store, now: u64) -> Result<SweepCounts, StoreError> {
    let mut total = SweepCounts::default();
    loop {
        let step = store.run(move |tx| tx.sweep(now, SWEEP_BATCH)).await?;
        if step.is_empty() {
            return Ok(total);
        }
        total.add(step);
    }
}

/// Sweeps at start and then every [`SWEEP_EVERY`], forever. A failed sweep
/// is logged and tried again at the next tick.
pub async fn run_sweeper(store: Store, clock: Clock, log: LogSink) {
    let logger = Logger { clock, sink: log };
    let mut tick = tokio::time::interval(SWEEP_EVERY);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        let start = Instant::now();
        let result = sweep(&store, (logger.clock)()).await;
        logger.swept(result, start.elapsed());
    }
}
