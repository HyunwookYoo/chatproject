use super::{
    PeerPath,
    frame::{self, MAGIC},
    manager::{NetworkReply, Note},
};
use crate::{CoreError, ErrorCode};
use futures_util::StreamExt;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use std::time::{Duration, Instant};
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionTag {
    pub epoch: u64,
    pub generation: u64,
    pub peer_id: String,
}
impl SessionTag {
    pub fn is_current(&self, current: &Self) -> bool {
        self == current
    }
}

pub(crate) type Reply = oneshot::Sender<Result<NetworkReply, CoreError>>;
pub(crate) struct Write {
    pub text: String,
    pub reply: Reply,
}
pub(crate) struct Prepared {
    pub connection: Connection,
    pub send: SendStream,
    pub recv: RecvStream,
    pub ready_at: Instant,
}

pub(crate) async fn prepare_session(
    connection: Connection,
) -> Result<(SendStream, RecvStream), CoreError> {
    let mut send = connection
        .open_uni()
        .await
        .map_err(|_| frame::protocol_error())?;
    send.write_all(MAGIC)
        .await
        .map_err(|_| frame::protocol_error())?;
    let mut recv = connection
        .accept_uni()
        .await
        .map_err(|_| frame::protocol_error())?;
    let mut magic = [0; 4];
    recv.read_exact(&mut magic)
        .await
        .map_err(|_| frame::protocol_error())?;
    if &magic != MAGIC {
        connection.close(1u32.into(), b"invalid stream");
        return Err(frame::protocol_error());
    }
    Ok((send, recv))
}

pub(crate) fn selected_path(connection: &Connection) -> PeerPath {
    connection
        .paths()
        .iter()
        .find(|p| p.is_selected())
        .map(|p| match p.remote_addr() {
            iroh::TransportAddr::Ip(_) => PeerPath::Direct,
            iroh::TransportAddr::Relay(_) => PeerPath::Relay,
            _ => PeerPath::Unknown,
        })
        .unwrap_or(PeerPath::Unknown)
}

// Cancellation must also interrupt a full note queue; stop joins the senders.
pub(crate) async fn notify(notes: &mpsc::Sender<Note>, cancel: &CancellationToken, note: Note) {
    tokio::select! { biased; _ = cancel.cancelled() => note.discard().await, permit = notes.reserve() => match permit { Ok(permit) => permit.send(note), Err(_) => note.discard().await } }
}

pub(crate) struct Session {
    pub tag: SessionTag,
    pub connection: Connection,
    pub outgoing: bool,
    pub cancel: CancellationToken,
    pub writes: mpsc::Sender<Write>,
    pub started: Option<Instant>,
}
impl Session {
    pub fn close(&self) {
        self.cancel.cancel();
        self.connection.close(0u32.into(), b"session ended");
    }
}

pub(crate) fn spawn_session(
    tasks: &mut JoinSet<()>,
    prepared: Prepared,
    tag: SessionTag,
    outgoing: bool,
    started: Option<Instant>,
    root: &CancellationToken,
    notes: &mpsc::Sender<Note>,
) -> Session {
    let Prepared {
        connection,
        send,
        mut recv,
        ..
    } = prepared;
    let cancel = root.child_token();
    let (writes, input) = mpsc::channel::<Write>(16);
    tasks.spawn(run_writer(
        send,
        input,
        cancel.clone(),
        root.clone(),
        notes.clone(),
        tag.clone(),
    ));
    let reader_cancel = cancel.clone();
    let reader_notes = notes.clone();
    let reader_tag = tag.clone();
    tasks.spawn(async move {
        loop {
            let result = tokio::select! { biased; _ = reader_cancel.cancelled() => break, result = frame::read_text(&mut recv) => result };
            match result {
                Ok(Some(text)) => notify(&reader_notes, &reader_cancel, Note::Text { tag: reader_tag.clone(), text }).await,
                result => { notify(&reader_notes, &reader_cancel, Note::Ended { tag: reader_tag, error: result.err() }).await; break; }
            }
        }
    });
    let guard_cancel = cancel.clone();
    let guard_notes = notes.clone();
    let guard_tag = tag.clone();
    let guard_connection = connection.clone();
    tasks.spawn(async move {
        tokio::select! {
            biased;
            _ = guard_cancel.cancelled() => {},
            _ = guard_connection.accept_uni() => { guard_connection.close(1u32.into(), b"extra stream"); notify(&guard_notes, &guard_cancel, Note::Ended { tag: guard_tag, error: Some(frame::protocol_error()) }).await; },
            _ = guard_connection.accept_bi() => { guard_connection.close(1u32.into(), b"extra stream"); notify(&guard_notes, &guard_cancel, Note::Ended { tag: guard_tag, error: Some(frame::protocol_error()) }).await; },
        }
    });
    let path_connection = connection.clone();
    let path_tag = tag.clone();
    let path_cancel = cancel.clone();
    let path_notes = notes.clone();
    tasks.spawn(async move {
        let events = path_connection.path_events();
        observe_paths(
            &path_connection,
            events,
            &path_notes,
            &path_cancel,
            &path_tag,
        )
        .await;
    });
    Session {
        tag,
        connection,
        outgoing,
        cancel,
        writes,
        started,
    }
}

async fn observe_paths(
    connection: &Connection,
    mut events: impl futures_util::Stream + Unpin,
    notes: &mpsc::Sender<Note>,
    cancel: &CancellationToken,
    tag: &SessionTag,
) {
    loop {
        // Notifications, including Lagged, are hints: always repoll current selection.
        let path = selected_path(connection);
        notify(
            notes,
            cancel,
            Note::Path {
                tag: tag.clone(),
                path,
                observed: Instant::now(),
            },
        )
        .await;
        tokio::select! { biased; _ = cancel.cancelled() => break, event = events.next() => if event.is_none() { break; } }
    }
}
async fn run_writer<W: tokio::io::AsyncWrite + Unpin>(
    mut send: W,
    mut input: mpsc::Receiver<Write>,
    cancel: CancellationToken,
    root: CancellationToken,
    notes: mpsc::Sender<Note>,
    tag: SessionTag,
) {
    loop {
        let request = tokio::select! { biased; _ = cancel.cancelled() => break, next = input.recv() => match next { Some(next) => next, None => break } };
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(cancel_error(&root)),
            result = tokio::time::timeout(Duration::from_secs(5), frame::write_text(&mut send, &request.text)) => result.unwrap_or_else(|_| Err(CoreError::new(ErrorCode::NetworkTimeout, "write timed out"))),
        };
        let error = result.as_ref().err().cloned();
        let _ = request.reply.send(result.map(|_| NetworkReply::Done));
        if let Some(error) = error {
            notify(
                &notes,
                &cancel,
                Note::Ended {
                    tag: tag.clone(),
                    error: Some(error),
                },
            )
            .await;
            break;
        }
    }
    input.close();
    while let Some(request) = input.recv().await {
        let _ = request.reply.send(Err(cancel_error(&root)));
    }
}
fn cancel_error(root: &CancellationToken) -> CoreError {
    CoreError::new(
        if root.is_cancelled() {
            ErrorCode::NetworkNotStarted
        } else {
            ErrorCode::PeerNotConnected
        },
        "write cancelled",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn every_notification_repolls_current_selected_path() {
        use super::super::endpoint::{
            NetworkProfile, bind_endpoint, endpoint_ticket, parse_target,
        };
        let profile = NetworkProfile::Local { relay: None };
        let a = bind_endpoint(&profile, iroh::SecretKey::generate())
            .await
            .unwrap();
        let b = bind_endpoint(&profile, iroh::SecretKey::generate())
            .await
            .unwrap();
        let target = parse_target(&endpoint_ticket(&b, &profile)).unwrap();
        let (outgoing, incoming) = tokio::join!(a.connect(target, frame::ALPN), async {
            b.accept().await.unwrap().await.unwrap()
        });
        let outgoing = outgoing.unwrap();
        let (notes, mut received) = mpsc::channel(64);
        let tag = SessionTag {
            epoch: 1,
            generation: 1,
            peer_id: b.id().to_string(),
        };
        observe_paths(
            &outgoing,
            futures_util::stream::iter([()]),
            &notes,
            &CancellationToken::new(),
            &tag,
        )
        .await;
        for _ in 0..2 {
            assert!(matches!(
                received.try_recv().unwrap(),
                Note::Path {
                    path: PeerPath::Direct,
                    ..
                }
            ));
        }
        assert!(received.try_recv().is_err());
        let (left, right) = tokio::join!(
            prepare_session(outgoing.clone()),
            prepare_session(incoming.clone())
        );
        let (send, recv) = left.unwrap();
        let _remote_streams = right.unwrap();
        let mut tasks = JoinSet::new();
        let root = CancellationToken::new();
        let session = spawn_session(
            &mut tasks,
            Prepared {
                connection: outgoing.clone(),
                send,
                recv,
                ready_at: Instant::now(),
            },
            tag,
            true,
            None,
            &root,
            &notes,
        );
        assert_eq!(session.writes.capacity(), 16);
        let mut replies = Vec::new();
        for _ in 0..16 {
            let (reply, rx) = oneshot::channel();
            session
                .writes
                .try_send(Write {
                    text: "x".into(),
                    reply,
                })
                .ok()
                .unwrap();
            replies.push(rx);
        }
        let (reply, _) = oneshot::channel();
        assert!(matches!(
            session.writes.try_send(Write {
                text: "x".into(),
                reply
            }),
            Err(mpsc::error::TrySendError::Full(_))
        ));
        root.cancel();
        session.close();
        while tasks.join_next().await.is_some() {}
        for reply in replies {
            assert_eq!(
                reply.await.unwrap().unwrap_err().code,
                ErrorCode::NetworkNotStarted
            );
        }
        outgoing.close(0u32.into(), b"done");
        incoming.close(0u32.into(), b"done");
        a.close().await;
        b.close().await;
    }
    #[tokio::test(start_paused = true)]
    async fn writer_queue_cap_timeout_and_cancellation_resolve_every_reply() {
        for stop in [false, true] {
            let root = CancellationToken::new();
            let cancel = root.child_token();
            let (tx, input) = mpsc::channel(16);
            let (notes, mut received) = mpsc::channel(64);
            let mut replies = Vec::new();
            for _ in 0..16 {
                let (reply, rx) = oneshot::channel();
                tx.try_send(Write {
                    text: "가".into(),
                    reply,
                })
                .ok()
                .unwrap();
                replies.push(rx);
            }
            let (reply, _) = oneshot::channel();
            assert!(matches!(
                tx.try_send(Write {
                    text: "x".into(),
                    reply
                }),
                Err(mpsc::error::TrySendError::Full(_))
            ));
            let (writer, _reader) = tokio::io::duplex(1);
            let tag = SessionTag {
                epoch: 1,
                generation: 1,
                peer_id: "peer".into(),
            };
            let task = tokio::spawn(run_writer(
                writer,
                input,
                cancel.clone(),
                root.clone(),
                notes,
                tag,
            ));
            tokio::task::yield_now().await;
            let first_code = if stop {
                root.cancel();
                ErrorCode::NetworkNotStarted
            } else {
                tokio::time::advance(Duration::from_secs(5)).await;
                ErrorCode::NetworkTimeout
            };
            assert_eq!(
                replies.remove(0).await.unwrap().unwrap_err().code,
                first_code
            );
            task.await.unwrap();
            for reply in replies {
                assert_eq!(
                    reply.await.unwrap().unwrap_err().code,
                    if stop {
                        ErrorCode::NetworkNotStarted
                    } else {
                        ErrorCode::PeerNotConnected
                    }
                );
            }
            if !stop {
                assert!(matches!(received.recv().await, Some(Note::Ended { .. })));
            }
            assert!(tx.is_closed());
        }
    }
    #[tokio::test(start_paused = true)]
    async fn disconnect_cancels_a_blocked_write_as_peer_not_connected() {
        let root = CancellationToken::new();
        let cancel = root.child_token();
        let (tx, input) = mpsc::channel(16);
        let (notes, _) = mpsc::channel(64);
        let (reply, rx) = oneshot::channel();
        tx.send(Write {
            text: "abc".into(),
            reply,
        })
        .await
        .ok()
        .unwrap();
        let (writer, _reader) = tokio::io::duplex(1);
        let task = tokio::spawn(run_writer(
            writer,
            input,
            cancel.clone(),
            root,
            notes,
            SessionTag {
                epoch: 1,
                generation: 1,
                peer_id: "peer".into(),
            },
        ));
        tokio::task::yield_now().await;
        cancel.cancel();
        assert_eq!(
            rx.await.unwrap().unwrap_err().code,
            ErrorCode::PeerNotConnected
        );
        task.await.unwrap();
    }
}
