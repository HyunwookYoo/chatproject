use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::error::ErrorCode;
use crate::transport::{NetworkInfo, PeerInfo};

/// What this device is connected to right now (design 12.1 `ConnectionState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionSnapshot {
    pub direct_peers: u32,
    /// A ready session with another of my devices that announced itself as a mailbox (M6). The
    /// account actor owns it (`EventBus::set_mailbox_ok`); every snapshot published carries it.
    pub mailbox_ok: bool,
    /// This device uses a server queue (M8 spec 4, `QueueStateView::using_queue`): the server
    /// holds what peers could not deliver while it is off. The account actor owns it
    /// (`EventBus::set_using_queue`) as it owns `mailbox_ok`.
    pub using_queue: bool,
    /// The account has a qualified mailbox it saw lately (M8 spec 5.1, `QueueStateView::verdict`
    /// is `HasMailbox`). Spec 4: the home card shows only while `using_queue` and not this. The
    /// account actor owns it (`EventBus::set_has_mailbox`) as it owns `mailbox_ok`.
    pub has_mailbox: bool,
}

impl ConnectionSnapshot {
    pub const OFFLINE: Self =
        Self { direct_peers: 0, mailbox_ok: false, using_queue: false, has_mailbox: false };
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoreEvent {
    MessagesChanged { revision: u64, conversation: crate::message::ConversationId },
    MessageStateChanged { revision: u64, key: crate::message::MessageKey, state: crate::message::DeliveryState, cancel_requested: bool },
    AccountChanged { revision: u64, view: Box<crate::account::AccountView> },
    AccountAuthChanged { peer_id: String, installation_id: String, ready: bool },
    AccountCacheUsed { peer_id: String },
    ConnectionState(ConnectionSnapshot),
    Error { code: ErrorCode, detail: String },
    NetworkChanged(NetworkInfo),
    PeerChanged { revision: u64, peer: PeerInfo },
    ProbeTextReceived { peer_id: String, text: String },
    /// New messages to notify about, grouped by conversation (M6): one event per directly
    /// received text, and one per own-device sync once it ends.
    Notify { items: Vec<NotifyItem> },
}

/// One conversation of a [`CoreEvent::Notify`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NotifyItem {
    pub conversation: crate::message::ConversationId,
    pub peer: crate::account::PeerId,
    /// Messages from the other account stored by this processing and still unread at its end.
    pub new_messages: u32,
    /// The conversation's unread count; with `new_messages == 0` the app only updates (or, at 0,
    /// clears) the conversation's notification.
    pub unread: u64,
    /// The notification's title (M7): the contact's alias, empty without one (the app then
    /// shows "새 메시지").
    pub title: String,
    /// The notification's body (M7, `message::preview_text`): the start of the last unread text
    /// from the other account, with " 외 N개" for the N other unread ones. `None` when no text
    /// is known (nothing unread, or a push that could not be opened): the app shows
    /// "새 메시지 N개" as in M6.
    pub preview: Option<String>,
    /// With `new_messages == 0`: new texts that do not alert, because a generic push
    /// notification ("새 메시지") alerted for them already (Ruling R13). Unlike a count update,
    /// the app shows it also when no notification of the conversation is shown (the user
    /// dismissed the generic one).
    pub silenced: bool,
}

/// Receives core events. Return `false` once the receiver is gone; the bus then drops it.
/// Callbacks must be nonblocking and must not reenter the event bus or core slot:
/// they run while the bus lock (and possibly the core slot lock) is held.
pub trait EventSink: Send + Sync + 'static {
    fn send(&self, event: CoreEvent) -> bool;
}

/// Fans out core events to every subscriber in publish order. A subscriber that joins
/// late first gets the latest connection and public account state. Other events are not replayed.
#[derive(Default, Clone)]
pub struct EventBus {
    state: Arc<Mutex<BusState>>,
}

#[derive(Default)]
struct BusState {
    sinks: Vec<Arc<dyn EventSink>>,
    last_connection: Option<ConnectionSnapshot>,
    last_account: Option<CoreEvent>,
    mailbox_ok: bool,
    using_queue: bool,
    has_mailbox: bool,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn subscribe(&self, sink: Arc<dyn EventSink>) {
        let mut state = self.state.lock().expect("event bus lock poisoned");
        if let Some(snapshot) = state.last_connection
            && !sink.send(CoreEvent::ConnectionState(snapshot)) {
            return;
        }
        if let Some(snapshot) = &state.last_account
            && !sink.send(snapshot.clone()) { return; }
        state.sinks.push(sink);
    }

    pub fn publish(&self, event: CoreEvent) {
        let mut state = self.state.lock().expect("event bus lock poisoned");
        Self::publish_locked(&mut state, event);
    }

    /// The account actor's word on `mailbox_ok`: a change is published at once with the rest of
    /// the latest snapshot, and every later snapshot (the transport's) carries it. Read and sent
    /// under one lock: the transport publishes from its own task, and a snapshot it replaced
    /// must not be sent again (or kept for late subscribers).
    pub(crate) fn set_mailbox_ok(&self, ok: bool) {
        self.set_actor_word(|state| &mut state.mailbox_ok, ok);
    }

    /// The account actor's word on `using_queue`, published and carried like `mailbox_ok`.
    pub(crate) fn set_using_queue(&self, using: bool) {
        self.set_actor_word(|state| &mut state.using_queue, using);
    }

    /// The account actor's verdict (`has_mailbox`), published and carried like `mailbox_ok`.
    pub(crate) fn set_has_mailbox(&self, has: bool) {
        self.set_actor_word(|state| &mut state.has_mailbox, has);
    }

    fn set_actor_word(&self, field: fn(&mut BusState) -> &mut bool, value: bool) {
        let mut state = self.state.lock().expect("event bus lock poisoned");
        let slot = field(&mut state);
        if *slot == value {
            return;
        }
        *slot = value;
        let snapshot = state.last_connection.unwrap_or(ConnectionSnapshot::OFFLINE);
        Self::publish_locked(&mut state, CoreEvent::ConnectionState(snapshot));
    }

    fn publish_locked(state: &mut BusState, mut event: CoreEvent) {
        if let CoreEvent::ConnectionState(snapshot) = &mut event {
            snapshot.mailbox_ok = state.mailbox_ok;
            snapshot.using_queue = state.using_queue;
            snapshot.has_mailbox = state.has_mailbox;
            state.last_connection = Some(*snapshot);
        }
        if matches!(&event, CoreEvent::AccountChanged { .. }) {
            state.last_account = Some(event.clone());
        }
        state.sinks.retain(|sink| sink.send(event.clone()));
    }

    pub fn subscriber_count(&self) -> usize {
        self.state.lock().expect("event bus lock poisoned").sinks.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<CoreEvent>>,
        closed: AtomicBool,
    }

    impl RecordingSink {
        fn events(&self) -> Vec<CoreEvent> {
            self.events.lock().unwrap().clone()
        }
    }

    impl EventSink for RecordingSink {
        fn send(&self, event: CoreEvent) -> bool {
            if self.closed.load(Ordering::SeqCst) {
                return false;
            }
            self.events.lock().unwrap().push(event);
            true
        }
    }

    fn state(direct_peers: u32) -> CoreEvent {
        CoreEvent::ConnectionState(ConnectionSnapshot { direct_peers, ..ConnectionSnapshot::OFFLINE })
    }

    fn error(detail: &str) -> CoreEvent {
        CoreEvent::Error { code: ErrorCode::InvalidConfig, detail: detail.to_string() }
    }

    #[test]
    fn clone_shares_subscribers_and_latest_snapshot() {
        let bus = EventBus::new();
        let cloned = bus.clone();
        let sink = Arc::new(RecordingSink::default());
        bus.subscribe(sink.clone());
        cloned.publish(state(2));
        assert_eq!(sink.events(), vec![state(2)]);
        assert_eq!(cloned.subscriber_count(), 1);
        let late_sink = Arc::new(RecordingSink::default());
        cloned.subscribe(late_sink.clone());
        assert_eq!(late_sink.events(), vec![state(2)]);
        sink.closed.store(true, Ordering::SeqCst);
        cloned.publish(state(3));
        assert_eq!(bus.subscriber_count(), 1);
        assert_eq!(late_sink.events(), vec![state(2), state(3)]);
    }

    #[test]
    fn subscriber_gets_events_in_publish_order() {
        let bus = EventBus::new();
        let sink = Arc::new(RecordingSink::default());
        bus.subscribe(sink.clone());
        bus.publish(state(1));
        bus.publish(error("e"));
        bus.publish(state(2));
        assert_eq!(sink.events(), vec![state(1), error("e"), state(2)]);
    }

    #[test]
    fn late_subscriber_gets_latest_connection_state_first() {
        let bus = EventBus::new();
        bus.publish(state(1));
        bus.publish(state(3));
        let sink = Arc::new(RecordingSink::default());
        bus.subscribe(sink.clone());
        bus.publish(state(4));
        assert_eq!(sink.events(), vec![state(3), state(4)]);
    }

    #[test]
    fn late_subscription_replays_account_connection_but_not_messages() {
        let bus = EventBus::new();
        let account = CoreEvent::AccountChanged { revision: 7, view: Box::new(crate::account::AccountView {
            disclosure_possible: false, pending_pair_transactions: vec![], pair_resumes: vec![], pending_import_requires_remote_revoke: false,
            revision: 7, phase: crate::account::AccountPhase::Unconfigured, lock_error: None, public_peer_id: None,
            installation: None, devices: vec![], contacts: vec![], remote_records: vec![], pair: None,
            publish_intents: vec![], publication: crate::account::PublicationView::default(),
        }) };
        bus.publish(state(2));
        bus.publish(account.clone());
        bus.publish(CoreEvent::MessagesChanged { revision: 1, conversation: crate::ConversationId([1; 32]) });
        bus.publish(CoreEvent::MessageStateChanged { revision: 2, key: crate::MessageKey { conversation: crate::ConversationId([1; 32]), sender: crate::account::PeerId([2; 32]), installation: crate::account::InstallationId([3; 32]), id: crate::MessageId([4; 16]) }, state: crate::DeliveryState::Queued, cancel_requested: false });
        let sink = Arc::new(RecordingSink::default());
        bus.subscribe(sink.clone());
        assert_eq!(sink.events(), vec![state(2), account]);
    }

    #[test]
    fn errors_are_not_replayed_to_late_subscribers() {
        let bus = EventBus::new();
        bus.publish(error("old"));
        let sink = Arc::new(RecordingSink::default());
        bus.subscribe(sink.clone());
        assert_eq!(sink.events(), Vec::<CoreEvent>::new());
    }

    #[test]
    fn mailbox_ok_is_published_and_carried_by_later_snapshots() {
        let bus = EventBus::new();
        let sink = Arc::new(RecordingSink::default());
        bus.subscribe(sink.clone());
        bus.publish(state(2));
        bus.set_mailbox_ok(true);
        bus.set_mailbox_ok(true);
        bus.publish(state(3));
        bus.set_mailbox_ok(false);
        let ok = |direct_peers, mailbox_ok| {
            CoreEvent::ConnectionState(ConnectionSnapshot { direct_peers, mailbox_ok, using_queue: false, has_mailbox: false })
        };
        assert_eq!(sink.events(), vec![ok(2, false), ok(2, true), ok(3, true), ok(3, false)]);
    }

    #[test]
    fn using_queue_is_published_and_carried_beside_mailbox_ok() {
        let bus = EventBus::new();
        let sink = Arc::new(RecordingSink::default());
        bus.subscribe(sink.clone());
        bus.publish(state(1));
        bus.set_using_queue(true);
        bus.set_using_queue(true);
        bus.set_mailbox_ok(true);
        bus.publish(state(2));
        bus.set_using_queue(false);
        let snapshot = |direct_peers, mailbox_ok, using_queue| {
            CoreEvent::ConnectionState(ConnectionSnapshot { direct_peers, mailbox_ok, using_queue, has_mailbox: false })
        };
        assert_eq!(
            sink.events(),
            vec![
                snapshot(1, false, false),
                snapshot(1, false, true),
                snapshot(1, true, true),
                snapshot(2, true, true),
                snapshot(2, true, false),
            ]
        );
    }

    #[test]
    fn has_mailbox_is_published_and_carried_beside_using_queue() {
        let bus = EventBus::new();
        let sink = Arc::new(RecordingSink::default());
        bus.subscribe(sink.clone());
        bus.publish(state(1));
        bus.set_using_queue(true);
        bus.set_has_mailbox(true);
        bus.set_has_mailbox(true);
        bus.publish(state(2));
        bus.set_has_mailbox(false);
        let snapshot = |direct_peers, has_mailbox| {
            CoreEvent::ConnectionState(ConnectionSnapshot {
                direct_peers,
                using_queue: true,
                has_mailbox,
                ..ConnectionSnapshot::OFFLINE
            })
        };
        assert_eq!(
            sink.events(),
            vec![
                state(1),
                snapshot(1, false),
                snapshot(1, true),
                snapshot(2, true),
                snapshot(2, false),
            ]
        );
    }

    /// The transport publishes from its own task while the account actor sets `mailbox_ok`: a
    /// snapshot the transport replaced is never published again, and the one kept for late
    /// subscribers is the transport's latest with the latest `mailbox_ok`.
    #[test]
    fn mailbox_ok_never_republishes_a_replaced_snapshot() {
        const LAST: u32 = 20_000;
        let bus = EventBus::new();
        let sink = Arc::new(RecordingSink::default());
        bus.subscribe(sink.clone());
        let transport = {
            let bus = bus.clone();
            std::thread::spawn(move || {
                for direct_peers in 1..=LAST {
                    bus.publish(state(direct_peers));
                }
            })
        };
        let mut ok = false;
        while !transport.is_finished() {
            ok = !ok;
            bus.set_mailbox_ok(ok);
        }
        transport.join().unwrap();
        let peers: Vec<u32> = sink
            .events()
            .into_iter()
            .filter_map(|event| match event {
                CoreEvent::ConnectionState(snapshot) => Some(snapshot.direct_peers),
                _ => None,
            })
            .collect();
        assert!(
            peers.windows(2).all(|w| w[0] <= w[1]),
            "direct_peers went backwards"
        );
        let late = Arc::new(RecordingSink::default());
        bus.subscribe(late.clone());
        assert_eq!(
            late.events(),
            vec![CoreEvent::ConnectionState(ConnectionSnapshot {
                direct_peers: LAST,
                mailbox_ok: ok,
                using_queue: false,
                has_mailbox: false
            })]
        );
    }

    #[test]
    fn closed_sink_is_dropped_on_next_publish() {
        let bus = EventBus::new();
        let sink = Arc::new(RecordingSink::default());
        bus.subscribe(sink.clone());
        assert_eq!(bus.subscriber_count(), 1);
        sink.closed.store(true, Ordering::SeqCst);
        bus.publish(state(1));
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[test]
    fn subscriber_closed_before_snapshot_is_not_kept() {
        let bus = EventBus::new();
        bus.publish(state(1));
        let sink = Arc::new(RecordingSink::default());
        sink.closed.store(true, Ordering::SeqCst);
        bus.subscribe(sink.clone());
        assert_eq!(bus.subscriber_count(), 0);
    }
}
