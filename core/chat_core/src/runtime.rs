use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{CoreError, ErrorCode};
use crate::event::{ConnectionSnapshot, CoreEvent, EventBus};

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
    running: Mutex<Option<CoreInfo>>,
}

impl CoreSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts the core, or returns the running one when `config` names the same folder.
    pub fn start(&self, config: CoreConfig, bus: &EventBus) -> Result<CoreInfo, CoreError> {
        let data_dir = prepare_data_dir(&config.data_dir)?;
        let mut running = self.running.lock().expect("core slot lock poisoned");
        if let Some(info) = running.as_ref() {
            if info.data_dir == data_dir {
                return Ok(info.clone());
            }
            return Err(CoreError::new(
                ErrorCode::AlreadyStartedDifferentConfig,
                format!("core already runs with {}", info.data_dir.display()),
            ));
        }
        let info = CoreInfo {
            version: CORE_VERSION.to_string(),
            instance_id: new_instance_id(),
            data_dir,
        };
        bus.publish(CoreEvent::ConnectionState(ConnectionSnapshot::OFFLINE));
        *running = Some(info.clone());
        Ok(info)
    }

    pub fn info(&self) -> Option<CoreInfo> {
        self.running.lock().expect("core slot lock poisoned").clone()
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
