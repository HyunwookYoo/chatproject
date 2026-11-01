//! Scheduling candidates confer no authority: the account actor prepares and launches.
use super::store::{DueCursor, MessageDbOp, MessageDbReply, MessageRepo};
use super::types::MessageKey;
use crate::account::{PeerId, clock::Clock};
use zeroize::Zeroizing;
pub(crate) struct Dispatch {
    pub key: MessageKey,
    pub peer: PeerId,
    pub wire: Zeroizing<Vec<u8>>,
    pub ticket: u64,
}

use crate::CoreError;
use std::sync::Arc;

pub(crate) const MAX_DELAY_MS: u64 = 300_000;
pub(crate) fn retry_delay(attempts: u64) -> u64 {
    1000u64
        .checked_shl(attempts.saturating_sub(1).min(32) as u32)
        .unwrap_or(MAX_DELAY_MS)
        .min(MAX_DELAY_MS)
}

pub(crate) struct Scheduler {
    repo: MessageRepo,
    clock: Arc<dyn Clock>,
    wall: u64,
    mono: u64,
    pub excluded: Vec<PeerId>,
    pub ticket: u64,
    streak: u8,
    cursors: [Option<DueCursor>; 2],
}
impl Scheduler {
    pub(crate) fn new(repo: MessageRepo, clock: Arc<dyn Clock>) -> Self {
        Self {
            wall: clock.wall_ms(),
            mono: clock.monotonic_ms(),
            repo,
            clock,
            excluded: Vec::new(),
            ticket: 0,
            streak: 0,
            cursors: [None, None],
        }
    }
    // A wall jump never changes live due times; security still uses unix_seconds.
    pub(crate) fn now_ms(&self) -> u64 {
        self.wall
            .saturating_add(self.clock.monotonic_ms().saturating_sub(self.mono))
            .min(i64::MAX as u64)
    }
    pub(crate) async fn poll_due(&mut self, now_ms: u64) -> Result<Vec<Dispatch>, CoreError> {
        if !self.repo.due_hint(now_ms).await? {
            self.cursors = [None, None];
            return Ok(Vec::new());
        }
        // A positive hint confers no authority; every candidate still uses full validation.
        let MessageDbReply::Due(mut text, mut receipts, cursors) = self
            .repo
            .operation(MessageDbOp::Due(
                now_ms,
                self.excluded.clone(),
                self.cursors,
            ))
            .await?
        else {
            unreachable!()
        };
        self.cursors = cursors;
        let mut jobs = Vec::new();
        let mut streak = self.streak;
        while jobs.len() < 32 && (!text.is_empty() || !receipts.is_empty()) {
            let receipt = !receipts.is_empty() && (streak < 8 || text.is_empty());
            let mut job = if receipt {
                receipts.remove(0)
            } else {
                text.remove(0)
            };
            text.retain(|j| j.peer != job.peer);
            receipts.retain(|j| j.peer != job.peer);
            job.ticket = self.ticket;
            jobs.push(job);
            // Count reserved network work in dispatched(), not skipped candidates.
            streak = if receipt { streak.saturating_add(1) } else { 0 };
        }
        Ok(jobs)
    }
    pub(crate) fn dispatched(&mut self, receipt: bool) {
        self.streak = if receipt {
            self.streak.saturating_add(1).min(8)
        } else {
            0
        };
    }
}
