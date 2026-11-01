mod account;
mod message;
pub(crate) mod records;
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
    let mut text = zeroize::Zeroizing::new(serde_json::to_string(&record).map_err(|_| failed())?);
    if text.len() > 64 * 1024 {
        let Outgoing::Result { id, .. } = record else { return Err(failed()); };
        text = zeroize::Zeroizing::new(serde_json::to_string(&Outgoing::Result {
            id, result: protocol::Outcome::account(Err(CoreError::new(
                ErrorCode::MessageTooLarge, "Control result exceeds transport budget"))),
        }).map_err(|_| failed())?);
    }
    tokio::time::timeout(
        Duration::from_secs(5),
        socket.send(Message::Text(text.as_str().into())),
    )
    .await
    .map_err(|_| failed())?
    .map_err(|_| failed())
}
// Keep independently owned lifecycle handles explicit; avoid a wrapper used only here.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn start(
    handle: &tokio::runtime::Handle,
    state: Arc<HookState>,
    network: NetworkHandle,
    account: Arc<std::sync::Mutex<crate::runtime::AccountSlot>>,
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
            result = run(&state, network, account, &bus, &instance, &control_url, &name, &mut ready) => result,
        };
        if let Err(error) = result {
            if let Some(ready) = ready.take() { let _ = ready.send(Err(error.clone())); }
            bus.publish(CoreEvent::Error { code: error.code, detail: error.detail });
        }
    });
    received.await.map_err(|_| failed())?
}
// Mirrors start with borrowed connection metadata and the readiness sender.
#[allow(clippy::too_many_arguments)]
async fn run(
    state: &HookState,
    network: NetworkHandle,
    account: Arc<std::sync::Mutex<crate::runtime::AccountSlot>>,
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
    let Incoming::Config { relay, records, controlled_time } = protocol::parse(&text)? else {
        return Err(protocol::invalid());
    };
    let relay = relay
        .map(|value| {
            protocol::validate_relay(&value)?;
            value.parse().map_err(|_| protocol::invalid())
        })
        .transpose()?;
    // Account open uses this same mutex. Install first and retain configuration
    // even if native opening subsequently fails. No owned-task/core Arc cycle.
    if controlled_time && records.is_none() { return Err(protocol::invalid()); }
    if let Some(url) = records {
        self::records::validate(&url)?;
        let mut slot = account.lock().expect("account slot lock poisoned");
        if slot.handle.is_some() || slot.records.is_some() { return Err(protocol::invalid()); }
        if controlled_time {
            use crate::account::clock::{Clock, FakeClock, SystemClock};
            slot.controlled_clock = Some(Arc::new(FakeClock::new(SystemClock::default().unix_seconds(), 0)));
        }
        slot.records = Some(url);
        slot.configuring = true;
    }
    let configured = network.configure_local(relay).await;
    if configured.is_ok() { account.lock().expect("account slot lock poisoned").configuring = false; }
    configured?;
    let (sender, mut events) = mpsc::channel(256);
    let overflow = CancellationToken::new();
    bus.subscribe(Arc::new(Sink {
        sender,
        overflow: overflow.clone(),
    }));
    let _ = ready.take().unwrap().send(Ok(()));
    let mut ids = protocol::RequestIds::default();
    let ack_loss = Arc::new(message::AckLossControl::default());
    let mut pending: FuturesUnordered<
        futures_util::future::BoxFuture<'static, (u64, protocol::Outcome)>,
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
                    send(&mut socket, Outgoing::Result { id, result }).await?;
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
                            let mut args = account::SecretJson(args);
                            ids.accept(id)?;
                            if pending.len() >= 64 {
                                send(&mut socket, Outgoing::Result { id, result: protocol::Outcome::account(Err(CoreError::new(ErrorCode::Busy, "control request limit"))) }).await?;
                            } else if op == "clock_advance" {
                                let result = message::advance(&account.lock().expect("account slot lock poisoned"), args);
                                send(&mut socket, Outgoing::Result { id, result: protocol::Outcome::account(result) }).await?;
                            } else if op.starts_with("message_local_test_") {
                                let handle = account.lock().expect("account slot lock poisoned").handle.clone();
                                let control = ack_loss.clone();
                                pending.push(Box::pin(async move { (id, protocol::Outcome::account(message::execute_test(handle, control, &op, args).await)) }));
                            } else if op.starts_with("message_local_") {
                                let handle = account.lock().expect("account slot lock poisoned").handle.clone();
                                pending.push(Box::pin(async move { (id, protocol::Outcome::account(message::execute(handle, &op, args).await)) }));
                            } else if op.starts_with("account_local_") {
                                let handle = account.lock().expect("account slot lock poisoned").handle.clone();
                                let network = network.clone();
                                pending.push(Box::pin(async move { (id, protocol::Outcome::account(account::execute(handle, network, &op, args).await)) }));
                            } else {
                                match protocol::command(&op, std::mem::take(&mut args.0)) {
                                    Ok(op) => { let network = network.clone(); pending.push(Box::pin(async move { (id, network.execute(op).await.into()) })); }
                                    Err(error) => send(&mut socket, Outgoing::Result { id, result: Err(error).into() }).await?,
                                }
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
    ack_loss.release().await;
    drop(events);
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.close(None)).await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn oversized_private_result_returns_fixed_error_with_original_id() {
        use serde_json::json;
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let connect=tokio_tungstenite::connect_async(format!("ws://{}",listener.local_addr().unwrap()));
        let accept=async { tokio_tungstenite::accept_async(listener.accept().await.unwrap().0).await.unwrap() };
        let (client,mut server)=tokio::join!(connect,accept);
        let mut client=client.unwrap().0;
        for (id,value) in [(7,json!({"body":"\u{1}".repeat(16_384)})),
            (8,json!({"items":vec![json!({"body":"x".repeat(16_384)});50]}))] {
            let outgoing=Outgoing::Result { id,result:protocol::Outcome::account(Ok(value)) };
            let (sent,reply)=tokio::join!(send(&mut client,outgoing),server.next());
            sent.unwrap();
            let Message::Text(reply)=reply.unwrap().unwrap() else {panic!()};
            assert!(reply.len()<=64*1024);
            let parsed:serde_json::Value=serde_json::from_str(&reply).unwrap();
            assert_eq!(parsed["id"],id);
            assert_eq!(parsed["error"]["code"],"MessageTooLarge");
            assert!(parsed.get("value").is_none());
        }
    }
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
