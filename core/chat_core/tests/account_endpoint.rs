#![cfg(all(feature = "test-hooks", debug_assertions))]
use chat_core::account::{
    Secret32, StoreId,
    storage::{KeyProtector, Store, StoredPlaintext},
};
use chat_core::{
    CoreConfig, CoreError, CoreSlot, ErrorCode, EventBus, NetworkHandle, NetworkInfo, NetworkOp,
    NetworkReply, NetworkState,
};
use std::time::Duration;
#[allow(dead_code)]
#[path = "support/account_store.rs"]
mod support;

fn core(dir: &std::path::Path) -> CoreSlot {
    let core = CoreSlot::new();
    core.start(
        CoreConfig {
            data_dir: dir.into(),
        },
        &EventBus::new(),
    )
    .unwrap();
    core
}
async fn info(net: &NetworkHandle) -> NetworkInfo {
    let NetworkReply::Info(info) = net.execute(NetworkOp::Info).await.unwrap() else {
        panic!()
    };
    info
}
async fn online(net: &NetworkHandle) -> NetworkInfo {
    net.execute(NetworkOp::Start).await.unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let info = info(net).await;
            if info.state == NetworkState::Online {
                return info;
            }
            assert_ne!(info.state, NetworkState::Failed);
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}
async fn settled(account: &chat_core::account::AccountHandle) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(account.lifecycle_status(), 0);
}

// A5 supplies the Active-state half: A4 has only a real Candidate codec.
#[tokio::test]
async fn active_and_candidate_restart_keep_endpoint_new_instance() {
    let dir = tempfile::tempdir().unwrap();
    let first = core(dir.path());
    let net = first.network().unwrap();
    net.configure_local(None).await.unwrap();
    let diagnostic = online(&net).await.endpoint_id;
    let instance = first.info().unwrap().instance_id;
    let account = first.open_account(support::protector()).unwrap();
    let candidate = account.prepare_candidate().await.unwrap();
    assert_ne!(Some(candidate.endpoint_id.clone()), diagnostic);
    assert_eq!(
        info(&net).await.endpoint_id,
        Some(candidate.endpoint_id.clone())
    );
    net.execute(NetworkOp::Stop).await.unwrap();
    drop(account);
    drop(net);
    drop(first);
    // Runtime shutdown drops the Store actor asynchronously; ownership must really end.
    let second = core(dir.path());
    let net = second.network().unwrap();
    net.configure_local(None).await.unwrap();
    let account = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match second.open_account(support::protector()) {
                Ok(a) => break a,
                Err(e) if e.code == ErrorCode::WriterBusy => tokio::task::yield_now().await,
                Err(e) => panic!("{e}"),
            }
        }
    })
    .await
    .unwrap();
    settled(&account).await;
    assert_eq!(info(&net).await.state, NetworkState::Stopped);
    assert_eq!(online(&net).await.endpoint_id, Some(candidate.endpoint_id));
    assert_ne!(second.info().unwrap().instance_id, instance);
    net.execute(NetworkOp::Stop).await.unwrap();
}

#[tokio::test]
async fn transition_closes_old_probe_and_ignores_old_epoch() {
    let dir = tempfile::tempdir().unwrap();
    let slot = core(dir.path());
    let net = slot.network().unwrap();
    net.configure_local(None).await.unwrap();
    let old = online(&net).await;
    let remote = NetworkHandle::spawn(
        &tokio::runtime::Handle::current(),
        chat_core::transport::NetworkProfile::Local { relay: None },
        EventBus::new(),
    );
    let remote_info = online(&remote).await;
    net.execute(NetworkOp::Connect {
        target: remote_info.ticket.unwrap(),
    })
    .await
    .unwrap();
    let account = slot.open_account(support::protector()).unwrap();
    let candidate = account.prepare_candidate().await.unwrap();
    assert_ne!(Some(candidate.endpoint_id.clone()), old.endpoint_id);
    assert!(
        net.execute(NetworkOp::Send {
            peer_id: remote_info.endpoint_id.unwrap(),
            text: "obsolete".into()
        })
        .await
        .is_err()
    );
    assert!(info(&net).await.peers.is_empty());
    net.execute(NetworkOp::Stop).await.unwrap();
    remote.execute(NetworkOp::Stop).await.unwrap();
}

#[tokio::test]
async fn transition_busy_and_bind_failure_keeps_saved_key() {
    let dir = tempfile::tempdir().unwrap();
    let slot = core(dir.path());
    let net = slot.network().unwrap();
    net.configure_local(None).await.unwrap();
    let account = slot.open_account(support::protector()).unwrap();
    net.fail_next_bind_for_test();
    assert_eq!(
        account.prepare_candidate().await.unwrap_err().code,
        ErrorCode::TransportFailed
    );
    let bytes = std::fs::read(dir.path().join("account.bin")).unwrap();
    let saved_id = info(&net).await.endpoint_id;
    let candidate = account.prepare_candidate().await.unwrap();
    assert_eq!(saved_id, Some(candidate.endpoint_id));
    assert_eq!(
        std::fs::read(dir.path().join("account.bin")).unwrap(),
        bytes
    );
    net.execute(NetworkOp::Stop).await.unwrap();
}

#[tokio::test]
async fn suspend_blocks_immediately_and_failed_resume_keeps_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let slot = core(dir.path());
    let net = slot.network().unwrap();
    net.configure_local(None).await.unwrap();
    let account = slot.open_account(support::protector()).unwrap();
    account.prepare_candidate().await.unwrap();
    competing_writer(dir.path());
    assert_eq!(account.request_suspend(), 1);
    assert_eq!(
        net.execute(NetworkOp::Start).await.unwrap_err().code,
        ErrorCode::Busy
    );
    assert_eq!(
        account.prepare_candidate().await.unwrap_err().code,
        ErrorCode::StorageLocked
    );
    settled(&account).await;
    competing_writer(dir.path());
    #[cfg(not(target_os = "ios"))]
    assert_eq!(
        Store::open(dir.path(), support::protector())
            .err()
            .unwrap()
            .code,
        ErrorCode::WriterBusy
    );
    std::fs::write(dir.path().join("account.bin"), b"corrupt").unwrap();
    assert_eq!(account.request_resume(), 1);
    tokio::time::timeout(Duration::from_secs(10), async {
        while account.lifecycle_status() == 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(account.lifecycle_status(), 2);
    competing_writer(dir.path());
    assert_eq!(
        net.execute(NetworkOp::Start).await.unwrap_err().code,
        ErrorCode::Busy
    );
    #[cfg(not(target_os = "ios"))]
    assert_eq!(
        Store::open(dir.path(), support::protector())
            .err()
            .unwrap()
            .code,
        ErrorCode::WriterBusy
    );
}

#[tokio::test]
async fn suspend_interrupts_a_binding_transition_and_latest_request_wins() {
    let dir = tempfile::tempdir().unwrap();
    let slot = core(dir.path());
    let net = slot.network().unwrap();
    net.configure_local(None).await.unwrap();
    let account = slot.open_account(support::protector()).unwrap();
    settled(&account).await;
    let release = net.hold_next_bind_for_test();
    let worker = account.clone();
    let preparing = tokio::spawn(async move { worker.prepare_candidate().await });
    tokio::time::timeout(Duration::from_secs(5), async {
        while info(&net).await.state != NetworkState::Starting {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        net.execute(NetworkOp::Start).await.unwrap_err().code,
        ErrorCode::Busy
    );
    account.request_suspend();
    account.request_resume();
    account.request_suspend();
    assert!(
        tokio::time::timeout(Duration::from_secs(3), preparing)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    settled(&account).await;
    release.notify_one();
    assert_eq!(info(&net).await.state, NetworkState::Stopped);
    assert_eq!(
        net.execute(NetworkOp::Start).await.unwrap_err().code,
        ErrorCode::Busy
    );
    account.request_resume();
    settled(&account).await;
    assert_eq!(info(&net).await.state, NetworkState::Stopped);
    let candidate = account.prepare_candidate().await.unwrap();
    assert_eq!(online(&net).await.endpoint_id, Some(candidate.endpoint_id));
    net.execute(NetworkOp::Stop).await.unwrap();
}

struct SlowProtector {
    entered: std::sync::mpsc::Sender<()>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    first: std::sync::atomic::AtomicBool,
}
impl KeyProtector for SlowProtector {
    fn protect(&self, id: StoreId, key: &Secret32) -> Result<Vec<u8>, CoreError> {
        support::protector().protect(id, key)
    }
    fn unprotect(&self, id: StoreId, blob: &[u8]) -> Result<Secret32, CoreError> {
        if self.first.swap(false, std::sync::atomic::Ordering::AcqRel) {
            self.entered.send(()).unwrap();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(10))
                .unwrap();
        }
        support::protector().unprotect(id, blob)
    }
    fn delete(&self, id: StoreId) -> Result<(), CoreError> {
        support::protector().delete(id)
    }
}
fn saved_candidate(dir: &std::path::Path) {
    let device = chat_core::account::derive_device(Secret32::from([7; 32])).unwrap();
    let mut payload = vec![0x83, 1, 0, 0x58, 32];
    payload.extend_from_slice(&[7; 32]);
    Store::open(dir, support::protector())
        .unwrap()
        .commit(&StoredPlaintext {
            installation: device.installation_id().0,
            payload: zeroize::Zeroizing::new(payload),
        })
        .unwrap();
}
#[tokio::test]
async fn lifecycle_gates_during_slow_open_without_waiting_for_protector() {
    let dir = tempfile::tempdir().unwrap();
    saved_candidate(dir.path());
    let slot = std::sync::Arc::new(core(dir.path()));
    let net = slot.network().unwrap();
    net.configure_local(None).await.unwrap();
    let (entered, wait) = std::sync::mpsc::channel();
    let (release, receive) = std::sync::mpsc::channel();
    let protector = std::sync::Arc::new(SlowProtector {
        entered,
        release: std::sync::Mutex::new(receive),
        first: true.into(),
    });
    let worker = slot.clone();
    let thread = std::thread::spawn(move || worker.open_account(protector).unwrap());
    wait.recv_timeout(Duration::from_secs(3)).unwrap();
    let start = std::time::Instant::now();
    assert_eq!(slot.account_suspend(), 1);
    assert!(start.elapsed() < Duration::from_millis(100));
    assert_eq!(
        net.execute(NetworkOp::Start).await.unwrap_err().code,
        ErrorCode::Busy
    );
    assert_eq!(slot.account_resume(), 1);
    assert_eq!(slot.account_suspend(), 1);
    release.send(()).unwrap();
    let account = thread.join().unwrap();
    settled(&account).await;
    assert_eq!(
        account.prepare_candidate().await.unwrap_err().code,
        ErrorCode::StorageLocked
    );
    assert_eq!(
        net.execute(NetworkOp::Start).await.unwrap_err().code,
        ErrorCode::Busy
    );
    assert_eq!(slot.account_resume(), 1);
    settled(&account).await;
    assert_eq!(info(&net).await.state, NetworkState::Stopped);
    online(&net).await;
    net.execute(NetworkOp::Stop).await.unwrap();
}

#[tokio::test]
async fn lifecycle_without_account_does_not_strand_diagnostic_start() {
    let dir = tempfile::tempdir().unwrap();
    let slot = core(dir.path());
    let net = slot.network().unwrap();
    net.configure_local(None).await.unwrap();
    assert_eq!(slot.account_suspend(), 0);
    assert_eq!(
        net.execute(NetworkOp::Start).await.unwrap_err().code,
        ErrorCode::Busy
    );
    assert_eq!(slot.account_resume(), 0);
    online(&net).await;
    net.execute(NetworkOp::Stop).await.unwrap();
}

#[test]
fn account_writer_child() {
    let Some(path) = std::env::var_os("CP_A4_WRITER_PATH") else {
        return;
    };
    assert_eq!(
        Store::open(std::path::Path::new(&path), support::protector())
            .err()
            .unwrap()
            .code,
        ErrorCode::WriterBusy
    );
}
fn competing_writer(path: &std::path::Path) {
    #[cfg(not(target_os = "ios"))]
    {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "account_writer_child", "--nocapture"])
            .env("CP_A4_WRITER_PATH", path)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    let _ = path;
}

#[test]
fn invalid_candidate_payload_never_replaces_existing_snapshot() {
    let good = [vec![0x83, 1, 0, 0x58, 32], vec![7; 32]].concat();
    let device = chat_core::account::derive_device(Secret32::from([7; 32])).unwrap();
    for (payload, installation) in [
        (good.clone(), [0; 32]),
        (
            [vec![0x9f, 1, 0, 0x58, 32], vec![7; 32], vec![0xff]].concat(),
            device.installation_id().0,
        ),
        ([good.clone(), vec![0]].concat(), device.installation_id().0),
        (
            [vec![0x83, 0x18, 1, 0, 0x58, 32], vec![7; 32]].concat(),
            device.installation_id().0,
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        Store::open(dir.path(), support::protector())
            .unwrap()
            .commit(&StoredPlaintext {
                installation,
                payload: zeroize::Zeroizing::new(payload),
            })
            .unwrap();
        let before = std::fs::read(dir.path().join("account.bin")).unwrap();
        let slot = core(dir.path());
        assert_eq!(
            slot.open_account(support::protector()).err().unwrap().code,
            ErrorCode::StorageCorrupt
        );
        assert_eq!(
            std::fs::read(dir.path().join("account.bin")).unwrap(),
            before
        );
    }
}
struct LockedProtector;
impl KeyProtector for LockedProtector {
    fn protect(&self, _: StoreId, _: &Secret32) -> Result<Vec<u8>, CoreError> {
        panic!("must not replace unavailable key")
    }
    fn unprotect(&self, _: StoreId, _: &[u8]) -> Result<Secret32, CoreError> {
        Err(CoreError::new(
            ErrorCode::StorageLocked,
            "opaque key failure",
        ))
    }
    fn delete(&self, _: StoreId) -> Result<(), CoreError> {
        panic!("must not delete unavailable key")
    }
}
#[test]
fn opaque_unprotect_failure_does_not_regenerate_candidate() {
    let dir = tempfile::tempdir().unwrap();
    saved_candidate(dir.path());
    let before = std::fs::read(dir.path().join("account.bin")).unwrap();
    let slot = core(dir.path());
    assert_eq!(
        slot.open_account(std::sync::Arc::new(LockedProtector))
            .err()
            .unwrap()
            .code,
        ErrorCode::StorageLocked
    );
    assert_eq!(
        std::fs::read(dir.path().join("account.bin")).unwrap(),
        before
    );
}
