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
/// late first gets the latest connection state; other events are not replayed.
#[derive(Default, Clone)]
pub struct EventBus {
    state: Arc<Mutex<BusState>>,
}

#[derive(Default)]
struct BusState {
    sinks: Vec<Arc<dyn EventSink>>,
    last_connection: Option<ConnectionSnapshot>,
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
        state.sinks.push(sink);
    }

    pub fn publish(&self, event: CoreEvent) {
        let mut state = self.state.lock().expect("event bus lock poisoned");
        if let CoreEvent::ConnectionState(snapshot) = &event {
            state.last_connection = Some(*snapshot);
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
