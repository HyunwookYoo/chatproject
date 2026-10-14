pub mod protocol;

use crate::{CoreError, CoreEvent, ErrorCode, EventBus, EventSink, NetworkHandle};
use futures_util::{SinkExt, StreamExt, stream::FuturesUnordered};
use protocol::{Incoming, Outgoing};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, protocol::WebSocketConfig},
};
use tokio_util::sync::CancellationToken;

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
#[derive(Default)]
pub(crate) struct HookState {
    active: AtomicBool,
    sequence: AtomicU64,
    pub cancel: CancellationToken,
}
struct Active(Arc<HookState>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.active.store(false, Ordering::Release);
    }
}
struct Sink {
    sender: mpsc::Sender<CoreEvent>,
    overflow: CancellationToken,
}
impl EventSink for Sink {
    fn send(&self, event: CoreEvent) -> bool {
        match self.sender.try_send(event) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.overflow.cancel();
                false
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }
}
fn failed() -> CoreError {
    CoreError::new(ErrorCode::TransportFailed, "control connection failed")
}
async fn send(socket: &mut Socket, record: Outgoing<'_>) -> Result<(), CoreError> {
    let text = serde_json::to_string(&record).map_err(|_| failed())?;
    tokio::time::timeout(
        Duration::from_secs(5),
        socket.send(Message::Text(text.into())),
    )
    .await
    .map_err(|_| failed())?
    .map_err(|_| failed())
}
pub(crate) async fn start(
    handle: &tokio::runtime::Handle,
    state: Arc<HookState>,
    network: NetworkHandle,
    bus: EventBus,
    instance: String,
    control_url: String,
    name: String,
) -> Result<(), CoreError> {
    protocol::validate_control(&control_url)?;
    if name.is_empty() || name.len() > 128 {
        return Err(protocol::invalid());
    }
    if state.active.swap(true, Ordering::AcqRel) {
        return Err(CoreError::new(
            ErrorCode::Busy,
            "control connection already active",
        ));
    }
    let active = Active(state.clone());
    let (ready, received) = oneshot::channel();
    handle.spawn(async move {
        let _active = active;
        let mut ready = Some(ready);
        let result = tokio::select! {
            biased;
            _ = state.cancel.cancelled() => Err(failed()),
            result = run(&state, network, &bus, &instance, &control_url, &name, &mut ready) => result,
        };
        if let Err(error) = result {
            if let Some(ready) = ready.take() { let _ = ready.send(Err(error.clone())); }
            bus.publish(CoreEvent::Error { code: error.code, detail: error.detail });
        }
    });
    received.await.map_err(|_| failed())?
}
async fn run(
    state: &HookState,
    network: NetworkHandle,
    bus: &EventBus,
    instance: &str,
    control_url: &str,
    name: &str,
    ready: &mut Option<oneshot::Sender<Result<(), CoreError>>>,
) -> Result<(), CoreError> {
    let config = WebSocketConfig::default()
        .max_message_size(Some(64 * 1024))
        .max_frame_size(Some(64 * 1024));
    let (mut socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async_with_config(control_url, Some(config), false),
    )
    .await
    .map_err(|_| failed())?
    .map_err(|_| failed())?;
    send(
        &mut socket,
        Outgoing::Hello {
            protocol: protocol::PROTOCOL,
            name,
            instance,
            relay: protocol::RELAY,
        },
    )
    .await?;
    let message = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .map_err(|_| failed())?
        .ok_or_else(failed)?
        .map_err(|_| failed())?;
    let Message::Text(text) = message else {
        return Err(protocol::invalid());
    };
    let Incoming::Config { relay } = protocol::parse(&text)? else {
        return Err(protocol::invalid());
    };
    let relay = relay
        .map(|value| {
            protocol::validate_relay(&value)?;
            value.parse().map_err(|_| protocol::invalid())
        })
        .transpose()?;
    network.configure_local(relay).await?;
    let (sender, mut events) = mpsc::channel(256);
    let overflow = CancellationToken::new();
    bus.subscribe(Arc::new(Sink {
        sender,
        overflow: overflow.clone(),
    }));
    let _ = ready.take().unwrap().send(Ok(()));
    let mut ids = protocol::RequestIds::default();
    let mut pending: FuturesUnordered<
        futures_util::future::BoxFuture<'static, (u64, Result<crate::NetworkReply, CoreError>)>,
    > = FuturesUnordered::new();
    let result = tokio::select! {
        biased;
        _ = overflow.cancelled() => Err(CoreError::new(ErrorCode::TransportFailed, "control event queue overflow")),
        result = async {
        loop {
            tokio::select! {
                Some(event) = events.recv() => {
                    let seq = state.sequence.load(Ordering::Relaxed) + 1;
                    send(&mut socket, Outgoing::Event { seq, instance, event }).await?;
                    // Discarded queued events never consume a sequence. A failed in-flight
                    // write remains a transport failure; this protocol does not replay it.
                    state.sequence.store(seq, Ordering::Relaxed);
                }
                Some((id, result)) = pending.next(), if !pending.is_empty() => {
                    send(&mut socket, Outgoing::Result { id, result: result.into() }).await?;
                }
                message = socket.next() => {
                    let Some(message) = message else { return Err(failed()); };
                    match message.map_err(|_| failed())? {
                        Message::Close(_) => return Err(failed()),
                        Message::Ping(_) | Message::Pong(_) => {},
                        Message::Text(text) => {
                            let Incoming::Cmd { id, op, args } = protocol::parse(&text)? else { return Err(protocol::invalid()); };
                            // Invalid/duplicate IDs terminate the connection: emitting another
                            // result with that ID could resolve the wrong pending request.
                            ids.accept(id)?;
                            let command = protocol::command(&op, args);
                            let command = if pending.len() >= 64 { Err(CoreError::new(ErrorCode::Busy, "control request limit")) } else { command };
                            match command {
                                Ok(op) => { let network = network.clone(); pending.push(Box::pin(async move { (id, network.execute(op).await) })); }
                                Err(error) => send(&mut socket, Outgoing::Result { id, result: Err(error).into() }).await?,
                            }
                        }
                        _ => return Err(protocol::invalid()),
                    }
                }
            }
        }
        }
        => result,
    };
    // Dropping pending futures leaves the normal network actor alive. The control
    // server observes socket closure and fails its outstanding RPCs.
    drop(pending);
    drop(events);
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.close(None)).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn overflowing_sink_signals_without_reentering_bus_or_advancing_sequence() {
        let bus = EventBus::new();
        let (sender, _receiver) = mpsc::channel(256);
        let overflow = CancellationToken::new();
        bus.subscribe(Arc::new(Sink {
            sender,
            overflow: overflow.clone(),
        }));
        for _ in 0..256 {
            bus.publish(CoreEvent::ConnectionState(
                crate::ConnectionSnapshot::OFFLINE,
            ));
        }
        assert!(!overflow.is_cancelled());
        bus.publish(CoreEvent::ConnectionState(
            crate::ConnectionSnapshot::OFFLINE,
        ));
        assert!(overflow.is_cancelled());
        assert_eq!(bus.subscriber_count(), 0);
    }
}
