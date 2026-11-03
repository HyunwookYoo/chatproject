#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared M4/M5 harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! Task 11: MLS 1:1 messaging under a hostile network. The test is the network for application
//! messages: every ciphertext a device seals reaches its recipients late, twice, out of order or
//! not at all (and is then sent again, the same ciphertext), all drawn from a fixed seed, while
//! Commits that add and remove a device run through the real group protocol between the cores.
include!("support/mls_messaging.rs");

use chat_core::account::InstallationId;
use chat_core::storage_fixture::{CommitFault, Messages};
use std::collections::BTreeSet;

/// The fixed seed: a failure replays exactly.
const SEED: u64 = 0x4d35_4d4c_5331_0b11;
/// Ticks (one per sent message) a copy may wait before it is handed over.
const DELAY: u64 = 40;
/// Percent of copies that are sent twice.
const DUPLICATE: u64 = 30;
/// Percent of hand-overs that are lost (the copy is sent again later).
const LOSS: u64 = 10;

/// Device indexes: A has two installations, B one.
const DA1: usize = 0;
const DA2: usize = 1;
const DB1: usize = 2;

/// splitmix64.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.below(i as u64 + 1) as usize;
            items.swap(i, j);
        }
    }
}

/// Where this test network carries a device's application messages: the other account's
/// devices. The best-effort spec 5.3 copies to the sender's own other devices go through the
/// actors (`actor_path_two_receiving_devices_seeded`).
fn recipients(from: usize) -> Vec<usize> {
    if from == DB1 {
        vec![DA1, DA2]
    } else {
        vec![DB1]
    }
}

/// A1, A2 and B1 as real cores; B's record also lists B2, which never runs (its detached
/// KeyPackage is what the membership Commits add and remove). B1 makes the group with A1 and
/// A2, and every device's automatic message delivery is held: the test carries the messages.
async fn three() -> (Vec<Fixture>, Arc<Backend>, ConversationId) {
    let (ra, rb) = (record(A, &[A1, A2], 0), record(B, &[B1, B2], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let a2 = device(A, A2, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    let devices = vec![a1, a2, b1];
    befriend(&devices[DA1], &devices[DB1]).await;
    befriend(&devices[DA2], &devices[DB1]).await;
    befriend(&devices[DB1], &devices[DA1]).await;
    connect(&devices).await;
    let conv = conversation(&devices[DB1], &devices[DA1]).await;
    conversation(&devices[DA1], &devices[DB1]).await;
    conversation(&devices[DA2], &devices[DB1]).await;
    ensure(&devices[DB1], devices[DA1].peer).await;
    settle!(
        devices.iter(),
        agreed(&devices[DA1], &devices[DB1], conv).await
            && agreed(&devices[DA2], &devices[DB1], conv).await
            && state(&devices[DB1], conv).await.members.len() == 3
            && state(&devices[DB1], conv).await.deliveries.is_empty()
    );
    for f in &devices {
        f.account.message_test_hold_scheduler(true).await.unwrap();
    }
    (devices, net, conv)
}

/// Every device can dial every other one, and holds a session with each device in the group:
/// A1 and A2 dial B, A2 dials A1. B1 does not dial A itself: `connect_account` makes every
/// session with that account not ready and proves itself to the first device that answers
/// only, which would leave B1 without a session to A2 (Commits go over existing sessions).
async fn connect(devices: &[Fixture]) {
    for (i, x) in devices.iter().enumerate() {
        for y in &devices[i + 1..] {
            link(x, y).await;
        }
    }
    for (d, account) in [(DA1, B), (DA2, B), (DA2, A)] {
        if let Err(e) = devices[d].account.connect_account(peer_of(account)).await {
            panic!("device {d} to account {account}: {e:?}");
        }
    }
}

/// Stops a device's core and starts it again on the same data directory (the M4 restart).
async fn restart(f: Fixture, account: u8, net: &Arc<Backend>) -> Fixture {
    let mut f = start(stop(f), net.clone()).await;
    f.peer = peer_of(account);
    f.account.message_test_hold_scheduler(true).await.unwrap();
    f
}

async fn stores(devices: &[Fixture]) -> Vec<Messages> {
    let mut stores = Vec::new();
    for f in devices {
        stores.push(f.account.storage_test_fixture().messages().await.unwrap());
    }
    stores
}

async fn installations(devices: &[Fixture]) -> Vec<InstallationId> {
    let mut ids = Vec::new();
    for f in devices {
        ids.push(installation(f).await);
    }
    ids
}

/// One copy of a ciphertext on its way from device `from` to device `to`.
struct Copy {
    from: usize,
    to: usize,
    key: MessageKey,
    /// The message's place in sealing order (`Stream::sent`).
    seq: usize,
    ciphertext: Vec<u8>,
    /// The epoch it was sealed in.
    epoch: u64,
    due: u64,
}

/// A message as its sender stored it.
struct Sent {
    from: usize,
    key: MessageKey,
    lamport: u64,
    body: String,
}

/// The hostile network and what went through it.
struct Stream {
    rng: Rng,
    tick: u64,
    /// The group epoch every device is at (Commits are settled before the next message).
    epoch: u64,
    queue: Vec<Copy>,
    sent: Vec<Sent>,
    /// (recipient, msg_id) that answered `Ack` at least once.
    acked: BTreeSet<(usize, [u8; 16])>,
    /// Messages whose sender recorded the first `Ack` (Delivered).
    delivered: BTreeSet<[u8; 16]>,
    /// Every answer other than `Ack`, described.
    failures: Vec<String>,
    copies: u64,
    duplicates: u64,
    lost: u64,
    handed: u64,
    /// Hand-overs of a ciphertext sealed in an older epoch than the recipient's.
    past_epoch: u64,
    /// Hand-overs of a message after a later one of the same sender reached that recipient.
    reordered: u64,
    /// The latest `seq` handed over per (sender, recipient).
    newest: BTreeMap<(usize, usize), usize>,
}

impl Stream {
    fn new(epoch: u64) -> Self {
        Self {
            rng: Rng(SEED),
            tick: 0,
            epoch,
            queue: Vec::new(),
            sent: Vec::new(),
            acked: BTreeSet::new(),
            delivered: BTreeSet::new(),
            failures: Vec::new(),
            copies: 0,
            duplicates: 0,
            lost: 0,
            handed: 0,
            past_epoch: 0,
            reordered: 0,
            newest: BTreeMap::new(),
        }
    }

    /// The sender's message `n`, queued through the product API (not sealed yet).
    async fn queue_text(&mut self, devices: &[Fixture], from: usize, n: u64) -> MessageKey {
        let mut id = [0; 16];
        id[..8].copy_from_slice(&n.to_be_bytes());
        id[8] = from as u8;
        let to = recipients(from)[0];
        let body = format!("resilience message {n} from device {from}");
        let view = devices[from]
            .account
            .messages()
            .send_text(devices[to].peer, MessageId(id), body.clone())
            .await
            .unwrap();
        self.sent.push(Sent {
            from,
            key: view.key,
            lamport: view.lamport,
            body,
        });
        view.key
    }

    /// Seals a queued message and hands its ciphertext to the network.
    async fn seal(
        &mut self,
        devices: &[Fixture],
        stores: &[Messages],
        conv: ConversationId,
        from: usize,
        key: MessageKey,
    ) {
        stores[from]
            .mls_seal(key, NOW * 1000)
            .await
            .unwrap()
            .expect("the group exists");
        let wire = outbox(&devices[from], key).await.wire;
        let (framed, ciphertext) = mls_app_from_wire(&wire).unwrap();
        assert_eq!(framed, conv);
        let seq = self.sent.iter().position(|s| s.key == key).unwrap();
        for to in recipients(from) {
            self.post(from, to, key, seq, ciphertext.clone());
        }
    }

    async fn send(
        &mut self,
        devices: &[Fixture],
        stores: &[Messages],
        conv: ConversationId,
        from: usize,
        n: u64,
    ) {
        let key = self.queue_text(devices, from, n).await;
        self.seal(devices, stores, conv, from, key).await;
    }

    fn post(&mut self, from: usize, to: usize, key: MessageKey, seq: usize, ciphertext: Vec<u8>) {
        let copies = if self.rng.chance(DUPLICATE) {
            self.duplicates += 1;
            2
        } else {
            1
        };
        for _ in 0..copies {
            let due = self.tick + self.rng.below(DELAY);
            self.copies += 1;
            self.queue.push(Copy {
                from,
                to,
                key,
                seq,
                ciphertext: ciphertext.clone(),
                epoch: self.epoch,
                due,
            });
        }
    }

    /// One tick: every copy that is due is handed over, in a shuffled order; a lost hand-over
    /// is sent again (the same ciphertext) a few ticks later.
    async fn step(
        &mut self,
        devices: &[Fixture],
        stores: &[Messages],
        installs: &[InstallationId],
        conv: ConversationId,
    ) {
        self.tick += 1;
        let (mut due, waiting): (Vec<Copy>, Vec<Copy>) = std::mem::take(&mut self.queue)
            .into_iter()
            .partition(|copy| copy.due <= self.tick);
        self.queue = waiting;
        self.rng.shuffle(&mut due);
        for mut copy in due {
            if self.rng.chance(LOSS) {
                self.lost += 1;
                copy.due = self.tick + 1 + self.rng.below(DELAY / 2);
                self.queue.push(copy);
                continue;
            }
            self.handed += 1;
            if copy.epoch < self.epoch {
                self.past_epoch += 1;
            }
            let newest = self.newest.entry((copy.from, copy.to)).or_insert(copy.seq);
            if copy.seq < *newest {
                self.reordered += 1;
            }
            *newest = (*newest).max(copy.seq);
            let reply = stores[copy.to]
                .mls_open_app(
                    devices[copy.from].peer,
                    installs[copy.from],
                    conv,
                    copy.ciphertext.clone(),
                    NOW * 1000,
                )
                .await
                .unwrap();
            match reply {
                MlsReply::Ack { .. } => {
                    self.acked.insert((copy.to, copy.key.id.0));
                    if self.delivered.insert(copy.key.id.0) {
                        // What the sender does with the first `Ack`: the message is delivered.
                        devices[copy.from]
                            .account
                            .storage_test_fixture()
                            .deliver_message(copy.key)
                            .await
                            .unwrap();
                    }
                }
                MlsReply::DecryptFailed { class, epoch } => self.failures.push(format!(
                    "device {} could not open message {:?} of device {} sealed in epoch {} (now {}): class {class}, epoch {epoch}",
                    copy.to, copy.key.id.0, copy.from, copy.epoch, self.epoch
                )),
                _ => self.failures.push(format!(
                    "device {} answered message {:?} with neither Ack nor DecryptFailed",
                    copy.to, copy.key.id.0
                )),
            }
        }
    }

    /// Ticks until nothing is on its way any more.
    async fn drain(
        &mut self,
        devices: &[Fixture],
        stores: &[Messages],
        installs: &[InstallationId],
        conv: ConversationId,
    ) {
        while !self.queue.is_empty() {
            self.step(devices, stores, installs, conv).await;
        }
    }

    fn summary(&self) -> String {
        format!(
            "{} messages, {} copies ({} duplicated), {} hand-overs ({} behind a later message, {} into a later epoch), {} lost and sent again, {} failures, {} ticks",
            self.sent.len(),
            self.copies,
            self.duplicates,
            self.handed,
            self.reordered,
            self.past_epoch,
            self.lost,
            self.failures.len(),
            self.tick
        )
    }
}

/// `committer` adds B2 (`add`) or removes it in one own Commit, delivered by the cores' own
/// group protocol; returns once every device is at the new epoch and the Commit is settled.
async fn change_members(devices: &[Fixture], conv: ConversationId, committer: usize, add: bool) {
    let peer = devices[recipients(committer)[0]].peer;
    let b2 = derive_device(Secret32::from([B2; 32]))
        .unwrap()
        .installation_id();
    let (added, removed) = if add {
        (vec![mls_detached_key_package(peer_of(B), [B2; 32])], vec![])
    } else {
        (vec![], vec![b2])
    };
    let epoch = devices[committer]
        .account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mls_commit(conv, peer, added, removed, NOW * 1000)
        .await
        .unwrap()
        .expect("an own Commit");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let mut done = !state(&devices[committer], conv)
            .await
            .own_commit_unacked
            .unwrap();
        for f in devices {
            done &= state(f, conv).await.group_epoch == Some(epoch);
        }
        if done {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            for (d, f) in devices.iter().enumerate() {
                let s = state(f, conv).await;
                eprintln!(
                    "device {d}: epoch {:?} unacked {:?} members {} deliveries {:?} holds {}",
                    s.group_epoch,
                    s.own_commit_unacked,
                    s.members.len(),
                    s.deliveries,
                    s.holds
                );
            }
            for (d, f) in devices.iter().enumerate() {
                let other = if d == DB1 { peer_of(A) } else { peer_of(B) };
                eprintln!(
                    "device {d} {:?}: query other {:?} own {:?}; sent {:?}",
                    installation(f).await.0[..4].to_vec(),
                    f.account.mls_test_query(other).await.map(|v| v
                        .iter()
                        .map(|(i, g)| (i.0[..4].to_vec(), *g))
                        .collect::<Vec<_>>()),
                    f.account.mls_test_query(f.peer).await.map(|v| v
                        .iter()
                        .map(|(i, g)| (i.0[..4].to_vec(), *g))
                        .collect::<Vec<_>>()),
                    f.account.mls_test_sent_frames().await.unwrap()
                );
            }
            panic!("Commit to epoch {epoch} by device {committer} did not settle");
        }
        for f in devices {
            f.clock.advance_monotonic_ms(1_000);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    for f in devices {
        assert_eq!(
            state(f, conv).await.members.contains(&b2),
            add,
            "B2 membership after the Commit"
        );
    }
}

/// Every message of every page, newest first.
async fn everything(f: &Fixture, conv: ConversationId) -> Vec<MessageView> {
    let mut all = Vec::new();
    let mut cursor = None;
    loop {
        let page = f
            .account
            .messages()
            .load_messages(conv, cursor, 50)
            .await
            .unwrap();
        all.extend(page.items);
        match page.next {
            Some(next) => cursor = Some(next),
            None => return all,
        }
    }
}

/// Each device holds exactly its own messages and every message addressed to it, once each,
/// with the sender's lamport and text, ordered by `(lamport, sender_installation, msg_id)`;
/// and nothing failed on the way.
async fn verify(devices: &[Fixture], conv: ConversationId, stream: &Stream) {
    assert!(
        stream.failures.is_empty(),
        "{}\n{}",
        stream.summary(),
        stream.failures.join("\n")
    );
    for (d, f) in devices.iter().enumerate() {
        let expected: BTreeMap<MessageKey, &Sent> = stream
            .sent
            .iter()
            .filter(|s| s.from == d || recipients(s.from).contains(&d))
            .map(|s| (s.key, s))
            .collect();
        let items = everything(f, conv).await;
        let keys: BTreeSet<MessageKey> = items.iter().map(|m| m.key).collect();
        assert_eq!(keys.len(), items.len(), "device {d} holds a message twice");
        assert_eq!(
            keys,
            expected.keys().copied().collect::<BTreeSet<_>>(),
            "device {d} holds other messages than were sent to it"
        );
        for m in &items {
            let sent = expected[&m.key];
            assert_eq!(
                m.lamport, sent.lamport,
                "device {d}: lamport of {:?}",
                m.key
            );
            assert_eq!(m.body, sent.body, "device {d}: text of {:?}", m.key);
            assert_eq!(m.outgoing, sent.from == d);
            if m.outgoing {
                assert_eq!(m.state, DeliveryState::Delivered, "device {d}: {:?}", m.key);
            }
        }
        let order: Vec<_> = items
            .iter()
            .map(|m| (m.lamport, m.key.installation.0, m.key.id.0))
            .collect();
        assert!(
            order.windows(2).all(|w| w[0] > w[1]),
            "device {d}: not ordered by (lamport, sender_installation, msg_id)"
        );
        let incoming = expected.values().filter(|s| s.from != d).count() as u64;
        let unread = f
            .account
            .messages()
            .list_conversations(None, 50)
            .await
            .unwrap()
            .items
            .into_iter()
            .find(|c| c.id == conv)
            .unwrap()
            .unread;
        assert_eq!(unread, incoming, "device {d}: unread count");
    }
    // Every recipient acknowledged every message sent to it.
    for s in &stream.sent {
        for to in recipients(s.from) {
            assert!(stream.acked.contains(&(to, s.key.id.0)));
        }
    }
}

#[tokio::test]
async fn delayed_duplicated_reordered_messages_all_arrive_once() {
    const MESSAGES: u64 = 300;
    // Membership Commits during the run: B2 added by A1, removed by B1, added by A2, removed
    // by B1. Ciphertexts still on their way cross them (older epochs, within the five kept).
    let changes: BTreeMap<u64, (usize, bool)> = [
        (60, (DA1, true)),
        (120, (DB1, false)),
        (180, (DA2, true)),
        (240, (DB1, false)),
    ]
    .into();
    let (devices, _net, conv) = three().await;
    let (stores, installs) = (stores(&devices).await, installations(&devices).await);
    let mut stream = Stream::new(state(&devices[DA1], conv).await.group_epoch.unwrap());
    let first = stream.epoch;
    let started = std::time::Instant::now();
    for n in 0..MESSAGES {
        if let Some(&(committer, add)) = changes.get(&n) {
            change_members(&devices, conv, committer, add).await;
            stream.epoch += 1;
        }
        let from = stream.rng.below(3) as usize;
        stream.send(&devices, &stores, conv, from, n).await;
        stream.step(&devices, &stores, &installs, conv).await;
    }
    stream.drain(&devices, &stores, &installs, conv).await;
    eprintln!("{} in {:?}", stream.summary(), started.elapsed());
    assert!(stream.duplicates > 0 && stream.reordered > 0 && stream.lost > 0);
    assert!(stream.past_epoch > 0);
    verify(&devices, conv, &stream).await;
    assert_eq!(stream.epoch, first + 4);
    for f in &devices {
        assert_eq!(state(f, conv).await.group_epoch, Some(stream.epoch));
    }
}

/// The view of `key` on `f`, looked up across every page.
async fn find(f: &Fixture, conv: ConversationId, key: MessageKey) -> Option<MessageView> {
    everything(f, conv).await.into_iter().find(|m| m.key == key)
}

fn decrypt_failed(reply: MlsReply) -> (u8, u64) {
    match reply {
        MlsReply::DecryptFailed { class, epoch } => (class, epoch),
        MlsReply::Ack { .. } => panic!("expected DecryptFailed, got Ack"),
        _ => panic!("expected DecryptFailed"),
    }
}

#[tokio::test]
async fn out_of_range_is_recovered_not_shown() {
    let (a1, b1, _net, conv) = setup(&[B1, B2]).await;
    grouped(&a1, &b1, conv).await;
    let events = Arc::new(Events::default());
    b1.bus.subscribe(events.clone());
    a1.account.message_test_hold_scheduler(true).await.unwrap();
    let (ma, mb) = (
        a1.account.storage_test_fixture().messages().await.unwrap(),
        b1.account.storage_test_fixture().messages().await.unwrap(),
    );
    let ia = installation(&a1).await;
    let sealed = |key: MessageKey| {
        let (a1, ma) = (&a1, ma.clone());
        async move {
            ma.mls_seal(key, NOW * 1000).await.unwrap().unwrap();
            mls_app_from_wire(&outbox(a1, key).await.wire).unwrap().1
        }
    };

    // Beyond the past epochs kept: sealed, then held while the group moves on six epochs.
    let stale = send(&a1, &b1, 1, "sealed six epochs ago").await;
    let stale_ct = sealed(stale.key).await;
    commits(&a1, &b1, conv, 6).await;
    // Beyond the out-of-order window: sealed, then 201 newer messages reach B first.
    let behind = send(&a1, &b1, 2, "overtaken by 201 newer messages").await;
    let behind_ct = sealed(behind.key).await;
    for n in 3..=203u8 {
        let newer = send(&a1, &b1, n, &format!("newer message {n}")).await;
        let ct = sealed(newer.key).await;
        let reply = mb
            .mls_open_app(a1.peer, ia, conv, ct, NOW * 1000)
            .await
            .unwrap();
        assert!(acked(&reply), "newer message {n}");
        // Its Ack reached A: delivered, nothing left to send.
        a1.account
            .storage_test_fixture()
            .deliver_message(newer.key)
            .await
            .unwrap();
    }

    // Neither opens on B, and trying leaves nothing behind there.
    let before = state(&b1, conv).await;
    let counts = b1
        .account
        .storage_test_fixture()
        .message_counts()
        .await
        .unwrap();
    let open = |ct: &Vec<u8>| mb.mls_open_app(a1.peer, ia, conv, ct.clone(), NOW * 1000);
    assert_eq!(
        decrypt_failed(open(&stale_ct).await.unwrap()).0,
        1,
        "past epoch"
    );
    assert_eq!(
        decrypt_failed(open(&behind_ct).await.unwrap()).0,
        3,
        "out of order"
    );
    let after = state(&b1, conv).await;
    assert_eq!(
        (after.kv_digest, after.processed_mls),
        (before.kv_digest, before.processed_mls)
    );
    assert_eq!(
        b1.account
            .storage_test_fixture()
            .message_counts()
            .await
            .unwrap(),
        counts
    );
    assert_eq!(everything(&b1, conv).await.len(), 201);

    // The product's own recovery: each old ciphertext is sent again as it is, B answers with
    // a decrypt failure, A seals the message again in the current epoch, and B stores it.
    a1.account.message_test_hold_scheduler(false).await.unwrap();
    settle!(
        [&a1, &b1],
        find(&a1, conv, stale.key).await.unwrap().state == DeliveryState::Delivered
            && find(&a1, conv, behind.key).await.unwrap().state == DeliveryState::Delivered
    );
    let cts = app_ciphertexts(&a1).await;
    assert_eq!(
        cts.len(),
        4,
        "per message: the old ciphertext once, then one re-encryption"
    );
    assert_eq!(cts.iter().filter(|ct| **ct == stale_ct).count(), 1);
    assert_eq!(cts.iter().filter(|ct| **ct == behind_ct).count(), 1);
    let fresh: BTreeSet<&Vec<u8>> = cts
        .iter()
        .filter(|ct| **ct != stale_ct && **ct != behind_ct)
        .collect();
    assert_eq!(fresh.len(), 2);

    // B has every message once, with nothing that tells of the failures.
    let items = everything(&b1, conv).await;
    assert_eq!(items.len(), 203);
    assert_eq!(
        items.iter().map(|m| m.key).collect::<BTreeSet<_>>().len(),
        203
    );
    for (sent, body) in [
        (&stale, "sealed six epochs ago"),
        (&behind, "overtaken by 201 newer messages"),
    ] {
        let got = items.iter().find(|m| m.key == sent.key).unwrap();
        assert_eq!((got.body.as_str(), got.lamport), (body, sent.lamport));
    }
    assert!(items.iter().all(|m| !m.outgoing && m.block.is_none()));
    let unread = b1
        .account
        .messages()
        .list_conversations(None, 50)
        .await
        .unwrap()
        .items[0]
        .unread;
    assert_eq!(unread, 203);
    assert_eq!(
        state(&b1, conv).await.processed_mls,
        before.processed_mls + 2,
        "only the two stored ciphertexts were recorded"
    );
    for event in events.0.lock().unwrap().iter() {
        match event {
            CoreEvent::MessageStateChanged { state, .. } => {
                assert_ne!(*state, DeliveryState::Failed)
            }
            CoreEvent::Error { .. } => panic!("an error event on the receiver: {event:?}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn restart_mid_stream_resumes() {
    const MESSAGES: u64 = 150;
    const RESTART_AT: u64 = 70;
    // B2 added by A1 before both apps restart, removed by B1 after.
    let changes: BTreeMap<u64, (usize, bool)> = [(40, (DA1, true)), (110, (DB1, false))].into();
    let (mut devices, net, conv) = three().await;
    let (mut stores_, installs) = (stores(&devices).await, installations(&devices).await);
    let mut stream = Stream::new(state(&devices[DA1], conv).await.group_epoch.unwrap());
    let group = state(&devices[DB1], conv).await.group_id;
    for n in 0..MESSAGES {
        if let Some(&(committer, add)) = changes.get(&n) {
            change_members(&devices, conv, committer, add).await;
            stream.epoch += 1;
        }
        if n == RESTART_AT {
            // B1 dies while storing a message: after the ratchet moved and the row was
            // written, before the commit. Nothing of it survives; the copy stays on its way.
            let copy = stream
                .queue
                .iter()
                .position(|c| c.to == DB1)
                .expect("a copy on its way to B1");
            let (from, ciphertext) = (
                stream.queue[copy].from,
                stream.queue[copy].ciphertext.clone(),
            );
            devices[DB1]
                .account
                .mls_test_hold_delivery(true)
                .await
                .unwrap();
            let before = state(&devices[DB1], conv).await;
            devices[DB1]
                .account
                .storage_test_fixture()
                .message_fault(CommitFault::BeforeCommit)
                .await;
            let error = stores_[DB1]
                .mls_open_app(
                    devices[from].peer,
                    installs[from],
                    conv,
                    ciphertext,
                    NOW * 1000,
                )
                .await
                .err()
                .expect("the store died before its commit");
            assert_eq!(error.code, ErrorCode::StorageUncertain);
            let after = state(&devices[DB1], conv).await;
            assert_eq!(
                (after.kv_digest, after.processed_mls),
                (before.kv_digest, before.processed_mls)
            );
            // A1 has a message queued but not sealed yet when it stops.
            let waiting = stream.queue_text(&devices, DA1, 2_000).await;
            assert_eq!(outbox(&devices[DA1], waiting).await.codec, Some(3));
            // Every app restarts on its data directory. (A2 restarts too: from its old endpoint,
            // dialing the restarted B1 at its new address timed out in this harness.)
            let mut restarted = Vec::new();
            for (d, f) in devices.drain(..).enumerate() {
                restarted.push(restart(f, if d == DB1 { B } else { A }, &net).await);
            }
            devices = restarted;
            connect(&devices).await;
            stores_ = stores(&devices).await;
            assert_eq!(installations(&devices).await, installs);
            for f in &devices {
                let s = state(f, conv).await;
                assert_eq!((s.group_epoch, &s.group_id), (Some(stream.epoch), &group));
            }
            // The message that waited is sealed after the restart and goes out like the rest.
            stream.seal(&devices, &stores_, conv, DA1, waiting).await;
        }
        let from = stream.rng.below(3) as usize;
        stream.send(&devices, &stores_, conv, from, n).await;
        stream.step(&devices, &stores_, &installs, conv).await;
    }
    stream.drain(&devices, &stores_, &installs, conv).await;
    eprintln!("{}", stream.summary());
    verify(&devices, conv, &stream).await;
    for f in &devices {
        assert_eq!(state(f, conv).await.group_epoch, Some(stream.epoch));
    }
}

// ---- Actor path: one account with two receiving devices (I4) ---------------------------------

/// B3 is a device of B that never runs: its detached KeyPackage is what membership Commits
/// add and remove.
const B3: u8 = 16;

/// A = [A1]; B = [B1, B2] (+ B3, never running). B1 and B2 dial A1 and B2 dials B1; A1 dials
/// nobody. A1 makes the group with B1 and B2. Every device runs its real scheduler.
async fn two_receivers() -> (Fixture, Fixture, Fixture, Arc<Backend>, ConversationId) {
    let (ra, rb) = (record(A, &[A1], 0), record(B, &[B1, B2, B3], 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    let b2 = device(B, B2, &rb, &net).await;
    link(&a1, &b1).await;
    link(&a1, &b2).await;
    link(&b1, &b2).await;
    befriend(&a1, &b1).await;
    befriend(&b1, &a1).await;
    befriend(&b2, &a1).await;
    b1.account.connect_account(a1.peer).await.unwrap();
    b2.account.connect_account(a1.peer).await.unwrap();
    b2.account.connect_account(b1.peer).await.unwrap();
    let conv = conversation(&a1, &b1).await;
    conversation(&b1, &a1).await;
    conversation(&b2, &a1).await;
    ensure(&a1, b1.peer).await;
    settle!(
        [&a1, &b1, &b2],
        agreed(&a1, &b1, conv).await
            && agreed(&a1, &b2, conv).await
            && state(&a1, conv).await.deliveries.is_empty()
    );
    (a1, b1, b2, net, conv)
}

/// An own Commit by `f` adding or removing B3; not waited for (the next message's attempt or
/// the delivery poll carries it). False when an own Commit is still unacknowledged.
async fn commit_b3(f: &Fixture, conv: ConversationId, add: bool) -> bool {
    let b3 = derive_device(Secret32::from([B3; 32]))
        .unwrap()
        .installation_id();
    let peer = if f.peer == peer_of(A) {
        peer_of(B)
    } else {
        peer_of(A)
    };
    let (added, removed) = if add {
        (vec![mls_detached_key_package(peer_of(B), [B3; 32])], vec![])
    } else {
        (vec![], vec![b3])
    };
    f.account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mls_commit(conv, peer, added, removed, NOW * 1000)
        .await
        .unwrap()
        .is_some()
}

/// Every device at the same epoch, on the same branch, with no own Commit unacknowledged: the
/// next Commit cannot fork with one still on its way (forks are `mls_group.rs`'s subject).
async fn quiet(all: [&Fixture; 3], conv: ConversationId) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let mut done = true;
        for f in all {
            let s = state(f, conv).await;
            done &= !s.own_commit_unacked.unwrap() && s.epoch_authenticator.is_some();
            done &= agreed(all[0], f, conv).await;
        }
        if done {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            for (d, f) in all.iter().enumerate() {
                let s = state(f, conv).await;
                eprintln!(
                    "device {d}: epoch {:?} unacked {:?} members {} auth {:?} deliveries {:?} holds {} rejoins {:?} sent {:?}",
                    s.group_epoch,
                    s.own_commit_unacked,
                    s.members.len(),
                    s.epoch_authenticator.as_ref().map(|a| a[..4].to_vec()),
                    s.deliveries.iter().map(|(i, k, e)| (i.0[0], *k, *e)).collect::<Vec<_>>(),
                    s.holds,
                    s.rejoin_count,
                    f.account.mls_test_sent_frames().await.unwrap().iter().rev().take(16).collect::<Vec<_>>()
                );
            }
            // What each queued Commit does at its recipient.
            let s0 = state(all[0], conv).await;
            for (to, kind, epoch) in s0.deliveries {
                if kind != 1 { continue; }
                let frame = all[0].account.storage_test_fixture().mls_delivery_frame(conv, to, kind, epoch).await.unwrap().unwrap();
                let (_, e, commit) = mls_commit_from_wire(&frame).unwrap();
                for f in all {
                    if installation(f).await == to {
                        let r = f.account.storage_test_fixture().messages().await.unwrap()
                            .mls_apply_commit(conv, all[0].peer, all[0].peer, e, commit.clone(), NOW * 1000).await;
                        eprintln!("apply at {:?}: {:?}", to.0[0], r.map(|o| format!("{o:?}")));
                    }
                }
            }
            panic!("not quiet");
        }
        for f in all {
            f.clock.advance_monotonic_ms(1_000);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// One own Commit by `committer`, settled on every device before it returns.
async fn change_epoch(all: [&Fixture; 3], committer: &Fixture, conv: ConversationId, add: bool) {
    assert!(commit_b3(committer, conv, add).await, "an own Commit");
    let epoch = state(committer, conv).await.group_epoch;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        let mut done = !state(committer, conv).await.own_commit_unacked.unwrap();
        for f in all {
            done &= state(f, conv).await.group_epoch == epoch;
        }
        if done {
            return;
        }
        if tokio::time::Instant::now() > deadline {
            for (d, f) in all.iter().enumerate() {
                let s = state(f, conv).await;
                eprintln!(
                    "device {d}: epoch {:?} unacked {:?} members {} deliveries {:?} holds {} sent {:?}",
                    s.group_epoch,
                    s.own_commit_unacked,
                    s.members.len(),
                    s.deliveries,
                    s.holds,
                    f.account.mls_test_sent_frames().await.unwrap().iter().rev().take(12).collect::<Vec<_>>()
                );
            }
            panic!("Commit to {epoch:?} did not settle");
        }
        for f in all {
            f.clock.advance_monotonic_ms(1_000);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// I4: the message path itself (actors, schedulers, session choice, Commits sent ahead of a
/// message, the spec 5.3 copies, a lost answer, re-encryption after a long delay) between one
/// account with one device and one with two, both directions, drawn from a fixed seed. Every
/// message ends Delivered on its sender, each device holds every message once (the sender's
/// other device as the sender account's own message), and nothing ever shows as failed.
#[tokio::test]
async fn actor_path_two_receiving_devices_seeded() {
    let (a1, b1, b2, _net, conv) = two_receivers().await;
    let all = [&a1, &b1, &b2];
    let events: Vec<Arc<Events>> = all
        .iter()
        .map(|f| {
            let events = Arc::new(Events::default());
            f.bus.subscribe(events.clone());
            events
        })
        .collect();
    let mut rng = Rng(SEED ^ 0x1405);
    const MESSAGES: u64 = 24;
    let late = 3 + rng.below(MESSAGES - 6);
    let mut b3_in = false;
    let (mut ahead, mut lost, mut reencrypted) = (0, 0, 0);
    let mut sent: Vec<(usize, MessageKey, String)> = Vec::new();
    for n in 0..MESSAGES {
        let from = match rng.below(10) {
            0..=5 => 0,
            6 | 7 => 1,
            _ => 2,
        };
        let sender = all[from];
        let to = if from == 0 { &b1 } else { &a1 };
        let body = format!("actor path message {n} from device {from}");
        eprintln!("message {n} from {from}");
        if n == late {
            // Sealed now, sent after the group moved on six epochs: re-encrypted once.
            sender.account.message_test_hold_scheduler(true).await.unwrap();
            let view = send(sender, to, 100 + n as u8, &body).await;
            sender
                .account
                .storage_test_fixture()
                .messages()
                .await
                .unwrap()
                .mls_seal(view.key, NOW * 1000)
                .await
                .unwrap()
                .unwrap();
            quiet(all, conv).await;
            for _ in 0..6 {
                b3_in = !b3_in;
                change_epoch(all, sender, conv, b3_in).await;
            }
            sender.account.message_test_hold_scheduler(false).await.unwrap();
            settle!(all, delivery(sender, view.key).await == DeliveryState::Delivered);
            assert_eq!(outbox(sender, view.key).await.codec, None);
            reencrypted += 1;
            sent.push((from, view.key, body));
            continue;
        }
        if rng.chance(25) {
            quiet(all, conv).await;
            if commit_b3(sender, conv, !b3_in).await {
                b3_in = !b3_in;
                ahead += 1;
            }
        }
        // A lost answer (once): A1, the only device of A and so surely the recipient, stores
        // the text but holds its Ack past the sender's 5 s attempt limit. The attempt ends
        // written without an answer; the retry sends the same ciphertext and A1 acknowledges
        // the stored text.
        let lose = from != 0 && lost == 0 && rng.chance(50);
        let pause = if lose {
            Some(
                a1.account
                    .message_test_pause(chat_core::account::MessagePoint::Response)
                    .await
                    .unwrap(),
            )
        } else {
            None
        };
        let view = send(sender, to, 100 + n as u8, &body).await;
        if let Some(mut pause) = pause {
            pause.wait_entered().await;
            tokio::time::sleep(Duration::from_secs(6)).await;
            assert_eq!(delivery(sender, view.key).await, DeliveryState::SentDirect);
            pause.release();
            lost += 1;
        }
        settle!(
            all,
            matches!(
                delivery(sender, view.key).await,
                DeliveryState::Delivered | DeliveryState::Failed
            )
        );
        assert_eq!(delivery(sender, view.key).await, DeliveryState::Delivered);
        sent.push((from, view.key, body));
    }
    // The copies are best effort, but every device is connected: let them land.
    settle!(all, {
        let mut done = true;
        for f in all {
            done &= everything(f, conv).await.len() == sent.len();
        }
        done
    });
    for (d, f) in all.iter().enumerate() {
        let items = everything(f, conv).await;
        let keys: BTreeSet<MessageKey> = items.iter().map(|m| m.key).collect();
        assert_eq!(keys.len(), items.len(), "device {d} holds a message twice");
        for (from, key, body) in &sent {
            let m = items.iter().find(|m| m.key == *key).unwrap();
            assert_eq!(&m.body, body);
            let own_account = (*from == 0) == (d == 0);
            assert_eq!(m.outgoing, own_account, "device {d}: {key:?}");
            if *from == d {
                assert_eq!(m.state, DeliveryState::Delivered);
            } else if own_account {
                assert_eq!(m.state, DeliveryState::SentDirect, "a copy shows sent");
            }
        }
    }
    for (d, events) in events.iter().enumerate() {
        for event in events.0.lock().unwrap().iter() {
            match event {
                CoreEvent::MessageStateChanged { state, .. } => {
                    assert_ne!(*state, DeliveryState::Failed, "device {d}: {event:?}")
                }
                CoreEvent::Error { .. } => panic!("device {d}: {event:?}"),
                _ => {}
            }
        }
    }
    let mut epochs = Vec::new();
    for f in all {
        epochs.push(state(f, conv).await.group_epoch);
    }
    assert!(epochs.windows(2).all(|w| w[0] == w[1]), "{epochs:?}");
    eprintln!(
        "actor path: {} messages, {ahead} Commits ahead, {lost} lost answers, {reencrypted} \
         re-encrypted after six epochs, final epoch {:?}",
        sent.len(),
        epochs[0]
    );
    assert!(ahead > 0 && lost > 0 && reencrypted == 1);
}
