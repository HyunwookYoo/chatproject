// Several installations per account on the M4 message network harness: one `Fixture` (own core,
// storage and endpoint) per installation, records shared through one `Backend`.
include!("message_network.rs");

use chat_core::account::GroupState;
use chat_core::storage_fixture::{ConversationId, MlsState};

/// Account seeds.
const A: u8 = 1;
const B: u8 = 2;
/// Installation seeds.
const A1: u8 = 11;
const A2: u8 = 13;
const B1: u8 = 12;
const B2: u8 = 14;

/// The record of `account` listing `devices`; `step` makes a newer one (higher sequence).
fn record(account: u8, devices: &[u8], step: u64) -> SignedPacket {
    packet(
        account,
        devices,
        account,
        NOW + step,
        NOW + 600 + step,
        (NOW + step) * 1_000_000,
    )
}

fn peer_of(account: u8) -> PeerId {
    derive_account(&Secret32::from([account; 32]))
        .unwrap()
        .peer_id()
}

fn backend(records: &[(u8, SignedPacket)]) -> Arc<Backend> {
    let backend = Arc::new(Backend::default());
    for (account, packet) in records {
        publish(&backend, *account, packet);
    }
    backend
}

fn publish(backend: &Backend, account: u8, packet: &SignedPacket) {
    backend
        .packets
        .lock()
        .unwrap()
        .insert(peer_of(account), packet.clone());
}

async fn device(account: u8, seed: u8, own: &SignedPacket, backend: &Arc<Backend>) -> Fixture {
    Fixture::new(account, seed, own, backend.clone()).await
}

/// Each side may dial the other.
async fn link(a: &Fixture, b: &Fixture) {
    a.route(b).await;
    b.route(a).await;
}

/// `a` accepts an invite created by `b`.
async fn befriend(a: &Fixture, b: &Fixture) {
    contacts(a, b).await;
}

async fn installation(f: &Fixture) -> chat_core::account::InstallationId {
    f.view().await.installation.unwrap()
}

async fn conversation(a: &Fixture, b: &Fixture) -> ConversationId {
    let messages = a.account.storage_test_fixture().messages().await.unwrap();
    messages.open_dm(a.peer, b.peer).await.unwrap().id
}

async fn state(f: &Fixture, conversation: ConversationId) -> MlsState {
    f.account
        .storage_test_fixture()
        .mls_state(conversation)
        .await
        .unwrap()
}

async fn ensure(f: &Fixture, peer: PeerId) -> GroupState {
    f.account.mls_test_ensure_group(peer).await.unwrap()
}

/// Polls `$cond` while every listed device's clock moves one second per round, so retry
/// back-offs pass quickly; fails after 60 real seconds.
macro_rules! settle {
    ($devices:expr, $cond:expr) => {{
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        loop {
            if $cond {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "did not settle: {}",
                stringify!($cond)
            );
            for d in $devices {
                d.clock.advance_monotonic_ms(1_000);
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }};
}

/// Both devices hold the same group on the same branch of the same epoch (equal epoch
/// authenticators), and agree on its first-Commit hash.
async fn agreed(a: &Fixture, b: &Fixture, conversation: ConversationId) -> bool {
    let (x, y) = (state(a, conversation).await, state(b, conversation).await);
    x.epoch_authenticator.is_some()
        && x.epoch_authenticator == y.epoch_authenticator
        && x.group_epoch == y.group_epoch
        && x.group_id == y.group_id
        && x.first_commit_hash == y.first_commit_hash
}

/// `from` seals a new message for the conversation and `to` opens it: proves both hold the same
/// group secrets. Returns the reply `to` gave. Automatic M4 delivery on `from` is held.
async fn talk(from: &Fixture, to: &Fixture, conversation: ConversationId, n: u8) -> MlsReply {
    from.account
        .message_test_hold_scheduler(true)
        .await
        .unwrap();
    let messages = from
        .account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap();
    // Between two devices of one account the message goes to the conversation's peer.
    let peer = if to.peer != from.peer {
        to.peer
    } else {
        let page = messages.list_conversations(None, 50).await.unwrap();
        page.items
            .iter()
            .find(|c| c.id == conversation)
            .unwrap()
            .peer
    };
    let sent = messages
        .enqueue(
            peer,
            MessageId([n; 16]),
            format!("mls text {n}"),
            NOW * 1000,
        )
        .await
        .unwrap();
    let fixture = from.account.storage_test_fixture();
    fixture.mls_promote_outbox(sent.key).await.unwrap();
    messages
        .mls_seal(sent.key, NOW * 1000)
        .await
        .unwrap()
        .unwrap();
    let wire = fixture.mls_outbox(sent.key).await.unwrap().wire;
    let (id, ciphertext) = mls_app_from_wire(&wire).unwrap();
    assert_eq!(id, conversation);
    to.account
        .storage_test_fixture()
        .messages()
        .await
        .unwrap()
        .mls_open_app(
            from.peer,
            installation(from).await,
            conversation,
            ciphertext,
            NOW * 1000,
        )
        .await
        .unwrap()
}

fn acked(reply: &MlsReply) -> bool {
    matches!(reply, MlsReply::Ack { .. })
}

use chat_core::storage_fixture::{MlsReply, mls_app_from_wire};
