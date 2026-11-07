// MLS messaging between real cores: the MLS network harness plus helpers shared by the
// Task 8 tests (`mls_messages.rs`, `mls_outbox.rs`).
include!("mls_network.rs");

use chat_core::storage_fixture::{
    Fixture as SqlFixture, MessageKey, MessageView, mls_commit_from_wire, mls_detached_key_package,
};

/// Text that must never appear in any frame on the network.
const SENTINEL: &str = "sentinel-plaintext-7c1f";

/// The kind of an account envelope `[1, kind, inner]`.
fn kind(frame: &[u8]) -> u8 {
    assert_eq!(frame[..2], [0x83, 0x01], "not an account envelope");
    frame[2]
}
/// Whether the frame is an M4 kind 16 text (codec 1 plaintext).
fn is_plaintext_text(frame: &[u8]) -> bool {
    if kind(frame) != 16 {
        return false;
    }
    let mut d = minicbor::Decoder::new(frame);
    d.array().unwrap();
    d.u8().unwrap();
    d.u8().unwrap();
    let inner = d.bytes().unwrap();
    matches!(
        chat_core::storage_fixture::decode_message(inner),
        Ok(chat_core::storage_fixture::MessageFrame::Text(_))
    )
}
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
async fn frames(f: &Fixture) -> Vec<Vec<u8>> {
    f.account.message_test_sent_frames().await.unwrap()
}
/// The MLS ciphertexts of the `App` frames `f` sent, in order.
async fn app_ciphertexts(f: &Fixture) -> Vec<Vec<u8>> {
    frames(f)
        .await
        .iter()
        .filter_map(|frame| mls_app_from_wire(frame).map(|(_, ct)| ct))
        .collect()
}

/// A1 and B1, contacts of each other, with an authenticated session.
async fn setup(b_devices: &[u8]) -> (Fixture, Fixture, Arc<Backend>, ConversationId) {
    let (ra, rb) = (record(A, &[A1], 0), record(B, b_devices, 0));
    let net = backend(&[(A, ra.clone()), (B, rb.clone())]);
    let a1 = device(A, A1, &ra, &net).await;
    let b1 = device(B, B1, &rb, &net).await;
    link(&a1, &b1).await;
    befriend(&a1, &b1).await;
    befriend(&b1, &a1).await;
    a1.account.connect_account(b1.peer).await.unwrap();
    let conv = conversation(&a1, &b1).await;
    conversation(&b1, &a1).await;
    (a1, b1, net, conv)
}

async fn send(from: &Fixture, to: &Fixture, n: u8, body: &str) -> MessageView {
    from.account
        .messages()
        .send_text(to.peer, MessageId([n; 16]), body.into())
        .await
        .unwrap()
}
async fn page(f: &Fixture, conv: ConversationId) -> Vec<MessageView> {
    f.account
        .messages()
        .load_messages(conv, None, 50)
        .await
        .unwrap()
        .items
}
async fn message(f: &Fixture, key: MessageKey) -> Option<MessageView> {
    page(f, key.conversation)
        .await
        .into_iter()
        .find(|m| m.key == key)
}
async fn delivery(f: &Fixture, key: MessageKey) -> DeliveryState {
    message(f, key).await.unwrap().state
}
async fn outbox(f: &Fixture, key: MessageKey) -> chat_core::storage_fixture::MlsOutbox {
    f.account
        .storage_test_fixture()
        .mls_outbox(key)
        .await
        .unwrap()
}
async fn receipt_jobs(f: &Fixture) -> u64 {
    f.account
        .storage_test_fixture()
        .message_counts()
        .await
        .unwrap()
        .2
}

/// A and B share a group (made by `ensure_group`, both on the same epoch).
async fn grouped(a: &Fixture, b: &Fixture, conv: ConversationId) {
    ensure(a, b.peer).await;
    settle!(
        [a, b],
        agreed(a, b, conv).await && state(a, conv).await.deliveries.is_empty()
    );
}

/// `n` own Commits on `a` (adding and removing B's never-online device B2 in turn), each
/// delivered to B1 before the next.
async fn commits(a: &Fixture, b: &Fixture, conv: ConversationId, n: usize) {
    let messages = a.account.storage_test_fixture().messages().await.unwrap();
    let b2 = derive_device(Secret32::from([B2; 32]))
        .unwrap()
        .installation_id();
    for i in 0..n {
        let (add, remove) = if i % 2 == 0 {
            (vec![mls_detached_key_package(b.peer, [B2; 32])], vec![])
        } else {
            (vec![], vec![b2])
        };
        let epoch = messages
            .mls_commit(conv, b.peer, add, remove, NOW * 1000)
            .await
            .unwrap()
            .expect("an own Commit");
        settle!([a, b], state(b, conv).await.group_epoch == Some(epoch));
        settle!([a, b], !state(a, conv).await.own_commit_unacked.unwrap());
    }
}

/// Stops a device's core and returns its data directory.
fn stop(f: Fixture) -> tempfile::TempDir {
    let Fixture {
        _sql,
        _dir,
        slot,
        account,
        ..
    } = f;
    let observer = account.storage_test_fixture();
    drop(slot);
    drop(account);
    drop(_sql);
    assert_eq!(observer.live_connections(), 0);
    _dir
}

/// Starts a core on an existing data directory (the account `A`).
async fn start(dir: tempfile::TempDir, backend: Arc<Backend>) -> Fixture {
    let slot = CoreSlot::new();
    let bus = EventBus::new();
    let events = Arc::new(Events::default());
    bus.subscribe(events.clone());
    slot.start(
        CoreConfig {
            data_dir: dir.path().into(),
        },
        &bus,
    )
    .unwrap();
    slot.network().unwrap().configure_local(None).await.unwrap();
    let clock = Arc::new(FakeClock::new(NOW, 0));
    let account = slot
        .open_account_with_services(
            support::protector(),
            AccountServices::new(backend, clock.clone()),
        )
        .unwrap();
    lifecycle_idle_handle(&account).await;
    account.snapshot().await.unwrap();
    let f = Fixture {
        _sql: sql_store::Store::from_fixture(account.storage_test_fixture()),
        _dir: dir,
        slot,
        bus,
        events,
        account,
        peer: peer_of(A),
        clock,
    };
    online(&f).await;
    f
}

async fn lifecycle_idle_handle(account: &AccountHandle) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(account.lifecycle_status(), 0);
}
async fn online(f: &Fixture) {
    f.slot
        .network()
        .unwrap()
        .execute(NetworkOp::Start)
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let NetworkReply::Info(info) = f
                .slot
                .network()
                .unwrap()
                .execute(NetworkOp::Info)
                .await
                .unwrap()
            else {
                panic!()
            };
            if info.state == NetworkState::Online {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
