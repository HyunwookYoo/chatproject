use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::error::ErrorCode;
use crate::transport::{NetworkInfo, PeerInfo};

/// What this device is connected to right now (design 12.1 `ConnectionState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionSnapshot {
    pub direct_peers: u32,
    pub mailbox_ok: bool,
    /// `None` while this user has no server queue.
    pub queue_ok: Option<bool>,
}

impl ConnectionSnapshot {
    pub const OFFLINE: Self = Self { direct_peers: 0, mailbox_ok: false, queue_ok: None };
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
        if let CoreEvent::ConnectionState(snapshot) = &event {
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
