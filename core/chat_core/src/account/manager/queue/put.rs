//! Putting texts into peers' queues (spec 8):
//!
//! * A text's attempt that went out or tried to (the list its push decides from) names the
//!   devices of the peer it did not reach. Each of them with a live queue on this device's
//!   server (`queue_peer`) that does not hold the text yet (`queue_delivery`) gets it, in one
//!   `SEND` for all of them: the `Commit`s and `Welcome`s it still lacks, then the text, each
//!   the frame a direct attempt writes (`QueueEntry`, naming this device). It is read before the
//!   attempt's outcome is stored, since an acknowledged text's row goes with it. A mailbox does
//!   the same for the texts my other devices handed off to it.
//! * Every frame taken: `queue_delivery` holds the text for that device until the server drops
//!   it (`ENTRY_TTL_MS`). The outbox keeps trying directly as before, and the first of its
//!   attempts after that time puts the text in again, as the outbox has it then (so a
//!   re-encrypted text goes in anew). Once delivered, the text is gone and so is the `SEND`.
//! * `gone`: that queue is dead until the device announces another. `full`, `limited`, `bad` or
//!   no answer: nothing is noted, so the text's next attempt tries again (the outbox back-off).
//!   One `SEND` per text at a time.
//! * Rows whose text is gone or that expired are swept hourly, a bounded batch at a time.
//! * No address, or the mode "never": no `SEND`.

use super::*;

/// The JSON of one `SEND` item around its base64 `ct` (the queue id, signature, time, keys),
/// with room to spare; and around all the items.
const SEND_ITEM_JSON: usize = 256;
const SEND_BODY_JSON: usize = 64;

/// Putting texts into peers' queues (spec 8).
#[derive(Default)]
pub(super) struct Put {
    /// Texts whose `SEND` is on its way: no second one until it answers.
    sending: BTreeSet<MessageKey>,
    /// The scheduler time `queue_delivery` is swept next.
    sweep_at: u64,
}

/// The items of one `SEND` (spec 8.1, 9.2), with the device and queue of each: every frame of
/// each target in order, as an entry naming this device (`me`), signed with that queue's sender
/// key at `ts`. A target whose items would take the request past `MAX_ITEMS` or `MAX_BODY` is
/// left out whole (its frames must stay in order), for the text's next attempt.
fn send_items(
    send: ToSend,
    me: (PeerId, InstallationId),
    ts: u64,
) -> (Vec<(InstallationId, QueueId)>, Vec<SendItem>) {
    let (mut ids, mut items) = (Vec::new(), Vec::new());
    let mut body = SEND_BODY_JSON;
    for target in send.targets {
        let cts: Vec<Vec<u8>> = target
            .frames
            .into_iter()
            .map(|frame| {
                QueueEntry {
                    sender_peer: me.0,
                    sender_install: me.1,
                    frame,
                }
                .encode()
            })
            .collect();
        let size: usize = cts
            .iter()
            .map(|ct| ct.len().div_ceil(3) * 4 + SEND_ITEM_JSON)
            .sum();
        if items.len() + cts.len() > MAX_ITEMS || body + size > MAX_BODY {
            continue;
        }
        body += size;
        for ct in cts {
            let signed = Signed::Send { ct: &ct };
            let sig = sign(
                &target.sender_secret,
                &signing_bytes(&signed, &target.queue_id, ts),
            );
            ids.push((target.installation, target.queue_id));
            items.push(SendItem {
                queue_id: b64(&target.queue_id),
                ct: b64(&ct),
                ts,
                sig: b64(&sig),
            });
        }
    }
    (ids, items)
}

/// Devices with the queue each was sent to.
type Targets = Vec<(InstallationId, QueueId)>;

/// What a `SEND` came to for each device: `put`, every frame of it taken (`ok`); `gone`, its
/// queue unknown at the server. Neither: `full`, `limited`, `bad`, or no answer.
fn sent_outcome(items: &[(InstallationId, QueueId)], answers: &[ItemResult]) -> (Targets, Targets) {
    let (mut put, mut gone) = (Vec::new(), Vec::new());
    let mut targets: Vec<(InstallationId, QueueId)> = items.to_vec();
    targets.dedup();
    for target in targets {
        let results: Vec<Option<&ItemResult>> = items
            .iter()
            .enumerate()
            .filter(|(_, item)| **item == target)
            .map(|(at, _)| answers.get(at))
            .collect();
        if results.iter().any(|r| matches!(r, Some(ItemResult::Gone))) {
            gone.push(target);
        } else if results.iter().all(|r| matches!(r, Some(ItemResult::Ok))) {
            put.push(target);
        }
    }
    (put, gone)
}

impl Manager {
    /// Whether a queue server address is configured: texts are put into peers' queues.
    pub(in crate::account::manager) fn queue_on(&self) -> bool {
        self.messages.queue.server.is_some()
    }

    /// Spec 8.1: puts text `key` into the queues of the devices of `peer` its attempt did not
    /// reach (all but `reached`), one `SEND` in a spawned task. Call it before the attempt's
    /// outcome is stored. Nothing without an address, or while a `SEND` of it is on its way.
    pub(in crate::account::manager) async fn queue_put(
        &mut self,
        key: MessageKey,
        peer: PeerId,
        reached: &[InstallationId],
        request: Request,
    ) {
        let Some(server) = self.messages.queue.server.clone() else {
            return;
        };
        let Ok(local) = self.local() else {
            return;
        };
        let me = (local.keys.peer_id(), local.device.installation_id());
        if self.messages.queue.put.sending.contains(&key) {
            return;
        }
        let devices: Vec<InstallationId> = self
            .current_installations(peer)
            .into_iter()
            .filter(|i| !reached.contains(i))
            .collect();
        let wall = self.services.clock.wall_ms();
        let op = QueueDbOp::ToSend {
            unreached: Unreached { key, devices },
            server_url: server.url.to_string(),
            wall,
        };
        let Ok(QueueDbReply::ToSend(Some(send))) = self.queue_db(op, request).await else {
            return;
        };
        let (items, sends) = send_items(send, me, wall);
        if sends.is_empty() {
            return;
        }
        self.messages.queue.put.sending.insert(key);
        let mailbox = self.auth.send.clone();
        let ticket = request.ticket;
        tokio::spawn(async move {
            let answers = match tokio::time::timeout(REQUEST_TIMEOUT, server.client.send(sends))
                .await
            {
                Ok(Ok(answers)) if answers.len() == items.len() => answers,
                _ => Vec::new(),
            };
            let _ = mailbox
                .send(Input::Queue(QueueEvent::Sent {
                    ticket,
                    key,
                    items,
                    answers,
                    ts: wall,
                }))
                .await;
        });
    }

    /// A text's `SEND` came back: the queues that took it hold it until the server drops it,
    /// and a queue the server does not know is dead. Anything else is left to the text's next
    /// attempt.
    pub(super) async fn queue_sent(
        &mut self,
        ticket: u64,
        key: MessageKey,
        items: Vec<(InstallationId, QueueId)>,
        answers: Vec<ItemResult>,
        ts: u64,
    ) {
        self.messages.queue.put.sending.remove(&key);
        let Some(request) = self.ready().ok().filter(|r| r.ticket == ticket) else {
            return;
        };
        let (put, gone) = sent_outcome(&items, &answers);
        if put.is_empty() && gone.is_empty() {
            return;
        }
        let op = QueueDbOp::Sent {
            key,
            put,
            gone,
            expires_at: ts.saturating_add(ENTRY_TTL_MS),
        };
        // Not stored: the text is put in again at its next attempt.
        let _ = self.queue_db(op, request).await;
    }

    /// Drops `queue_delivery` rows that expired or whose text is gone, hourly (at once again
    /// while a batch came back full).
    pub(super) async fn queue_sweep(&mut self, request: Request) {
        let now = self.schedule_now();
        if now < self.messages.queue.put.sweep_at {
            return;
        }
        let op = QueueDbOp::Sweep {
            wall: self.services.clock.wall_ms(),
        };
        let more = matches!(
            self.queue_db(op, request).await,
            Ok(QueueDbReply::Swept(n)) if n >= SWEEP_MAX
        );
        self.messages.queue.put.sweep_at = if more { now } else { now.saturating_add(CHECK_MS) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 1_800_000_000_000;

    fn conv(n: u8) -> ConversationId {
        ConversationId([n; 32])
    }

    fn target(n: u8, frames: Vec<Vec<u8>>) -> crate::message::queue_store::Target {
        crate::message::queue_store::Target {
            installation: InstallationId([n; 32]),
            queue_id: [n; 32],
            sender_secret: Zeroizing::new([n; 32]),
            frames,
        }
    }

    fn to_send(targets: Vec<crate::message::queue_store::Target>) -> ToSend {
        ToSend {
            key: MessageKey {
                conversation: conv(1),
                sender: PeerId([9; 32]),
                installation: InstallationId([8; 32]),
                id: crate::message::types::MessageId([7; 16]),
            },
            targets,
        }
    }

    #[test]
    fn send_items_keep_each_devices_frames_in_order_signed_by_its_queue() {
        let me = (PeerId([9; 32]), InstallationId([8; 32]));
        let send = to_send(vec![
            target(1, vec![b"commit".to_vec(), b"text".to_vec()]),
            target(2, vec![b"text".to_vec()]),
        ]);
        let (ids, items) = send_items(send, me, T);
        let one = (InstallationId([1; 32]), [1; 32]);
        let two = (InstallationId([2; 32]), [2; 32]);
        assert_eq!(ids, [one, one, two]);
        let frames: Vec<Vec<u8>> = items
            .iter()
            .zip(&ids)
            .map(|(item, (_, queue))| {
                assert_eq!(item.queue_id, b64(queue));
                assert_eq!(item.ts, T);
                let ct = queue_wire::decode_ct(&item.ct).unwrap();
                let sig: [u8; 64] = queue_wire::decode_array(&item.sig).unwrap();
                assert!(queue_wire::verify(
                    &public_key(queue),
                    &signing_bytes(&Signed::Send { ct: &ct }, queue, T),
                    &sig
                ));
                let entry = QueueEntry::decode(&ct).unwrap();
                assert_eq!((entry.sender_peer, entry.sender_install), me);
                entry.frame
            })
            .collect();
        assert_eq!(frames, [b"commit".to_vec(), b"text".to_vec(), b"text".to_vec()]);
    }

    #[test]
    fn send_items_leave_out_a_device_whose_frames_do_not_fit() {
        let me = (PeerId([9; 32]), InstallationId([8; 32]));
        // Nine frames at the frame limit fill most of one request; a second such device does
        // not fit after it, a small one does.
        let big = vec![vec![0xAB; 61_440]; 9];
        let send = to_send(vec![
            target(1, big.clone()),
            target(2, big),
            target(3, vec![b"text".to_vec()]),
        ]);
        let (ids, items) = send_items(send, me, T);
        let devices: Vec<u8> = ids.iter().map(|(i, _)| i.0[0]).collect();
        assert_eq!(devices, [1, 1, 1, 1, 1, 1, 1, 1, 1, 3]);
        let body = serde_json::to_vec(&queue_wire::QueueRequest { items }).unwrap();
        assert!(body.len() <= MAX_BODY, "{}", body.len());
    }

    #[test]
    fn sent_outcome_puts_a_device_only_when_every_frame_was_taken() {
        let at = |n: u8| (InstallationId([n; 32]), [n; 32]);
        let items = [at(1), at(1), at(2), at(3), at(4), at(5), at(5)];
        let answers = [
            ItemResult::Ok,
            ItemResult::Ok,
            ItemResult::Gone,
            ItemResult::Full,
            ItemResult::Limited,
            ItemResult::Bad,
            ItemResult::Ok,
        ];
        assert_eq!(sent_outcome(&items, &answers), (vec![at(1)], vec![at(2)]));
        // No answer (the request failed): neither.
        assert_eq!(sent_outcome(&items, &[]), (vec![], vec![]));
    }
}
