//! Putting frames into the queues of devices no direct send reaches (spec 8):
//!
//! * Any frame a direct send would write to a device goes into that device's queue while no
//!   direct send reaches it: texts, receipts, `Commit`s, `Welcome`s, `PushHandle`s and
//!   `QueueRef`s, each the frame the direct send keeps (`queue_store`), as an entry naming the
//!   device that sealed it (`QueueEntry`: this device, or a handed-off text's origin, so the
//!   origin's copy and its mailbox's are the same entry and the server keeps one). A device
//!   with a live queue on this device's server (`queue_peer`) gets them, a peer's device or
//!   another of mine (spec 7.1: my other devices send in the conversation too).
//! * Two triggers, one path (`queue_send`):
//!   * A text's or receipt's attempt that went out or tried to names the devices it did not
//!     reach (`queue_put`): for a text the peer's current devices, and my other current devices
//!     I have no session with (one I have gets my texts from my log, M6, or a copy, 5.3); for
//!     a receipt the device that sent the message. Read before the attempt's outcome is stored,
//!     since an acknowledged text's row goes with it. The frames those devices still lack in
//!     the conversation go ahead of it. A mailbox does the same for the texts handed off to it.
//!   * The standing pass (`queue_standing`), every `STANDING_EVERY_MS` and at once when
//!     routing frames were sealed: every device without a session gets the `Commit`s,
//!     `Welcome`s, `PushHandle`s and `QueueRef`s still queued for it, in every conversation, so
//!     those go in with no text after them (a rotated queue reaches a peer that is off).
//! * Each trigger is one `SEND` request for every device and frame. A device's frames stay
//!   together and in order; one whose frames would take the request past `MAX_ITEMS` or
//!   `MAX_BODY` waits for the next trigger (the standing pass comes again at the next poll
//!   when its request stored something, else at its period, so a server that takes nothing
//!   gets one request per `STANDING_EVERY_MS`).
//!   Each request starts one device further on, so a device whose frames fill it never keeps
//!   the others out.
//! * Each frame the server took: `queue_delivery` holds it for that device's queue until the
//!   server drops it (`ENTRY_TTL_MS`); the next trigger after that puts it in again if it still
//!   waits (a re-encrypted text is a new frame). `gone`: that queue is dead until the device
//!   announces another. `full`, `limited`, `bad` or no answer: nothing is noted, so the next
//!   trigger tries again (the outbox back-off, or the standing pass).
//! * One `SEND` per text or receipt at a time, and one standing pass. A frame a `SEND` on its
//!   way carries goes in no other request for that device until it answers (two texts in a
//!   burst both lacking a `Commit` carry it once), and then goes again only if it was not taken.
//! * Rows that expired go in the transaction that notes the next `SEND`, so with no address
//!   nothing ever writes them.
//! * No address, or the mode "never": no `SEND`.

use super::*;
use super::super::messages::Reach;
use crate::message::queue_store::{Queued, frame_hash};

/// The JSON of one `SEND` item around its base64 `ct` (the queue id, signature, time, keys),
/// with room to spare; and around all the items.
const SEND_ITEM_JSON: usize = 256;
const SEND_BODY_JSON: usize = 64;
/// How often the standing pass runs while nothing asks for it sooner: the outbox's longest gap
/// between two attempts of a text.
const STANDING_EVERY_MS: u64 = 300_000;

/// Putting frames into other devices' queues (spec 8).
#[derive(Default)]
pub(super) struct Put {
    /// What a `SEND` is on its way for: a text's or receipt's key, `None` the standing pass. No
    /// second one for it until it answers.
    sending: BTreeSet<Option<MessageKey>>,
    /// The scheduler time the standing pass runs next.
    standing_at: u64,
    /// The last standing request left a device out: the pass comes again at once if that
    /// request stored something.
    standing_rest: bool,
    /// `SEND` requests made: each starts its devices one further on.
    turn: usize,
    /// The frames of the `SEND`s on their way, by device: no other request carries them.
    flying: BTreeSet<(InstallationId, [u8; 32])>,
}

/// The items of one `SEND` (spec 8.1, 9.2), with what each puts where: every frame of each
/// target in order but those a request on its way carries to it (`flying`), as an entry of my
/// account (`me`) naming its sealer, signed with that queue's sender key at `ts`. Targets go
/// from the one at `start` (in turn) round to the one before it; one whose items would take the
/// request past `MAX_ITEMS` or `MAX_BODY` is left out whole (its frames must stay in order).
/// `false`: one was left out.
fn send_items(
    send: ToSend,
    me: PeerId,
    ts: u64,
    start: usize,
    flying: &BTreeSet<(InstallationId, [u8; 32])>,
) -> (Vec<Queued>, Vec<SendItem>, bool) {
    let mut targets = send.targets;
    let first = start % targets.len().max(1);
    targets.rotate_left(first);
    let (mut queued, mut items, mut all) = (Vec::new(), Vec::new(), true);
    let mut body = SEND_BODY_JSON;
    for target in targets {
        let cts: Vec<([u8; 32], Vec<u8>)> = target
            .frames
            .into_iter()
            .map(|(sealer, frame)| (frame_hash(&frame), sealer, frame))
            .filter(|(hash, ..)| !flying.contains(&(target.installation, *hash)))
            .map(|(hash, sealer, frame)| {
                let entry = QueueEntry {
                    sender_peer: me,
                    sender_install: sealer,
                    frame,
                };
                (hash, entry.encode())
            })
            .collect();
        let size: usize = cts
            .iter()
            .map(|(_, ct)| ct.len().div_ceil(3) * 4 + SEND_ITEM_JSON)
            .sum();
        if items.len() + cts.len() > MAX_ITEMS || body + size > MAX_BODY {
            all = false;
            continue;
        }
        body += size;
        for (frame_hash, ct) in cts {
            let signed = Signed::Send { ct: &ct };
            let sig = sign(
                &target.sender_secret,
                &signing_bytes(&signed, &target.queue_id, ts),
            );
            queued.push(Queued {
                conversation: target.conversation,
                installation: target.installation,
                queue_id: target.queue_id,
                frame_hash,
            });
            items.push(SendItem {
                queue_id: b64(&target.queue_id),
                ct: b64(&ct),
                ts,
                sig: b64(&sig),
            });
        }
    }
    (queued, items, all)
}

/// What a `SEND` came to: `taken`, the frames answered `ok`; `gone`, each queue answered
/// `gone`, once. `full`, `limited`, `bad`, or no answer (a failed request): neither.
fn sent_outcome(items: &[Queued], answers: &[ItemResult]) -> (Vec<Queued>, Vec<Queued>) {
    let (mut taken, mut gone) = (Vec::new(), Vec::<Queued>::new());
    for (item, answer) in items.iter().zip(answers) {
        match answer {
            ItemResult::Ok => taken.push(*item),
            ItemResult::Gone
                if !gone
                    .iter()
                    .any(|g| (g.installation, g.queue_id) == (item.installation, item.queue_id)) =>
            {
                gone.push(*item)
            }
            _ => {}
        }
    }
    (taken, gone)
}

impl Manager {
    /// Whether a queue server address is configured: frames are put into other devices' queues.
    pub(in crate::account::manager) fn queue_on(&self) -> bool {
        self.messages.queue.server.is_some()
    }

    /// Spec 8.1: the attempt of text or receipt `reach.key` did not reach every device; those
    /// it should have and did not (see the module doc) get it in their queues, with what they
    /// lack ahead of it. Call it before the attempt's outcome is stored. Nothing without an
    /// address, or while a `SEND` of it is on its way.
    pub(in crate::account::manager) async fn queue_put(&mut self, reach: &Reach, request: Request) {
        if !self.queue_on() {
            return;
        }
        let Ok(local) = self.local() else {
            return;
        };
        let (own, me) = (local.keys.peer_id(), local.device.installation_id());
        let devices: Vec<InstallationId> = if reach.receipt {
            vec![reach.key.installation]
        } else {
            let near: Vec<InstallationId> = self
                .mls_sessions()
                .into_iter()
                .filter(|s| s.grant.peer == own)
                .map(|s| s.grant.installation)
                .collect();
            let mine = self
                .current_installations(own)
                .into_iter()
                .filter(|i| *i != me && *i != reach.key.installation && !near.contains(i));
            self.current_installations(reach.peer)
                .into_iter()
                .chain(mine)
                .collect()
        };
        let unreached = Unreached::Message {
            key: reach.key,
            receipt: reach.receipt,
            devices: devices
                .into_iter()
                .filter(|i| !reach.reached.contains(i))
                .collect(),
        };
        self.queue_send(Some(reach.key), unreached, request).await;
    }

    /// Routing frames were sealed: the standing pass runs at the next poll.
    pub(in crate::account::manager) fn queue_standing_soon(&mut self) {
        self.messages.queue.put.standing_at = 0;
    }

    /// The standing pass, when it is due: what is still queued for each device without a
    /// session goes into its queue. Again after `STANDING_EVERY_MS`, or at once when a device
    /// was left out and the request stored something (`queue_sent`).
    pub(super) async fn queue_standing(&mut self, request: Request) {
        let now = self.schedule_now();
        let put = &self.messages.queue.put;
        if self.messages.queue.server.is_none()
            || now < put.standing_at
            || put.sending.contains(&None)
        {
            return;
        }
        let reached = self
            .mls_sessions()
            .into_iter()
            .map(|s| s.grant.installation)
            .collect();
        let all = self
            .queue_send(None, Unreached::Standing { reached }, request)
            .await;
        let put = &mut self.messages.queue.put;
        put.standing_at = now.saturating_add(STANDING_EVERY_MS);
        put.standing_rest = !all;
    }

    /// One `SEND` of what `unreached` lacks, in a spawned task; `what` names it (`Put.sending`).
    /// `false`: a device was left out for later.
    async fn queue_send(
        &mut self,
        what: Option<MessageKey>,
        unreached: Unreached,
        request: Request,
    ) -> bool {
        let Some(server) = self.messages.queue.server.clone() else {
            return true;
        };
        let Ok(me) = self.local().map(|local| local.keys.peer_id()) else {
            return true;
        };
        if self.messages.queue.put.sending.contains(&what) {
            return true;
        }
        let wall = self.services.clock.wall_ms();
        let op = QueueDbOp::ToSend {
            unreached,
            server_url: server.url.to_string(),
            wall,
        };
        let Ok(QueueDbReply::ToSend(Some(send))) = self.queue_db(op, request).await else {
            return true;
        };
        let put = &mut self.messages.queue.put;
        let (items, sends, all) = send_items(send, me, wall, put.turn, &put.flying);
        if sends.is_empty() {
            // Nothing fits even alone, or all is on its way: no sooner pass would change that.
            return true;
        }
        put.turn = put.turn.wrapping_add(1);
        put.sending.insert(what);
        put.flying
            .extend(items.iter().map(|q| (q.installation, q.frame_hash)));
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
                    what,
                    items,
                    answers,
                    ts: wall,
                }))
                .await;
        });
        all
    }

    /// A `SEND` came back: the queues that took a frame hold it until the server drops it, and
    /// a queue the server does not know is dead. Anything else is left to the next trigger. A
    /// standing request that left a device out and stored something runs the pass again at the
    /// next poll.
    pub(super) async fn queue_sent(
        &mut self,
        ticket: u64,
        what: Option<MessageKey>,
        items: Vec<Queued>,
        answers: Vec<ItemResult>,
        ts: u64,
    ) {
        let put = &mut self.messages.queue.put;
        put.sending.remove(&what);
        for q in &items {
            put.flying.remove(&(q.installation, q.frame_hash));
        }
        let Some(request) = self.ready().ok().filter(|r| r.ticket == ticket) else {
            return;
        };
        let (taken, gone) = sent_outcome(&items, &answers);
        if taken.is_empty() && gone.is_empty() {
            return;
        }
        let stored = !taken.is_empty();
        let op = QueueDbOp::Sent { taken, gone, ts };
        // Not stored: the frames go in again at the next trigger.
        let noted = self.queue_db(op, request).await.is_ok();
        let put = &mut self.messages.queue.put;
        if what.is_none() && put.standing_rest && stored && noted {
            put.standing_at = 0;
        }
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
            conversation: conv(1),
            installation: InstallationId([n; 32]),
            queue_id: [n; 32],
            sender_secret: Zeroizing::new([n; 32]),
            frames: frames
                .into_iter()
                .map(|frame| (InstallationId([8; 32]), frame))
                .collect(),
        }
    }

    fn at(n: u8, frame: &[u8]) -> Queued {
        Queued {
            conversation: conv(1),
            installation: InstallationId([n; 32]),
            queue_id: [n; 32],
            frame_hash: frame_hash(frame),
        }
    }

    #[test]
    fn send_items_keep_each_devices_frames_in_order_signed_by_its_queue() {
        let me = PeerId([9; 32]);
        let mut sealed_elsewhere = target(1, vec![b"commit".to_vec()]);
        sealed_elsewhere
            .frames
            .push((InstallationId([7; 32]), b"text".to_vec()));
        let send = ToSend {
            targets: vec![sealed_elsewhere, target(2, vec![b"text".to_vec()])],
        };
        let (queued, items, all) = send_items(send, me, T, 0, &BTreeSet::new());
        assert!(all);
        assert_eq!(queued, [at(1, b"commit"), at(1, b"text"), at(2, b"text")]);
        let entries: Vec<(InstallationId, Vec<u8>)> = items
            .iter()
            .zip(&queued)
            .map(|(item, q)| {
                assert_eq!(item.queue_id, b64(&q.queue_id));
                assert_eq!(item.ts, T);
                let ct = queue_wire::decode_ct(&item.ct).unwrap();
                let sig: [u8; 64] = queue_wire::decode_array(&item.sig).unwrap();
                assert!(queue_wire::verify(
                    &public_key(&q.queue_id),
                    &signing_bytes(&Signed::Send { ct: &ct }, &q.queue_id, T),
                    &sig
                ));
                let entry = QueueEntry::decode(&ct).unwrap();
                assert_eq!(entry.sender_peer, me);
                (entry.sender_install, entry.frame)
            })
            .collect();
        // Each entry names the frame's sealer.
        let (mine, origin) = (InstallationId([8; 32]), InstallationId([7; 32]));
        assert_eq!(
            entries,
            [
                (mine, b"commit".to_vec()),
                (origin, b"text".to_vec()),
                (mine, b"text".to_vec())
            ]
        );
    }

    #[test]
    fn send_items_leave_out_a_device_whose_frames_do_not_fit() {
        let me = PeerId([9; 32]);
        // Nine frames at the frame limit fill most of one request; a second such device does
        // not fit after it, a small one does.
        let big = vec![vec![0xAB; 61_440]; 9];
        let send = ToSend {
            targets: vec![
                target(1, big.clone()),
                target(2, big),
                target(3, vec![b"text".to_vec()]),
            ],
        };
        let (queued, items, all) = send_items(send, me, T, 0, &BTreeSet::new());
        assert!(!all);
        let devices: Vec<u8> = queued.iter().map(|q| q.installation.0[0]).collect();
        assert_eq!(devices, [1, 1, 1, 1, 1, 1, 1, 1, 1, 3]);
        let body = serde_json::to_vec(&queue_wire::QueueRequest { items }).unwrap();
        assert!(body.len() <= MAX_BODY, "{}", body.len());
    }

    /// Review minor 2: the next request starts one device further on, so the device left out
    /// behind a large one goes first and fits.
    #[test]
    fn send_items_start_one_device_further_on_each_request() {
        let me = PeerId([9; 32]);
        let send = |start| {
            let big = vec![vec![0xAB; 61_440]; 9];
            let send = ToSend {
                targets: vec![target(1, big.clone()), target(2, big)],
            };
            let (queued, _, _) = send_items(send, me, T, start, &BTreeSet::new());
            let mut devices: Vec<u8> = queued.iter().map(|q| q.installation.0[0]).collect();
            devices.dedup();
            devices
        };
        assert_eq!(send(0), [1]);
        assert_eq!(send(1), [2]);
        assert_eq!(send(2), [1]);
    }

    /// Review minor 4: a frame a request on its way carries to a device stays out of the next
    /// one for that device; the device's other frames, and another device's copy, go.
    #[test]
    fn send_items_leave_out_frames_on_their_way() {
        let me = PeerId([9; 32]);
        let send = ToSend {
            targets: vec![
                target(1, vec![b"commit".to_vec(), b"text".to_vec()]),
                target(2, vec![b"commit".to_vec()]),
            ],
        };
        let flying = BTreeSet::from([(InstallationId([1; 32]), frame_hash(b"commit"))]);
        let (queued, items, all) = send_items(send, me, T, 0, &flying);
        assert!(all);
        assert_eq!(queued, [at(1, b"text"), at(2, b"commit")]);
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn sent_outcome_takes_each_frame_answered_ok() {
        let items = [
            at(1, b"commit"),
            at(1, b"text"),
            at(2, b"a"),
            at(2, b"b"),
            at(3, b"text"),
            at(4, b"text"),
            at(5, b"text"),
        ];
        let answers = [
            ItemResult::Ok,
            ItemResult::Ok,
            ItemResult::Gone,
            ItemResult::Gone,
            ItemResult::Full,
            ItemResult::Limited,
            ItemResult::Bad,
        ];
        assert_eq!(
            sent_outcome(&items, &answers),
            (vec![at(1, b"commit"), at(1, b"text")], vec![at(2, b"a")])
        );
        // No answer (the request failed): neither.
        assert_eq!(sent_outcome(&items, &[]), (vec![], vec![]));
    }
}
