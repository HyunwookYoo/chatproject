//! The account's mailbox verdict and the queue a device wants (M8 spec 5), as pure functions
//! over what a device knows: the actor gathers the facts (`account/manager/mailbox.rs`), the
//! store keeps them (`message/mailbox_store.rs`).

use crate::account::InstallationId;

/// A mailbox qualifies with at least this much of the 7-day window up, per mille (spec 5.1).
pub(crate) const QUALIFIED_PERMILLE: u32 = 900;
/// ... and more than this long recorded since its role was on (spec 5.1: a PC just set as a
/// mailbox does not qualify within the hour).
pub(crate) const QUALIFIED_RECORDED_MS: u64 = 86_400_000;
/// A mailbox counts only if seen (an own-device session, or its `Announce`) this recently.
pub(crate) const SEEN_MS: u64 = 86_400_000;
/// The qualification and the verdict are worked out again this often (scheduler time).
pub(crate) const VERDICT_EVERY_MS: u64 = 3_600_000;

/// The user's server queue setting (spec 5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueMode {
    /// A queue only while the account has no qualified mailbox.
    Auto,
    /// Always a queue.
    Always,
    /// Never a queue, nor any request to the queue server.
    Never,
}

/// Whether the account has a qualified mailbox it has seen lately (spec 5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    HasMailbox { installation: InstallationId },
    NoMailbox,
}

/// What one device knows of one of the account's devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MailboxFact {
    pub installation: InstallationId,
    /// Its mailbox role is on.
    pub mailbox: bool,
    /// It said it qualifies (or, for this device, it does).
    pub qualified: bool,
    /// Wall time it was last seen (`None`: never).
    pub last_seen_ms: Option<u64>,
}

impl MailboxFact {
    /// Spec 5.1 rules 1 to 3 at `now_ms`. A last-seen time ahead of `now_ms` (the wall clock went
    /// back) counts as seen; the store's hourly run sets such a time to now, so the 24 hours run
    /// from there.
    pub(crate) fn counts(&self, now_ms: u64) -> bool {
        self.mailbox
            && self.qualified
            && self
                .last_seen_ms
                .is_some_and(|seen| now_ms.saturating_sub(seen) <= SEEN_MS)
    }
}

/// The verdict of this device (`me`, whose last-seen time is always now and is not read) given
/// what it knows of the account's other devices. This device first, then `others` in order.
pub(crate) fn verdict(me: &MailboxFact, others: &[MailboxFact], now_ms: u64) -> Verdict {
    let me = MailboxFact {
        last_seen_ms: Some(now_ms),
        ..*me
    };
    std::iter::once(&me)
        .chain(others.iter().filter(|f| f.installation != me.installation))
        .find(|f| f.counts(now_ms))
        .map_or(Verdict::NoMailbox, |f| Verdict::HasMailbox {
            installation: f.installation,
        })
}

/// Whether what is known of one device going from `before` (`None`: nothing) to `after` can
/// change the verdict at `now_ms`. Anything else leaves it as it is until the hourly run.
pub(crate) fn changes_verdict(
    before: Option<&MailboxFact>,
    after: &MailboxFact,
    now_ms: u64,
) -> bool {
    before.is_some_and(|b| b.counts(now_ms)) != after.counts(now_ms)
}

/// Spec 5.3: whether this device wants a queue. A qualified mailbox, this device included,
/// means none in `Auto`.
pub(crate) fn wants_queue(mode: QueueMode, verdict: &Verdict) -> bool {
    match mode {
        QueueMode::Auto => *verdict == Verdict::NoMailbox,
        QueueMode::Always => true,
        QueueMode::Never => false,
    }
}

/// Spec 5.1 rule 2: up at least 900 per mille of the 7-day window, and more than 24 hours
/// recorded.
pub(crate) fn qualifies(uptime_permille: u32, recorded_ms: u64) -> bool {
    uptime_permille >= QUALIFIED_PERMILLE && recorded_ms > QUALIFIED_RECORDED_MS
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: u64 = 1_800_000_000_000;
    const HOUR: u64 = 3_600_000;
    const ME: InstallationId = InstallationId([1; 32]);
    const OTHER: InstallationId = InstallationId([2; 32]);
    const THIRD: InstallationId = InstallationId([3; 32]);

    fn fact(
        installation: InstallationId,
        mailbox: bool,
        qualified: bool,
        last_seen_ms: Option<u64>,
    ) -> MailboxFact {
        MailboxFact {
            installation,
            mailbox,
            qualified,
            last_seen_ms,
        }
    }
    fn plain_me() -> MailboxFact {
        fact(ME, false, false, None)
    }
    fn has(installation: InstallationId) -> Verdict {
        Verdict::HasMailbox { installation }
    }

    #[test]
    fn qualifies_needs_900_permille_and_more_than_24_hours() {
        let day = QUALIFIED_RECORDED_MS;
        assert!(qualifies(900, day + 1));
        assert!(qualifies(1000, 7 * day));
        assert!(!qualifies(899, 7 * day), "under 900 per mille");
        assert!(!qualifies(1000, day), "exactly 24 hours is not more");
        // A mailbox just set: up all of the last hour, but recorded for an hour only.
        assert!(!qualifies(1000, HOUR), "fresh mailbox under 24 h");
        assert!(!qualifies(0, 0));
    }

    #[test]
    fn verdict_finds_a_qualified_mailbox_seen_lately() {
        let seen = Some(NOW - HOUR);
        let others = [fact(OTHER, true, true, seen)];
        assert_eq!(verdict(&plain_me(), &others, NOW), has(OTHER));
        // Rule 1: the role off; rule 2: not qualified; rule 3: never seen.
        for other in [
            fact(OTHER, false, true, seen),
            fact(OTHER, true, false, seen),
            fact(OTHER, true, true, None),
        ] {
            assert_eq!(verdict(&plain_me(), &[other], NOW), Verdict::NoMailbox);
        }
        assert_eq!(verdict(&plain_me(), &[], NOW), Verdict::NoMailbox);
    }

    #[test]
    fn mailbox_unseen_24_hours_gives_no_mailbox() {
        let at = |seen: u64| verdict(&plain_me(), &[fact(OTHER, true, true, Some(seen))], NOW);
        assert_eq!(
            at(NOW - SEEN_MS),
            has(OTHER),
            "24 hours exactly still counts"
        );
        assert_eq!(at(NOW - SEEN_MS - 1), Verdict::NoMailbox);
        // The wall clock went back since: seen.
        assert_eq!(at(NOW + HOUR), has(OTHER));
    }

    #[test]
    fn me_as_the_qualified_mailbox() {
        // My own last-seen time is not read: always now.
        let me = fact(ME, true, true, None);
        assert_eq!(verdict(&me, &[], NOW), has(ME));
        // Me first, even with another qualified mailbox.
        let others = [fact(OTHER, true, true, Some(NOW))];
        assert_eq!(verdict(&me, &others, NOW), has(ME));
        // An unqualified mailbox (me) uses another's verdict, or none.
        let me = fact(ME, true, false, Some(NOW));
        assert_eq!(verdict(&me, &others, NOW), has(OTHER));
        assert_eq!(verdict(&me, &[], NOW), Verdict::NoMailbox);
        // A row about me among the others is not mine to trust.
        let stale_me = [fact(ME, true, true, Some(NOW))];
        assert_eq!(verdict(&plain_me(), &stale_me, NOW), Verdict::NoMailbox);
    }

    #[test]
    fn verdict_takes_the_first_counting_device() {
        let others = [
            fact(OTHER, true, false, Some(NOW)),
            fact(THIRD, true, true, Some(NOW)),
        ];
        assert_eq!(verdict(&plain_me(), &others, NOW), has(THIRD));
    }

    /// Spec 5.3, one line per row of the table.
    #[test]
    fn wants_queue_follows_the_table() {
        let mailbox = has(OTHER);
        let none = Verdict::NoMailbox;
        assert!(!wants_queue(QueueMode::Auto, &mailbox));
        assert!(wants_queue(QueueMode::Auto, &none));
        for verdict in [mailbox, none, has(ME)] {
            assert!(wants_queue(QueueMode::Always, &verdict));
            assert!(!wants_queue(QueueMode::Never, &verdict));
        }
        // The qualified mailbox itself wants none; an unqualified one does.
        let me = fact(ME, true, true, None);
        assert!(!wants_queue(QueueMode::Auto, &verdict(&me, &[], NOW)));
        let me = fact(ME, true, false, None);
        assert!(wants_queue(QueueMode::Auto, &verdict(&me, &[], NOW)));
    }

    #[test]
    fn only_a_change_of_what_counts_changes_the_verdict() {
        let seen = Some(NOW);
        let mailbox = fact(OTHER, true, true, seen);
        // The same Announce again.
        assert!(!changes_verdict(Some(&mailbox), &mailbox, NOW));
        // A newer last-seen time of a device seen lately.
        let earlier = fact(OTHER, true, true, Some(NOW - HOUR));
        assert!(!changes_verdict(Some(&earlier), &mailbox, NOW));
        // Qualified on or off, the role on or off, first heard of.
        assert!(changes_verdict(
            Some(&fact(OTHER, true, false, seen)),
            &mailbox,
            NOW
        ));
        assert!(changes_verdict(
            Some(&mailbox),
            &fact(OTHER, false, true, seen),
            NOW
        ));
        assert!(changes_verdict(None, &mailbox, NOW));
        assert!(!changes_verdict(
            None,
            &fact(OTHER, false, false, seen),
            NOW
        ));
        // Seen again after more than 24 hours.
        let stale = fact(OTHER, true, true, Some(NOW - SEEN_MS - 1));
        assert!(changes_verdict(Some(&stale), &mailbox, NOW));
    }
}
