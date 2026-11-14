//! Mailbox devices and own-device sync (M6).
//!
//! Items here are consumed by the later M6 tasks, hence the blanket `dead_code` allowance
//! until they land.
#![allow(dead_code)]

pub(crate) mod wire;

/// Account frame kind of the own-device sync protocol.
pub(crate) const KIND: u8 = 18;
/// Most log entries in one `SyncResp` page (and in one `Handoff`).
pub(crate) const PAGE_MAX_ENTRIES: usize = 64;
/// How long a mailbox keeps its log: 30 days.
pub(crate) const MAILBOX_RETENTION_MS: u64 = 2_592_000_000;
/// Back-off cap of a mailbox's retries (spec 7: handed-off ciphertexts).
pub(crate) const MAILBOX_MAX_DELAY_MS: u64 = 30_000;
/// The window a mailbox's uptime is reported over (spec 8.4): 7 days.
pub(crate) const UPTIME_WINDOW_MS: u64 = 604_800_000;
/// A mailbox up again within this long of its last uptime record continues the same period.
pub(crate) const UPTIME_GAP_MS: u64 = 120_000;
