#![cfg(all(feature = "test-hooks", debug_assertions))]
use chat_core::account::{
    AccountHandle, AccountPhase, PublishIntent, RecoveryDisplay, Secret32, StoreId,
    decode_recovery, derive_account,
    storage::{KeyProtector, StoredPlaintext},
};
use chat_core::{CoreConfig, CoreError, CoreSlot, ErrorCode, EventBus, NetworkOp, NetworkReply};
use std::time::Duration;
#[allow(dead_code)]
#[path = "support/account_store.rs"]
mod support;
#[path = "support/account_services.rs"]
mod services;

async fn open(path: &std::path::Path) -> (CoreSlot, AccountHandle) {
    open_with_bus(path, &EventBus::new()).await
}
async fn open_with_bus(path: &std::path::Path, bus: &EventBus) -> (CoreSlot, AccountHandle) {
    let slot = CoreSlot::new();
    slot.start(
        CoreConfig {
            data_dir: path.into(),
        },
        bus,
    )
    .unwrap();
    slot.network().unwrap().configure_local(None).await.unwrap();
    let account = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match slot.open_account_with_services(support::protector(), services::offline()) {
                Ok(account) => break account,
                Err(e) if e.code == ErrorCode::WriterBusy => tokio::task::yield_now().await,
                Err(e) => panic!("{e}"),
            }
        }
    })
    .await
    .unwrap();
    settled(&account).await;
    (slot, account)
}
async fn settled(account: &AccountHandle) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(account.lifecycle_status(), 0);
}
fn answers(display: &RecoveryDisplay) -> [String; 3] {
    display
        .positions
        .map(|p| display.words[usize::from(p - 1)].clone())
}
fn fixture() -> String {
    chat_core::account::encode_recovery(&Secret32::from([0; 32])).unwrap()
}

#[tokio::test]
async fn three_positions_required_then_phrase_absent_on_disk() {
    #[derive(Default)]
    struct Events(std::sync::Mutex<Vec<chat_core::CoreEvent>>);
    impl chat_core::EventSink for Events {
        fn send(&self, event: chat_core::CoreEvent) -> bool {
            self.0.lock().unwrap().push(event);
            true
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let bus = EventBus::new();
    let events = std::sync::Arc::new(Events::default());
    bus.subscribe(events.clone());
    let (_slot, account) = open_with_bus(dir.path(), &bus).await;
    let display = account.begin_create().await.unwrap();
    assert_eq!(display.words.len(), 24);
    let mut positions = display.positions;
    positions.sort();
    assert!(positions[0] >= 1 && positions[2] <= 24);
    assert!(positions[0] < positions[1] && positions[1] < positions[2]);
    let entropy = decode_recovery(&display.words.join(" ")).unwrap();
    let peer = derive_account(&entropy).unwrap().peer_id();
    assert!(account.snapshot().await.unwrap().publish_intents.is_empty());
    assert_eq!(
        account
            .confirm_create(display.flow, [String::new(), String::new(), String::new()])
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidRecovery
    );
    account
        .confirm_create(display.flow, answers(&display))
        .await
        .unwrap();
    let view = account.snapshot().await.unwrap();
    assert_eq!(view.phase, AccountPhase::LocalReady);
    assert_eq!(view.public_peer_id, Some(peer));
    assert_eq!(view.publish_intents, vec![PublishIntent::LocalCreate]);
    let cert = public_cert(&account).await;
    chat_core::account::verify_cert(&cert, peer).unwrap();
    assert_eq!(view.devices[0].installation_id, cert.installation_id);
    let debug = format!("{view:?}");
    let phrase = display.words.join(" ");
    assert!(!debug.contains(&phrase));
    let events = serde_json::to_string(&*events.0.lock().unwrap()).unwrap();
    assert!(!events.contains(&phrase));
    assert!(!events.contains("words"));
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let path = entry.unwrap().path();
        if path.is_file() && path.file_name().unwrap() != ".account.lock" {
            let bytes = std::fs::read(path).unwrap();
            assert!(!bytes.windows(phrase.len()).any(|w| w == phrase.as_bytes()));
            assert!(!bytes.windows(32).any(|w| w == entropy.as_bytes()));
        }
    }
}

#[tokio::test]
async fn cancel_before_confirm_no_publish() {
    let dir = tempfile::tempdir().unwrap();
    let (_slot, account) = open(dir.path()).await;
    let display = account.begin_create().await.unwrap();
    account.cancel_create(display.flow).await.unwrap();
    let view = account.snapshot().await.unwrap();
    assert_eq!(view.phase, AccountPhase::Unconfigured);
    assert!(view.publish_intents.is_empty());
    assert!(!dir.path().join("account.bin").exists());
    assert!(
        account
            .confirm_create(display.flow, answers(&display))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn positions_are_sorted_answers_normalized_and_revision_advances() {
    let dir = tempfile::tempdir().unwrap();
    let (_slot, account) = open(dir.path()).await;
    let initial = account.snapshot().await.unwrap().revision;
    let display = account.begin_create().await.unwrap();
    assert!(display.positions.windows(2).all(|w| w[0] < w[1]));
    let creating = account.snapshot().await.unwrap().revision;
    assert!(creating > initial);
    let mut oversized = answers(&display);
    oversized[0] = " ".repeat(65);
    assert_eq!(
        account
            .confirm_create(display.flow, oversized)
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidRecovery
    );
    let normalized = answers(&display).map(|w| format!("  {}  ", w.to_uppercase()));
    account
        .confirm_create(display.flow, normalized)
        .await
        .unwrap();
    assert!(account.snapshot().await.unwrap().revision > creating);
}

#[tokio::test]
async fn recovery_same_peer_new_device_no_contacts() {
    let original = tempfile::tempdir().unwrap();
    let (_original_slot, original_account) = open(original.path()).await;
    let display = original_account.begin_create().await.unwrap();
    original_account
        .confirm_create(display.flow, answers(&display))
        .await
        .unwrap();
    let original_view = original_account.snapshot().await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (slot, account) = open(dir.path()).await;
    let candidate = account.prepare_candidate().await.unwrap();
    let instance = slot.info().unwrap().instance_id;
    assert!(account.recover(fixture(), false).await.is_err());
    account
        .recover(display.words.join(" "), true)
        .await
        .unwrap();
    let view = account.snapshot().await.unwrap();
    assert_eq!(view.public_peer_id, original_view.public_peer_id);
    assert_ne!(view.installation, Some(candidate.installation_id));
    assert_ne!(view.installation, original_view.installation);
    assert_ne!(
        view.devices[0].transport_public_key,
        original_view.devices[0].transport_public_key
    );
    chat_core::account::verify_cert(&public_cert(&account).await, original_view.public_peer_id.unwrap())
        .unwrap();
    assert!(view.contacts.is_empty());
    assert_eq!(view.phase, AccountPhase::PublishPending);
    assert_eq!(view.publish_intents, vec![PublishIntent::Replacement]);
    assert_eq!(slot.info().unwrap().instance_id, instance);
    let NetworkReply::Info(info) = slot
        .network()
        .unwrap()
        .execute(NetworkOp::Info)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_ne!(info.endpoint_id, Some(candidate.endpoint_id));
    slot.network()
        .unwrap()
        .execute(NetworkOp::Stop)
        .await
        .unwrap();
}

#[tokio::test]
async fn active_folder_overwrite_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let (_slot, account) = open(dir.path()).await;
    account.recover(fixture(), true).await.unwrap();
    let before = std::fs::read(dir.path().join("account.bin")).unwrap();
    assert_eq!(
        account.begin_create().await.err().unwrap().code,
        ErrorCode::AccountExists
    );
    assert_eq!(
        account.recover(fixture(), true).await.unwrap_err().code,
        ErrorCode::AccountExists
    );
    assert_eq!(
        account.prepare_candidate().await.unwrap_err().code,
        ErrorCode::AccountExists
    );
    assert_eq!(
        std::fs::read(dir.path().join("account.bin")).unwrap(),
        before
    );
}

#[tokio::test]
async fn lost_flow_cannot_approve_new_flow() {
    let dir = tempfile::tempdir().unwrap();
    let (_slot, account) = open(dir.path()).await;
    let old = account.begin_create().await.unwrap();
    let new = account.begin_create().await.unwrap();
    assert_ne!(old.flow, new.flow);
    assert_ne!(old.words, new.words);
    assert!(
        account
            .confirm_create(old.flow, answers(&old))
            .await
            .is_err()
    );
    assert!(account.cancel_create(old.flow).await.is_err());
    account
        .confirm_create(new.flow, answers(&new))
        .await
        .unwrap();
}

#[tokio::test]
async fn established_restart_reconstructs_signing_seed_and_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let (slot, account) = open(dir.path()).await;
    let display = account.begin_create().await.unwrap();
    account
        .confirm_create(display.flow, answers(&display))
        .await
        .unwrap();
    let before = account.snapshot().await.unwrap();
    let net = slot.network().unwrap();
    let NetworkReply::Info(info) = net.execute(NetworkOp::Info).await.unwrap() else {
        panic!()
    };
    let instance = slot.info().unwrap().instance_id;
    net.execute(NetworkOp::Stop).await.unwrap();
    drop(account);
    drop(net);
    drop(slot);
    let (slot, account) = open(dir.path()).await;
    let after = account.snapshot().await.unwrap();
    assert_eq!(after.revision, 1); // A fresh actor starts a new public stream epoch.
    assert_eq!(after, chat_core::account::AccountView { revision: after.revision, ..before });
    slot.network()
        .unwrap()
        .execute(NetworkOp::Start)
        .await
        .unwrap();
    let NetworkReply::Info(restarted) = slot
        .network()
        .unwrap()
        .execute(NetworkOp::Info)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(info.endpoint_id, restarted.endpoint_id);
    assert_ne!(slot.info().unwrap().instance_id, instance);
    assert_eq!(
        account.recover(fixture(), true).await.unwrap_err().code,
        ErrorCode::AccountExists
    );
    slot.network()
        .unwrap()
        .execute(NetworkOp::Stop)
        .await
        .unwrap();
}

#[tokio::test]
async fn suspended_commands_cannot_write_or_handoff_phrase() {
    let dir = tempfile::tempdir().unwrap();
    let (_slot, account) = open(dir.path()).await;
    let display = account.begin_create().await.unwrap();
    account.request_suspend();
    assert_eq!(
        account.begin_create().await.err().unwrap().code,
        ErrorCode::StorageLocked
    );
    assert_eq!(
        account
            .confirm_create(display.flow, answers(&display))
            .await
            .unwrap_err()
            .code,
        ErrorCode::StorageLocked
    );
    assert_eq!(
        account.recover(fixture(), true).await.unwrap_err().code,
        ErrorCode::StorageLocked
    );
    assert_eq!(
        account.cancel_create(display.flow).await.unwrap_err().code,
        ErrorCode::StorageLocked
    );
    settled(&account).await;
    assert_eq!(
        account.snapshot().await.unwrap().phase,
        AccountPhase::Locked
    );
    assert!(!dir.path().join("account.bin").exists());
    account.request_resume();
    settled(&account).await;
    assert!(
        account
            .confirm_create(display.flow, answers(&display))
            .await
            .is_err()
    );
}

#[tokio::test]
async fn queued_creation_cannot_cross_a_lifecycle_cycle() {
    let dir = tempfile::tempdir().unwrap();
    let (slot, account) = open(dir.path()).await;
    let net = slot.network().unwrap();
    let held = net.hold_next_bind_for_test();
    let worker = account.clone();
    let preparing = tokio::spawn(async move { worker.prepare_candidate().await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let NetworkReply::Info(info) = net.execute(NetworkOp::Info).await.unwrap() else {
                panic!()
            };
            if info.state == chat_core::NetworkState::Starting {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let begin = account.begin_create();
    tokio::pin!(begin);
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(begin.as_mut().poll(cx).is_pending()))
            .await
    );
    account.request_suspend();
    account.request_resume();
    assert!(preparing.await.unwrap().is_err());
    assert_eq!(begin.await.err().unwrap().code, ErrorCode::StorageLocked);
    settled(&account).await;
    drop(held);
    assert_eq!(
        account.snapshot().await.unwrap().phase,
        AccountPhase::Candidate
    );
    assert!(!account.begin_create().await.unwrap().words.is_empty());
}

#[tokio::test]
async fn commit_error_gates_mutations_until_disk_is_reconciled() {
    let dir = tempfile::tempdir().unwrap();
    let (slot, account) = open(dir.path()).await;
    account.prepare_candidate().await.unwrap();
    let before = std::fs::read(dir.path().join("account.bin")).unwrap();
    // A real filesystem failure at the atomic-write boundary.
    let obstruction = dir.path().join(".account.bin.tmp");
    std::fs::create_dir(&obstruction).unwrap();
    assert!(account.recover(fixture(), true).await.is_err());
    assert_eq!(account.lifecycle_status(), 2);
    assert_eq!(
        account.begin_create().await.err().unwrap().code,
        ErrorCode::StorageLocked
    );
    assert_eq!(
        account.recover(fixture(), true).await.unwrap_err().code,
        ErrorCode::StorageLocked
    );
    assert_eq!(
        slot.network()
            .unwrap()
            .execute(NetworkOp::Start)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    assert_eq!(
        std::fs::read(dir.path().join("account.bin")).unwrap(),
        before
    );
    std::fs::remove_dir(obstruction).unwrap();
    assert_eq!(account.request_resume(), 2);
    account.request_suspend();
    settled(&account).await;
    account.request_resume();
    settled(&account).await;
    account.recover(fixture(), true).await.unwrap();
    assert_eq!(
        account.snapshot().await.unwrap().phase,
        AccountPhase::PublishPending
    );
    slot.network()
        .unwrap()
        .execute(NetworkOp::Stop)
        .await
        .unwrap();
}

async fn public_cert(account: &AccountHandle) -> chat_core::account::InstallationCert {
    use chat_core::account::clock::Clock;
    let invite = account.create_invite(None).await.unwrap();
    chat_core::account::parse_invite(&invite, chat_core::account::clock::SystemClock::default().unix_seconds()).unwrap().issuing_cert
}
