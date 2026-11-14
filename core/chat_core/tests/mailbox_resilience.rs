#![cfg(all(feature = "test-hooks", debug_assertions))]
// The shared harness carries helpers these tests do not use.
#![allow(dead_code, unused_imports)]
//! Task 14: own-device sync under churn (M6, spec 2, 6, 11). A = [A1, A2, A3] and B = [B1] run
//! as real cores with their actors and schedulers; a fixed seed decides who sends each of 120
//! texts, which of A's devices go off and come back meanwhile, when a device reads, and where a
//! device dies in the middle of a page. B adds a device (a Commit) once. In the end every device
//! of A has every message once, in the order they were sent, the same read state as the others,
//! and was notified of exactly the texts it first stored while they were still unread.
//!
//! `thousand_entries_transfer` (ignored, run in Release) measures the log: bytes per entry and
//! how long a device takes to get 1,000 entries from another.
include!("support/mailbox_network.rs");

use chat_core::account::InstallationId;
use std::collections::BTreeSet;
use std::time::Instant;

/// The fixed seed: a failing run replays the same schedule.
const SEED: u64 = 0x4d36_6d61_696c_0b14;
/// Texts sent in all, both ways.
const MESSAGES: u8 = 120;
/// Times one of A's devices reads (marks read) texts from B.
const READS: usize = 20;
/// Restarts that die in the middle of a page.
const HALTS: usize = 2;
/// Before this text, B1 adds B2 (a Commit) while a device of A is off.
const COMMIT_AT: u8 = 60;
/// Percent of steps at which one of A's devices goes off or comes back.
const TOGGLE: u64 = 20;

/// splitmix64 (as `mls_resilience.rs`).
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
}

/// Every message of the conversation, in the order `f` shows them.
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
            None => break,
        }
    }
    all.sort_by_key(|m| (m.lamport, m.key.installation, m.key.id));
    all
}

async fn unread(f: &Fixture, conv: ConversationId) -> u64 {
    f.account
        .messages()
        .list_conversations(None, 50)
        .await
        .unwrap()
        .items
        .into_iter()
        .find(|c| c.id == conv)
        .map_or(0, |c| c.unread)
}

async fn seen(f: &Fixture, key: MessageKey) -> bool {
    f.account
        .storage_test_fixture()
        .message_facts(key)
        .await
        .unwrap()
        .seen_local
}

/// The new messages of every notification `f` published.
fn notified(f: &Fixture) -> u64 {
    notify_events(f)
        .iter()
        .map(|i| u64::from(i.new_messages))
        .sum()
}

/// One of A's devices, across its restarts.
struct Device {
    name: &'static str,
    id: InstallationId,
    live: Option<Fixture>,
    dir: Option<tempfile::TempDir>,
    /// B's texts this device stores (as last seen).
    have: BTreeSet<MessageKey>,
    /// B's texts it first stored while they were unread on every device: what it should have
    /// been notified of.
    expected: u64,
    /// New messages notified by its earlier runs.
    notified: u64,
}

/// The run: devices, what was sent and read, and counts.
struct Run {
    devices: Vec<Device>,
    b1: Fixture,
    net: Arc<Backend>,
    conv: ConversationId,
    rng: Rng,
    /// Every text in the order it was sent.
    sent: Vec<MessageKey>,
    /// B's texts.
    from_b: BTreeSet<MessageKey>,
    /// B's texts some device of A read.
    read: BTreeSet<MessageKey>,
    reads: usize,
    halts: usize,
    toggles: usize,
    /// B's texts stored by a run that then died mid-page (stored; the next run notifies them,
    /// Ruling 35).
    halted_texts: u64,
    epoch: Option<u64>,
}

impl Run {
    fn on(&self) -> Vec<usize> {
        (0..self.devices.len())
            .filter(|&i| self.devices[i].live.is_some())
            .collect()
    }

    fn live(&self, i: usize) -> &Fixture {
        self.devices[i].live.as_ref().unwrap()
    }

    /// Every running core (the devices of A that are on, and B1).
    fn running(&self) -> Vec<&Fixture> {
        let mut all: Vec<&Fixture> = self
            .devices
            .iter()
            .filter_map(|d| d.live.as_ref())
            .collect();
        all.push(&self.b1);
        all
    }

    /// Whether every device of A that is on shows everything sent, as unread as it should be,
    /// and has processed what the others log; and B1 shows what was read.
    async fn converged(&self) -> bool {
        let on = self.on();
        let unread_b = self.from_b.difference(&self.read).count() as u64;
        for &i in &on {
            let f = self.live(i);
            if everything(f, self.conv).await.len() != self.sent.len()
                || unread(f, self.conv).await != unread_b
            {
                return false;
            }
            for &j in &on {
                if i != j && !caught_up(f, self.live(j)).await {
                    return false;
                }
            }
        }
        let b = everything(&self.b1, self.conv).await;
        b.len() == self.sent.len()
            && b.iter()
                .filter(|m| self.read.contains(&m.key))
                .all(|m| m.state == DeliveryState::Read)
    }

    /// Lets the cores run until `converged`, then notes which of B's texts each device of A
    /// that is on stores for the first time, and whether they were unread then.
    async fn settle(&mut self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
        while !self.converged().await {
            assert!(
                tokio::time::Instant::now() < deadline,
                "did not converge after {} texts ({} on): {:?}",
                self.sent.len(),
                self.on().len(),
                self.devices
                    .iter()
                    .map(|d| d.live.is_some())
                    .collect::<Vec<_>>()
            );
            for f in self.running() {
                f.clock.advance_monotonic_ms(1_000);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        for i in self.on() {
            let keys: Vec<MessageKey> = everything(self.live(i), self.conv)
                .await
                .into_iter()
                .map(|m| m.key)
                .filter(|k| self.from_b.contains(k))
                .collect();
            let device = &mut self.devices[i];
            for key in keys {
                if device.have.insert(key) && !self.read.contains(&key) {
                    device.expected += 1;
                }
            }
        }
    }

    /// Stops device `i`; the cores still running see its sessions close (their transport
    /// noticing it is gone).
    async fn off(&mut self, i: usize) {
        let f = self.devices[i].live.take().unwrap();
        self.devices[i].notified += notified(&f);
        let id = self.devices[i].id;
        self.devices[i].dir = Some(stop(f));
        for f in self.running() {
            f.account.mls_test_close_session(id).await.unwrap();
        }
        self.toggles += 1;
    }

    /// Starts device `i` again. While fewer than `HALTS` restarts died mid-page and it missed at
    /// least three of B's texts, this restart dies after storing half of them (no cursor moves,
    /// no notification), and it is restarted once more.
    async fn back(&mut self, i: usize) {
        let missed = self.from_b.difference(&self.devices[i].have).count() as u64;
        if self.halts < HALTS && missed >= 3 {
            self.halts += 1;
            let f = start(self.devices[i].dir.take().unwrap(), self.net.clone()).await;
            f.account.mailbox_test_halt_after(missed / 2).await.unwrap();
            self.reach(&f, i).await;
            f.account.mailbox_sync_now().await.unwrap();
            let before = self.devices[i].have.len() as u64;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
            let stored = loop {
                let stored: Vec<MessageKey> = everything(&f, self.conv)
                    .await
                    .into_iter()
                    .map(|m| m.key)
                    .filter(|k| self.from_b.contains(k) && !self.devices[i].have.contains(k))
                    .collect();
                if stored.len() as u64 >= missed / 2 {
                    break stored;
                }
                assert!(tokio::time::Instant::now() < deadline, "halt not reached");
                f.clock.advance_monotonic_ms(1_000);
                self.b1.clock.advance_monotonic_ms(1_000);
                tokio::time::sleep(Duration::from_millis(50)).await;
            };
            quiet(&self.running().into_iter().chain([&f]).collect::<Vec<_>>()).await;
            assert_eq!(
                notify_count(&f),
                0,
                "a run that died mid-page notifies nothing"
            );
            assert_eq!(
                stored.len() as u64,
                missed / 2,
                "it stops right after the texts it was allowed"
            );
            self.halted_texts += stored.len() as u64;
            // Ruling 35: the next run notifies what this one stored, unless read by then.
            self.devices[i].expected +=
                stored.iter().filter(|k| !self.read.contains(k)).count() as u64;
            self.devices[i].have.extend(stored);
            assert_eq!(self.devices[i].have.len() as u64, before + missed / 2);
            let id = self.devices[i].id;
            self.devices[i].dir = Some(stop(f));
            for f in self.running() {
                f.account.mls_test_close_session(id).await.unwrap();
            }
        }
        let f = start(self.devices[i].dir.take().unwrap(), self.net.clone()).await;
        self.reach(&f, i).await;
        f.account.mailbox_sync_now().await.unwrap();
        f.account.connect_account(self.b1.peer).await.unwrap();
        self.devices[i].live = Some(f);
        self.toggles += 1;
    }

    /// The restarted device `i` and every running core can dial each other.
    async fn reach(&self, f: &Fixture, i: usize) {
        for j in self.on() {
            if j != i {
                link(f, self.live(j)).await;
            }
        }
        link(f, &self.b1).await;
    }

    /// Device `i` goes off if another one stays on, or comes back.
    async fn toggle(&mut self, i: usize) {
        if self.devices[i].live.is_none() {
            self.back(i).await;
        } else if self.on().len() > 1 {
            self.off(i).await;
        }
    }

    /// Text `n`: from B1, or from a device of A that is on (the seed chooses).
    async fn send(&mut self, n: u8) {
        let on = self.on();
        let key = if self.rng.chance(50) {
            let to = self.live(on[0]);
            let key = send(&self.b1, to, n, &format!("text {n} from B")).await.key;
            self.from_b.insert(key);
            key
        } else {
            let from = on[self.rng.below(on.len() as u64) as usize];
            send(self.live(from), &self.b1, n, &format!("text {n} from A"))
                .await
                .key
        };
        self.sent.push(key);
    }

    /// A device of A that is on reads B's oldest unread texts (one to all of them).
    async fn mark_read(&mut self) -> bool {
        let unread: Vec<MessageKey> = self
            .sent
            .iter()
            .filter(|k| self.from_b.contains(k) && !self.read.contains(k))
            .copied()
            .collect();
        if unread.is_empty() {
            return false;
        }
        let on = self.on();
        let reader = on[self.rng.below(on.len() as u64) as usize];
        let count = 1 + self.rng.below(unread.len() as u64) as usize;
        let keys = unread[..count].to_vec();
        self.live(reader)
            .account
            .messages()
            .mark_read(self.conv, keys.clone())
            .await
            .unwrap();
        self.read.extend(keys);
        self.reads += 1;
        true
    }

    /// B1 adds B2 while at least one device of A is off; the devices that are on apply it.
    async fn commit(&mut self) {
        if self.on().len() == self.devices.len() {
            let i = self.rng.below(self.devices.len() as u64) as usize;
            self.off(i).await;
        }
        let b = self.b1.peer;
        let peer = self.live(self.on()[0]).peer;
        let epoch = self
            .b1
            .account
            .storage_test_fixture()
            .messages()
            .await
            .unwrap()
            .mls_commit(
                self.conv,
                peer,
                vec![mls_detached_key_package(b, [B2; 32])],
                vec![],
                NOW * 1000,
            )
            .await
            .unwrap()
            .expect("an own Commit");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            let mut applied = true;
            for i in self.on() {
                applied =
                    applied && state(self.live(i), self.conv).await.group_epoch == Some(epoch);
            }
            if applied {
                break;
            }
            assert!(tokio::time::Instant::now() < deadline, "Commit not applied");
            for f in self.running() {
                f.clock.advance_monotonic_ms(1_000);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        self.epoch = Some(epoch);
    }
}

/// The seeded run (see the file comment). Afterwards every device is on, and each:
/// - shows every text once, in the order they were sent (also B1);
/// - has each of B's texts read exactly when some device of A read it (and B1 shows it `Read`);
/// - was notified of exactly the texts it first stored while they were unread everywhere, also
///   those a run stored and then died mid-page (the next run notifies them, Ruling 35);
///   nothing it stored after the read;
/// - is on B1's epoch after the Commit, and lacks nothing (`missing` 0).
#[tokio::test]
async fn churning_devices_converge_in_order_with_one_notification_per_unread_text() {
    let (a, b1, net, conv) = accounts(&[A1, A2, A3], &[B1, B2]).await;
    let names = ["A1", "A2", "A3"];
    let mut devices = Vec::new();
    for (name, f) in names.into_iter().zip(a) {
        devices.push(Device {
            name,
            id: installation(&f).await,
            live: Some(f),
            dir: None,
            have: BTreeSet::new(),
            expected: 0,
            notified: 0,
        });
    }
    let mut run = Run {
        devices,
        b1,
        net,
        conv,
        rng: Rng(SEED),
        sent: Vec::new(),
        from_b: BTreeSet::new(),
        read: BTreeSet::new(),
        reads: 0,
        halts: 0,
        toggles: 0,
        halted_texts: 0,
        epoch: None,
    };
    let started = Instant::now();
    for n in 1..=MESSAGES {
        if run.rng.chance(TOGGLE) {
            let i = run.rng.below(run.devices.len() as u64) as usize;
            run.toggle(i).await;
            // A device that came back has caught up before anyone sends (Lamport order is the
            // order sent).
            run.settle().await;
        }
        if n == COMMIT_AT {
            run.commit().await;
        }
        run.send(n).await;
        run.settle().await;
        // Reads spread over the run; the last steps read whatever is left to read.
        let left = usize::from(MESSAGES - n);
        if run.reads < READS
            && (run.rng.chance(25) || left < READS - run.reads)
            && run.mark_read().await
        {
            run.settle().await;
        }
    }
    for i in 0..run.devices.len() {
        if run.devices[i].live.is_none() {
            run.back(i).await;
        }
    }
    run.settle().await;
    quiet(&run.running()).await;
    println!(
        "seed {SEED:#x}: {} texts ({} from B), {} reads, {} toggles, {} mid-page deaths storing {} texts, {:.1} s",
        run.sent.len(),
        run.from_b.len(),
        run.reads,
        run.toggles,
        run.halts,
        run.halted_texts,
        started.elapsed().as_secs_f64()
    );
    assert_eq!(run.reads, READS, "the seed reads {READS} times");
    assert_eq!(run.halts, HALTS, "the seed dies mid-page {HALTS} times");
    assert!(run.epoch.is_some(), "the Commit ran");

    let b_order: Vec<MessageKey> = everything(&run.b1, conv)
        .await
        .iter()
        .map(|m| m.key)
        .collect();
    assert_eq!(b_order, run.sent, "B1: sent order, each once");
    let mut expected = 0;
    let mut notified_all = 0;
    for device in &run.devices {
        let f = device.live.as_ref().unwrap();
        let shown: Vec<MessageKey> = everything(f, conv).await.iter().map(|m| m.key).collect();
        assert_eq!(shown, run.sent, "{}: sent order, each once", device.name);
        for key in &run.from_b {
            assert_eq!(
                seen(f, *key).await,
                run.read.contains(key),
                "{}: read state of {key:?}",
                device.name
            );
        }
        assert_eq!(
            state(f, conv).await.group_epoch,
            state(&run.b1, conv).await.group_epoch,
            "{}: epoch",
            device.name
        );
        assert_eq!(f.account.mailbox_test_counters().await.unwrap().missing, 0);
        let notified = device.notified + notified(f);
        println!(
            "{}: notified of {notified} new texts, expected {}",
            device.name, device.expected
        );
        assert_eq!(notified, device.expected, "{}: notifications", device.name);
        expected += device.expected;
        notified_all += notified;
    }
    assert_eq!(notified_all, expected);
    assert!(expected > 0);
}

/// Waits (clocks moving) for `cond`, up to `limit`.
macro_rules! wait {
    ($devices:expr, $limit:expr, $cond:expr) => {{
        let deadline = tokio::time::Instant::now() + $limit;
        loop {
            if $cond {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "timed out: {}",
                stringify!($cond)
            );
            for d in $devices {
                d.clock.advance_monotonic_ms(1_000);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }};
}

async fn db_bytes(f: &Fixture) -> u64 {
    f.account.storage_test_fixture().db_bytes().await.unwrap()
}

/// Spec 11 measurements, printed (run in Release:
/// `cargo +1.92.0 test --release --locked -p chat_core --features test-hooks --test mailbox_resilience -- --ignored thousand_entries_transfer --nocapture`,
/// with `CARGO_PROFILE_RELEASE_DEBUG_ASSERTIONS=true`: the test hooks exist only with debug
/// assertions). A = [A1, A2], B = [B1]. While A1 is off, B1 sends 1,000 texts of 1 KiB to A2.
/// - Bytes per entry: the 1,000 entries A2 logged are recorded again in the log of an idle core
///   of a third account; its `page_count × page_size` grows by that much (rows and indexes).
///   A2's whole database growth per text is printed alongside.
/// - Transfer: A1 starts, can reach A2 and B1, and syncs; the time until it stores all 1,000
///   (one notification for them) and has processed A2's log.
#[tokio::test]
#[ignore = "measurement; run in Release with --ignored"]
async fn thousand_entries_transfer() {
    const TEXTS: usize = 1_000;
    let (mut a, b1, net, conv) = accounts(&[A1, A2], &[B1]).await;
    let a2 = a.pop().unwrap();
    let a1 = a.pop().unwrap();
    let a1_id = installation(&a1).await;
    let dir = stop(a1);
    for f in [&a2, &b1] {
        f.account.mls_test_close_session(a1_id).await.unwrap();
    }
    let a2_before = db_bytes(&a2).await;
    let sending = Instant::now();
    let mut ids = BTreeSet::new();
    for batch in 0..TEXTS / 50 {
        for k in 0..50 {
            let n = (batch * 50 + k) as u16;
            let mut id = [0xa6; 16];
            id[..2].copy_from_slice(&n.to_be_bytes());
            ids.insert(MessageId(id));
            let body = format!("{n:04} {}", "x".repeat(1019));
            assert_eq!(body.len(), 1024);
            b1.account
                .messages()
                .send_text(a2.peer, MessageId(id), body)
                .await
                .unwrap();
        }
        let want = ((batch + 1) * 50) as u64;
        wait!(
            [&a2, &b1],
            Duration::from_secs(300),
            unread(&a2, conv).await == want
        );
    }
    let sent_in = sending.elapsed();
    let a2_after = db_bytes(&a2).await;
    let entries: Vec<Vec<u8>> = mailbox(&a2)
        .await
        .entries
        .into_iter()
        .filter(|e| e.kind == 1 && e.msg_id.is_some_and(|id| ids.contains(&id)))
        .map(|e| e.body)
        .collect();
    assert_eq!(entries.len(), TEXTS, "A2 logged each text once");
    let body_bytes: usize = entries.iter().map(Vec::len).sum();

    // The log alone: the same entries in an idle core's log.
    let rc = record(3, &[16], 0);
    publish(&net, 3, &rc);
    let idle = device(3, 16, &rc, &net).await;
    let b1_id = installation(&b1).await;
    let idle_before = db_bytes(&idle).await;
    let store = idle.account.storage_test_fixture();
    for body in entries {
        store
            .mailbox_record_as(conv, b1.peer, b1_id, body, NOW * 1000)
            .await
            .unwrap()
            .expect("recorded");
    }
    let idle_after = db_bytes(&idle).await;

    let starting = Instant::now();
    let a1 = start(dir, net.clone()).await;
    link(&a1, &a2).await;
    link(&a1, &b1).await;
    let started_in = starting.elapsed();
    let syncing = Instant::now();
    a1.account.mailbox_sync_now().await.unwrap();
    wait!(
        [&a1, &a2, &b1],
        Duration::from_secs(600),
        unread(&a1, conv).await == TEXTS as u64
    );
    let stored_in = syncing.elapsed();
    wait!(
        [&a1, &a2, &b1],
        Duration::from_secs(60),
        caught_up(&a1, &a2).await
    );
    let caught_up_in = syncing.elapsed();
    let texts: Vec<MessageView> = everything(&a1, conv)
        .await
        .into_iter()
        .filter(|m| ids.contains(&m.key.id))
        .collect();
    assert_eq!(texts.len(), TEXTS);
    let mut order = texts.iter().map(|m| m.key.id).collect::<Vec<_>>();
    let sorted: Vec<MessageId> = ids.iter().copied().collect();
    assert_eq!(order, sorted, "in the order sent");
    order.clear();
    let new: u64 = notified(&a1);
    println!(
        "mailbox_store: {} B per entry ({} entries, {} B of bodies, {:.0} B per body); \
         A2 database grew {} B per text",
        (idle_after - idle_before) / TEXTS as u64,
        TEXTS,
        body_bytes,
        body_bytes as f64 / TEXTS as f64,
        (a2_after - a2_before) / TEXTS as u64,
    );
    println!(
        "transfer of {TEXTS}: all stored in {:.3} s, log processed in {:.3} s ({:.2} ms per entry); \
         core start {:.3} s; B1 sent them in {:.1} s; A1 notified of {new} in {} event(s)",
        stored_in.as_secs_f64(),
        caught_up_in.as_secs_f64(),
        caught_up_in.as_secs_f64() * 1000.0 / TEXTS as f64,
        started_in.as_secs_f64(),
        sent_in.as_secs_f64(),
        notify_count(&a1),
    );
}
