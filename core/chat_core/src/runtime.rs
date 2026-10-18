use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::runtime::Runtime;

use crate::error::{CoreError, ErrorCode};
use crate::event::{ConnectionSnapshot, CoreEvent, EventBus};
use crate::transport::{NetworkHandle, NetworkProfile};
use crate::account::{AccountHandle, storage::KeyProtector};

/// Version of this core build. The UI shows it; Kotlin and Swift read it too.
pub const CORE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreConfig {
    /// Absolute folder for the database and keys of this installation.
    pub data_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreInfo {
    pub version: String,
    /// New every time a process starts the core. Callers in one process (Dart UI,
    /// Kotlin push receiver) compare it to confirm they reached the same core.
    pub instance_id: String,
    /// Canonical form of `CoreConfig::data_dir`.
    pub data_dir: PathBuf,
}

/// The one core of this process. On Android the Flutter UI and the push receiver
/// share a process (design 12.3), and either one may start the core first.
#[derive(Default)]
pub struct CoreSlot {
    gate: Arc<crate::transport::Gate>,
    running: Mutex<Option<Arc<RunningCore>>>,
}

struct RunningCore {
    info: CoreInfo,
    network: NetworkHandle,
    account: Mutex<Option<AccountHandle>>,
    runtime: Option<Runtime>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    hooks: Arc<crate::puppet::HookState>,
    bus: EventBus,
}

impl Drop for RunningCore {
    fn drop(&mut self) {
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        self.hooks.cancel.cancel();
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

impl CoreSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts the core, or returns the running one when `config` names the same folder.
    pub fn start(&self, config: CoreConfig, bus: &EventBus) -> Result<CoreInfo, CoreError> {
        self.start_with_profile(config, bus, NetworkProfile::Public)
    }

    fn start_with_profile(
        &self,
        config: CoreConfig,
        bus: &EventBus,
        profile: NetworkProfile,
    ) -> Result<CoreInfo, CoreError> {
        let data_dir = prepare_data_dir(&config.data_dir)?;
        let mut running = self.running.lock().expect("core slot lock poisoned");
        if let Some(core) = running.as_ref() {
            let info = &core.info;
            if info.data_dir == data_dir {
                return Ok(info.clone());
            }
            return Err(CoreError::new(
                ErrorCode::AlreadyStartedDifferentConfig,
                format!("core already runs with {}", info.data_dir.display()),
            ));
        }
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|_| CoreError::new(ErrorCode::TransportFailed, "cannot create core runtime"))?;
        let network = NetworkHandle::spawn_with_gate(runtime.handle(), profile, bus.clone(), self.gate.clone());
        let info = CoreInfo {
            version: CORE_VERSION.to_string(),
            instance_id: new_instance_id(),
            data_dir,
        };
        bus.publish(CoreEvent::ConnectionState(ConnectionSnapshot::OFFLINE));
        *running = Some(Arc::new(RunningCore {
            info: info.clone(),
            network,
            account: Mutex::new(None),
            runtime: Some(runtime),
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            hooks: Arc::default(),
            bus: bus.clone(),
        }));
        Ok(info)
    }

    /// Opens the protected account actor once. Established state refreshes after
    /// lifecycle readiness; production DHT construction is lazy until that point.
    pub fn open_account(&self, protector: Arc<dyn KeyProtector>) -> Result<AccountHandle, CoreError> {
        self.open_account_services(protector, crate::account::AccountServices::production())
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub fn open_account_with_services(&self, protector: Arc<dyn KeyProtector>, services: crate::account::AccountServices) -> Result<AccountHandle, CoreError> {
        self.open_account_services(protector, services)
    }
    fn open_account_services(&self, protector: Arc<dyn KeyProtector>, services: crate::account::AccountServices) -> Result<AccountHandle, CoreError> {
        let core = self.running.lock().expect("core slot lock poisoned").clone()
            .ok_or_else(|| CoreError::new(ErrorCode::CoreNotStarted, "core not started"))?;
        let mut account = core.account.lock().expect("account slot lock poisoned");
        if let Some(account) = account.as_ref() { return Ok(account.clone()); }
        let handle = AccountHandle::open(core.runtime.as_ref().unwrap().handle(), &core.info.data_dir, protector, core.network.clone(), services, core.bus.clone())?;
        *account = Some(handle.clone());
        Ok(handle)
    }
    pub fn account(&self) -> Result<AccountHandle, CoreError> {
        let core = self.running.lock().expect("core slot lock poisoned").clone()
            .ok_or_else(|| CoreError::new(ErrorCode::CoreNotStarted, "core not started"))?;
        let account = core.account.lock().expect("account slot lock poisoned").clone();
        account.ok_or_else(|| CoreError::new(ErrorCode::NotAvailable, "account not opened"))
    }
    /// Native lifecycle callers do not wait for initialization or another slot caller.
    pub fn account_suspend(&self) -> i32 { self.account_lifecycle(true) }
    pub fn account_resume(&self) -> i32 { self.account_lifecycle(false) }
    fn account_lifecycle(&self, suspend: bool) -> i32 {
        self.gate.lifecycle(suspend);
        let Ok(running) = self.running.try_lock() else { return 1; };
        let Some(core) = running.as_ref() else { return 0; };
        let Ok(account) = core.account.try_lock() else { return 1; };
        match account.as_ref() {
            Some(account) if suspend => account.request_suspend(),
            Some(account) => account.request_resume(),
            None => 0,
        }
    }

    #[cfg(not(all(feature = "test-hooks", debug_assertions)))]
    pub async fn test_hooks_start(&self, _control_url: String, _name: String) -> Result<(), CoreError> {
        Err(CoreError::new(ErrorCode::NotAvailable, "test hooks are disabled"))
    }

    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub async fn test_hooks_start(&self, control_url: String, name: String) -> Result<(), CoreError> {
        let core = self.running.lock().expect("core slot lock poisoned").clone()
            .ok_or_else(|| CoreError::new(ErrorCode::CoreNotStarted, "core not started"))?;
        crate::puppet::start(core.runtime.as_ref().unwrap().handle(), core.hooks.clone(),
            core.network.clone(), core.bus.clone(), core.info.instance_id.clone(), control_url, name).await
    }

    pub fn info(&self) -> Option<CoreInfo> {
        self.running.lock().expect("core slot lock poisoned").as_ref()
            .map(|core| core.info.clone())
    }

    /// Clones the actor handle; callers await commands after the slot lock is released.
    pub fn network(&self) -> Result<NetworkHandle, CoreError> {
        self.running.lock().expect("core slot lock poisoned").as_ref()
            .map(|core| core.network.clone())
            .ok_or_else(|| CoreError::new(ErrorCode::CoreNotStarted, "core not started"))
    }
}

/// Creates the folder if needed and returns its canonical path, so two callers that
/// name one folder differently (`..`, Android's `/data/data` and `/data/user/0`) match.
fn prepare_data_dir(dir: &Path) -> Result<PathBuf, CoreError> {
    if !dir.is_absolute() {
        return Err(CoreError::new(
            ErrorCode::InvalidConfig,
            format!("data_dir must be absolute: {}", dir.display()),
        ));
    }
    std::fs::create_dir_all(dir).map_err(|e| {
        CoreError::new(ErrorCode::InvalidConfig, format!("cannot create {}: {e}", dir.display()))
    })?;
    std::fs::canonicalize(dir).map_err(|e| {
        CoreError::new(ErrorCode::InvalidConfig, format!("cannot resolve {}: {e}", dir.display()))
    })
}

fn new_instance_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{}-{nanos:x}", std::process::id())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::EventSink;
    use std::sync::Arc;

    struct CountingSink(Mutex<Vec<CoreEvent>>);

    impl EventSink for CountingSink {
        fn send(&self, event: CoreEvent) -> bool {
            self.0.lock().unwrap().push(event);
            true
        }
    }

    fn config(dir: &Path) -> CoreConfig {
        CoreConfig { data_dir: dir.to_path_buf() }
    }

    #[test]
    fn first_start_creates_folder_and_publishes_offline_state() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("core");
        let bus = EventBus::new();
        let sink = Arc::new(CountingSink(Mutex::new(Vec::new())));
        bus.subscribe(sink.clone());

        let info = CoreSlot::new().start(config(&dir), &bus).unwrap();

        assert!(dir.is_dir());
        assert_eq!(info.version, CORE_VERSION);
        assert_eq!(*sink.0.lock().unwrap(), vec![CoreEvent::ConnectionState(ConnectionSnapshot::OFFLINE)]);
    }

    #[test]
    fn same_folder_returns_the_running_core() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = CoreSlot::new();
        let bus = EventBus::new();
        let first = slot.start(config(tmp.path()), &bus).unwrap();
        let again = slot.start(config(tmp.path()), &bus).unwrap();
        assert_eq!(again.instance_id, first.instance_id);
        assert_eq!(slot.info(), Some(first));
    }

    #[test]
    fn fresh_core_has_no_network_handle() {
        assert!(matches!(CoreSlot::new().network(), Err(error) if error.code == ErrorCode::CoreNotStarted));
    }

    #[tokio::test]
    async fn same_folder_shares_stopped_network_without_endpoint() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = CoreSlot::new();
        let bus = EventBus::new();
        let first = slot.start(config(tmp.path()), &bus).unwrap();
        let network = slot.network().unwrap().clone();
        let again = slot.start(config(tmp.path()), &bus).unwrap();
        let crate::transport::NetworkReply::Info(info) = network
            .execute(crate::transport::NetworkOp::Info).await.unwrap() else {
                panic!("expected network info");
            };
        assert_eq!(info.state, crate::transport::NetworkState::Stopped);
        assert_eq!(info.endpoint_id, None);
        assert_eq!(info.ticket, None);
        assert!(info.peers.is_empty());
        assert_eq!(again.instance_id, first.instance_id);
        drop(slot); // Must be safe inside a Tokio context.
    }

    #[tokio::test]
    async fn local_network_start_stop_keeps_core_and_shared_bus() {
        use crate::transport::{NetworkOp, NetworkProfile, NetworkReply, NetworkState};
        async fn online_id(network: &NetworkHandle) -> Option<String> {
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                loop {
                    let NetworkReply::Info(info) = network.execute(NetworkOp::Info).await.unwrap() else {
                        panic!("expected network info");
                    };
                    if info.state == NetworkState::Online {
                        assert!(info.endpoint_id.is_some());
                        assert!(info.ticket.is_some());
                        break info.endpoint_id;
                    }
                    assert_ne!(info.state, NetworkState::Failed);
                    tokio::task::yield_now().await;
                }
            }).await.unwrap()
        }
        let tmp = tempfile::tempdir().unwrap();
        let slot = CoreSlot::new();
        let bus = EventBus::new();
        let sink = Arc::new(CountingSink(Mutex::new(Vec::new())));
        bus.subscribe(sink.clone());
        let first = slot.start_with_profile(
            config(tmp.path()), &bus, NetworkProfile::Local { relay: None },
        ).unwrap();
        let network = slot.network().unwrap().clone();
        let NetworkReply::Info(started) = network.execute(NetworkOp::Start).await.unwrap() else {
            panic!("expected network info");
        };
        assert_eq!(started.state, NetworkState::Starting);
        let first_id = online_id(&slot.network().unwrap()).await;
        assert!(matches!(network.execute(NetworkOp::Stop).await.unwrap(), NetworkReply::Done));
        let NetworkReply::Info(stopped) = slot.network().unwrap().execute(NetworkOp::Info).await.unwrap() else {
            panic!("expected network info");
        };
        assert_eq!(stopped.state, NetworkState::Stopped);
        assert_eq!(stopped.endpoint_id, first_id);
        assert_eq!(stopped.ticket, None);
        assert_eq!(slot.start(config(tmp.path()), &bus).unwrap().instance_id, first.instance_id);
        assert!(sink.0.lock().unwrap().iter().any(|event| matches!(event, CoreEvent::NetworkChanged(info) if info.state == NetworkState::Online)));
        assert_eq!(sink.0.lock().unwrap().last(), Some(&CoreEvent::ConnectionState(ConnectionSnapshot::OFFLINE)));
        network.execute(NetworkOp::Start).await.unwrap();
        assert_eq!(online_id(&network).await, first_id);
        network.execute(NetworkOp::Stop).await.unwrap();
        assert_eq!(slot.info().unwrap().instance_id, first.instance_id);
        drop(slot);
    }

    #[test]
    fn same_folder_written_differently_returns_the_running_core() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("core");
        let detour = tmp.path().join("core").join("..").join("core");
        let slot = CoreSlot::new();
        let bus = EventBus::new();
        let first = slot.start(config(&dir), &bus).unwrap();
        let again = slot.start(config(&detour), &bus).unwrap();
        assert_eq!(again.instance_id, first.instance_id);
    }

    #[test]
    fn other_folder_is_refused_while_running() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = CoreSlot::new();
        let bus = EventBus::new();
        slot.start(config(&tmp.path().join("a")), &bus).unwrap();
        let err = slot.start(config(&tmp.path().join("b")), &bus).unwrap_err();
        assert_eq!(err.code, ErrorCode::AlreadyStartedDifferentConfig);
    }

    #[test]
    fn relative_folder_is_refused() {
        let err = CoreSlot::new()
            .start(config(Path::new("relative/core")), &EventBus::new())
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidConfig);
    }

    #[test]
    fn file_in_place_of_folder_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("core");
        std::fs::write(&file, b"not a folder").unwrap();
        let err = CoreSlot::new().start(config(&file), &EventBus::new()).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidConfig);
    }

    #[test]
    fn non_ascii_folder_works() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("사용자 데이터");
        let info = CoreSlot::new().start(config(&dir), &EventBus::new()).unwrap();
        assert!(info.data_dir.ends_with("사용자 데이터"));
    }

    #[test]
    fn restart_with_same_folder_does_not_publish_again() {
        let tmp = tempfile::tempdir().unwrap();
        let slot = CoreSlot::new();
        let bus = EventBus::new();
        slot.start(config(tmp.path()), &bus).unwrap();
        let sink = Arc::new(CountingSink(Mutex::new(Vec::new())));
        bus.subscribe(sink.clone()); // receives the replayed snapshot
        slot.start(config(tmp.path()), &bus).unwrap();
        assert_eq!(sink.0.lock().unwrap().len(), 1);
    }
}
