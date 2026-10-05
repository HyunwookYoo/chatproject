# M1a 코어 뼈대 + 다리 (Windows·Android) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rust 코어(`chat_core`)를 Flutter(Dart)와 Kotlin이 **같은 인스턴스로** 부르는 뼈대를 만든다. Windows 앱과 Android 에뮬레이터에서 빌드·테스트가 사람 손 없이 돌고, CI도 같은 테스트를 돌린다.

**Architecture:** 상태는 Rust 코어만 가진다(설계 12.1). `chat_core`는 플랫폼을 모르는 순수 Rust 라이브러리다. `app/rust`의 `chat_ffi` 크레이트 하나가 두 다리를 함께 싣는다. Dart용은 flutter_rust_bridge이고, Kotlin·Swift용은 UniFFI다. Android에서는 Flutter 화면과 앞으로 붙을 푸시 수신기가 한 프로세스의 같은 `.so`를 쓰게 된다. UI는 `events()` 한 스트림만 구독하고, 늦게 구독해도 최신 연결 상태를 먼저 받는다. 기기 테스트는 `tools/e2e`의 기기 관리 도구가 시험 전용 에뮬레이터를 켜고 끄며 돌린다(자동 테스트 설계 5절).

**Tech Stack:** Rust 1.92.0, flutter_rust_bridge 2.11.1, UniFFI 0.32.2, Flutter 3.44.6 / Dart 3.12.2, freezed + build_runner, path_provider, Dart `args`·`path`·`test`, Android Emulator(이 PC는 WHPX, CI는 KVM), GitHub Actions.

**Spec:**
- 설계 [`docs/superpowers/specs/2026-10-04-p2p-chat-design.md`](../specs/2026-10-04-p2p-chat-design.md) (rev.3)
- 자동 테스트 설계 [`docs/superpowers/specs/2026-10-05-test-automation-design.md`](../specs/2026-10-05-test-automation-design.md)
- 범위와 완료 기준: [`2026-10-04-p2p-chat-roadmap.md`](2026-10-04-p2p-chat-roadmap.md)의 M1a

## Global Constraints

- Rust 툴체인은 `rust-toolchain.toml`의 1.92.0이다. `chat_core`와 `tools/uniffi-bindgen`은 edition 2024, `chat_ffi`는 edition 2021이다. flutter_rust_bridge가 생성하는 코드가 2021 기준이다.
- flutter_rust_bridge는 Rust 크레이트·Dart 패키지·codegen 바이너리를 모두 `2.11.1`로 맞춘다. 이 PC에 설치된 codegen이 2.11.1이다.
- UniFFI는 `=0.32.2`로 고정하고, `chat_ffi`와 `tools/uniffi-bindgen`이 같은 버전을 쓴다.
- Flutter 3.44.6, Dart 3.12.2.
- Android: `applicationId`는 `dev.chatproject.chat_app`, `minSdk = 26`이다(스파이크 A와 같음).
- 상태는 Rust만 가진다. UI는 `events()` 한 스트림만 구독한다(설계 12.1, 12.2 규칙 1).
- 오류는 `ErrorCode`로 구분한다. 오류 문자열을 비교하지 않는다(12.2 규칙 3).
- 색은 `app/lib/theme/tokens.dart`에만 둔다. 청회색 + 민트는 테스트용 잠정 색이다. 화면 문구는 한국어로 쓴다.
- Android 기기 테스트는 **시험 전용 에뮬레이터가 기본**이다.
  - AVD `chat_e2e_1`(`emulator-5554`), `chat_e2e_2`(`emulator-5556`)
  - 이미지는 이미 설치된 `system-images;android-36;google_apis_playstore;x86_64`
  - 사용자의 `flutter_emulator`, `flutter_emulator_2`, `flutter_emulator_3`은 건드리지 않는다.
  - 폰(`PHONESERIAL`)은 마지막에 손으로 확인할 때만 쓴다.
- 커밋 금지 파일: `spikes/c-push/secrets/`, `*.p8`, `*.p12`, `*.jks`, `*.keystore`, `google-services.json`, `GoogleService-Info.plist`.
- 명령은 Windows의 Git Bash 기준이다. adb 경로는 `/c/Users/user/AppData/Local/Android/Sdk/platform-tools/adb.exe`다.
- 커밋 메시지는 Conventional Commits(`feat:`, `test:`, `chore:`, `ci:`)로 쓴다. 끝에는 실행 세션이 정한 attribution 줄을 붙인다.

## Review Focus

1. **같은 폴더를 다른 경로 문자열로 넘긴다.**
   - 예: `..`가 들어간 경로, Android의 `/data/data/...`와 `/data/user/0/...`.
   - 기대: 두 번째 시작도 같은 코어를 돌려준다.
   - 확인: Task 2 `same_folder_written_differently_returns_the_running_core`.
2. **디버그 핫 리스타트로 `main()`이 같은 프로세스에서 다시 돈다.**
   - 기대: 같은 인스턴스를 돌려주고, 새 구독자는 연결 상태를 바로 받는다. 연결 상태 이벤트를 다시 발행하지는 않는다.
   - 확인: Task 1 `late_subscriber_gets_latest_connection_state_first`, Task 2 `restart_with_same_folder_does_not_publish_again`, Task 3 통합 테스트.
3. **화면이 닫혀 Dart 구독이 끊긴다.**
   - 기대: Rust가 그 구독을 버린다. 끊긴 곳에 계속 보내지 않는다.
   - 확인: Task 1 `closed_sink_is_dropped_on_next_publish`, `subscriber_closed_before_snapshot_is_not_kept`.
4. **Windows 사용자 폴더 이름이 한글이다.**
   - 기대: 코어가 정상적으로 시작한다.
   - 확인: Task 2 `non_ascii_folder_works`.
5. **코어 시작이 실패한다.**
   - 기대: 빈 화면 대신 이유가 보인다.
   - 확인: Task 6 `shows why the core did not start`.

---

## 시작 전 준비 (사용자)

1. **Windows 개발자 모드를 켠다**: 설정 → 시스템 → 개발자용 → 개발자 모드.
   - Flutter 플러그인 빌드에 심볼릭 링크가 필요하다.
   - 꺼져 있으면 Task 3의 `flutter_rust_bridge_codegen integrate`가 `Building with plugins requires symlink support.`로 멈춘다. 2026-10-04 사전 확인에서 이 오류가 실제로 났다.
2. **에뮬레이터 실행 환경을 확인한다.**
   - WHPX 가속: 2026-10-05에 `emulator -accel-check`로 동작을 확인했다.
   - 남은 메모리: 에뮬레이터 2대를 함께 띄우는 단계(Task 4)에서는 남은 메모리가 6 GB 이상이 되도록 다른 프로그램을 닫는다.
3. **폰은 선택이다.** Task 6의 마지막 손 확인 때만 USB 디버깅으로 연결한다.
4. **GitHub 비공개 저장소 생성을 승인한다(Task 7).**
   - `gh`는 `HyunwookYoo` 계정으로 로그인돼 있다.
   - 저장소를 만들고 push하는 명령은 승인을 받은 뒤에만 실행한다.

## 파일 구조

```
ChatProject/
├─ .gitignore                         Task 1
├─ .github/workflows/ci.yml           Task 7
├─ Cargo.toml                         Task 1 (멤버는 Task 3·5에서 추가)
├─ rust-toolchain.toml                Task 1
├─ core/chat_core/
│  ├─ Cargo.toml                      Task 1, 2
│  └─ src/
│     ├─ lib.rs                       재내보내기만
│     ├─ error.rs                     ErrorCode, CoreError            Task 1
│     ├─ event.rs                     CoreEvent, EventBus, EventSink   Task 1
│     └─ runtime.rs                   CoreConfig, CoreInfo, CoreSlot   Task 2
├─ tools/
│  ├─ e2e/                            기기 관리 도구 (Dart)             Task 4
│  │  ├─ pubspec.yaml, analysis_options.yaml
│  │  ├─ bin/devices.dart             ensure / up / down / test 명령
│  │  ├─ lib/src/android_sdk.dart     SDK·AVD 폴더 찾기
│  │  ├─ lib/src/process_runner.dart  외부 프로그램 실행 (시험에서 가짜로 바꿈)
│  │  ├─ lib/src/emulators.dart       에뮬레이터 만들기·켜기·대기·끄기
│  │  └─ test/emulators_test.dart
│  └─ uniffi-bindgen/                 Kotlin 바인딩 생성기              Task 5
└─ app/                               Flutter 앱 chat_app              Task 3
   ├─ rust/                           크레이트 chat_ffi
   │  └─ src/
   │     ├─ lib.rs
   │     ├─ api/mod.rs, api/chat.rs   Dart가 보는 명령·타입            Task 3
   │     ├─ convert.rs                chat_core 타입 ↔ Dart 타입, FrbSink  Task 3
   │     ├─ global.rs                 프로세스 전역 BUS, CORE           Task 3
   │     └─ native.rs                 Kotlin·Swift용 UniFFI 함수        Task 5
   ├─ lib/
   │  ├─ main.dart                                                      Task 3, 6
   │  ├─ core_client.dart             CoreClient 인터페이스              Task 6
   │  ├─ theme/tokens.dart, theme/app_theme.dart                        Task 6
   │  └─ home/home_screen.dart                                          Task 6
   ├─ test/home_screen_test.dart                                        Task 6
   ├─ integration_test/core_test.dart                                   Task 3
   ├─ integration_test/native_bridge_test.dart                          Task 5
   └─ android/app/
      ├─ build.gradle.kts                                               Task 5
      └─ src/main/kotlin/
         ├─ dev/chatproject/chat_app/MainActivity.kt                    Task 5
         └─ uniffi/chat_ffi/chat_ffi.kt (생성됨)                        Task 5
```

---

### Task 1: 저장소, Rust 워크스페이스, 이벤트 버스

**Files:**
- Create: `.gitignore`, `Cargo.toml`, `rust-toolchain.toml`
- Create: `core/chat_core/Cargo.toml`, `core/chat_core/src/lib.rs`, `core/chat_core/src/error.rs`, `core/chat_core/src/event.rs`

**Interfaces:**
- Consumes: 없음
- Produces:
  - `chat_core::ErrorCode { InvalidConfig, AlreadyStartedDifferentConfig }`
  - `chat_core::CoreError { code: ErrorCode, detail: String }` + `CoreError::new(code, detail: impl Into<String>)`
  - `chat_core::ConnectionSnapshot { direct_peers: u32, mailbox_ok: bool, queue_ok: Option<bool> }` + `ConnectionSnapshot::OFFLINE`
  - `chat_core::CoreEvent::{ConnectionState(ConnectionSnapshot), Error { code: ErrorCode, detail: String }}`
  - `trait chat_core::EventSink: Send + Sync + 'static { fn send(&self, event: CoreEvent) -> bool; }`
  - `chat_core::EventBus::{new() -> Self, subscribe(&self, Arc<dyn EventSink>), publish(&self, CoreEvent), subscriber_count(&self) -> usize}`

- [ ] **Step 1: 기존 문서·스파이크를 첫 커밋으로 넣는다**

`.gitignore`를 만든다:

```gitignore
# Rust
**/target/

# Flutter / Dart / Android build output
**/.dart_tool/
**/build/
**/.flutter-plugins
**/.flutter-plugins-dependencies
**/.gradle/
**/.kotlin/
**/.cxx/

# IDE
.idea/
.vscode/
*.iml

# Local machine config
**/local.properties

# Secrets: never commit
spikes/c-push/secrets/
*.p8
*.p12
*.jks
*.keystore
google-services.json
GoogleService-Info.plist
```

```bash
cd /c/ChatProject
git init -b main
git add -A
git status --short | grep -iE "secret|\.p8|\.p12|keystore|google-services" ; echo "secret check exit=$?"
```

Expected: `secret check exit=1` (일치하는 파일 없음). 0이 나오면 멈추고 `.gitignore`를 고친다.

```bash
git commit -m "chore: import design docs and spikes"
```

- [ ] **Step 2: 워크스페이스와 툴체인 파일을 만든다**

`Cargo.toml`:

```toml
[workspace]
resolver = "3"
members = ["core/chat_core"]
# Spikes are separate experiments with their own Cargo.lock.
exclude = ["spikes/a-connectivity", "spikes/b-mls-memory"]
```

`rust-toolchain.toml` (처음 한 번 rustup이 1.92.0을 내려받는다. 설치된 `stable`과 이름이 다르기 때문이다):

```toml
[toolchain]
channel = "1.92.0"
components = ["rustfmt", "clippy"]
targets = ["aarch64-linux-android", "armv7-linux-androideabi", "x86_64-linux-android", "i686-linux-android"]
```

`core/chat_core/Cargo.toml`:

```toml
[package]
name = "chat_core"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
thiserror = "2"
```

`core/chat_core/src/lib.rs`:

```rust
//! Chat core. All app state lives here; the UI only renders what this crate reports
//! (design 12.1).

pub mod error;
pub mod event;

pub use error::{CoreError, ErrorCode};
pub use event::{ConnectionSnapshot, CoreEvent, EventBus, EventSink};
```

`core/chat_core/src/error.rs`:

```rust
/// Error kinds the UI can branch on. Never match on `detail` (design 12.2 rule 3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    /// The start configuration is unusable, e.g. the data folder cannot be created.
    InvalidConfig,
    /// The core already runs in this process with another data folder.
    AlreadyStartedDifferentConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{code:?}: {detail}")]
pub struct CoreError {
    pub code: ErrorCode,
    pub detail: String,
}

impl CoreError {
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self { code, detail: detail.into() }
    }
}
```

- [ ] **Step 3: 이벤트 버스의 실패하는 테스트를 쓴다**

`core/chat_core/src/event.rs` (아직 테스트만):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[derive(Default)]
    struct RecordingSink {
        events: Mutex<Vec<CoreEvent>>,
        closed: AtomicBool,
    }

    impl RecordingSink {
        fn events(&self) -> Vec<CoreEvent> {
            self.events.lock().unwrap().clone()
        }
    }

    impl EventSink for RecordingSink {
        fn send(&self, event: CoreEvent) -> bool {
            if self.closed.load(Ordering::SeqCst) {
                return false;
            }
            self.events.lock().unwrap().push(event);
            true
        }
    }

    fn state(direct_peers: u32) -> CoreEvent {
        CoreEvent::ConnectionState(ConnectionSnapshot { direct_peers, ..ConnectionSnapshot::OFFLINE })
    }

    fn error(detail: &str) -> CoreEvent {
        CoreEvent::Error { code: ErrorCode::InvalidConfig, detail: detail.to_string() }
    }

    #[test]
    fn subscriber_gets_events_in_publish_order() {
        let bus = EventBus::new();
        let sink = Arc::new(RecordingSink::default());
        bus.subscribe(sink.clone());
        bus.publish(state(1));
        bus.publish(error("e"));
        bus.publish(state(2));
        assert_eq!(sink.events(), vec![state(1), error("e"), state(2)]);
    }

    #[test]
    fn late_subscriber_gets_latest_connection_state_first() {
        let bus = EventBus::new();
        bus.publish(state(1));
        bus.publish(state(3));
        let sink = Arc::new(RecordingSink::default());
        bus.subscribe(sink.clone());
        bus.publish(state(4));
        assert_eq!(sink.events(), vec![state(3), state(4)]);
    }

    #[test]
    fn errors_are_not_replayed_to_late_subscribers() {
        let bus = EventBus::new();
        bus.publish(error("old"));
        let sink = Arc::new(RecordingSink::default());
        bus.subscribe(sink.clone());
        assert_eq!(sink.events(), Vec::<CoreEvent>::new());
    }

    #[test]
    fn closed_sink_is_dropped_on_next_publish() {
        let bus = EventBus::new();
        let sink = Arc::new(RecordingSink::default());
        bus.subscribe(sink.clone());
        assert_eq!(bus.subscriber_count(), 1);
        sink.closed.store(true, Ordering::SeqCst);
        bus.publish(state(1));
        assert_eq!(bus.subscriber_count(), 0);
    }

    #[test]
    fn subscriber_closed_before_snapshot_is_not_kept() {
        let bus = EventBus::new();
        bus.publish(state(1));
        let sink = Arc::new(RecordingSink::default());
        sink.closed.store(true, Ordering::SeqCst);
        bus.subscribe(sink.clone());
        assert_eq!(bus.subscriber_count(), 0);
    }
}
```

- [ ] **Step 4: 테스트가 실패하는지 확인한다**

Run: `cargo test -p chat_core`
Expected: 컴파일 실패 — `cannot find type `EventBus` in this scope`, `cannot find type `Mutex`` 등. 테스트는 `use super::*`로 구현 쪽 `use`를 함께 쓰므로, 구현이 생기면 함께 풀린다.

- [ ] **Step 5: 이벤트 버스를 구현한다**

`core/chat_core/src/event.rs`의 테스트 모듈 **위에** 넣는다:

```rust
use std::sync::{Arc, Mutex};

use crate::error::ErrorCode;

/// What this device is connected to right now (design 12.1 `ConnectionState`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionSnapshot {
    pub direct_peers: u32,
    pub mailbox_ok: bool,
    /// `None` while this user has no server queue.
    pub queue_ok: Option<bool>,
}

impl ConnectionSnapshot {
    pub const OFFLINE: Self = Self { direct_peers: 0, mailbox_ok: false, queue_ok: None };
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreEvent {
    ConnectionState(ConnectionSnapshot),
    Error { code: ErrorCode, detail: String },
}

/// Receives core events. Return `false` once the receiver is gone; the bus then drops it.
pub trait EventSink: Send + Sync + 'static {
    fn send(&self, event: CoreEvent) -> bool;
}

/// Fans out core events to every subscriber in publish order. A subscriber that joins
/// late first gets the latest connection state; other events are not replayed.
#[derive(Default)]
pub struct EventBus {
    state: Mutex<BusState>,
}

#[derive(Default)]
struct BusState {
    sinks: Vec<Arc<dyn EventSink>>,
    last_connection: Option<ConnectionSnapshot>,
}

impl EventBus {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn subscribe(&self, sink: Arc<dyn EventSink>) {
        let mut state = self.state.lock().expect("event bus lock poisoned");
        if let Some(snapshot) = state.last_connection {
            if !sink.send(CoreEvent::ConnectionState(snapshot)) {
                return;
            }
        }
        state.sinks.push(sink);
    }

    pub fn publish(&self, event: CoreEvent) {
        let mut state = self.state.lock().expect("event bus lock poisoned");
        if let CoreEvent::ConnectionState(snapshot) = &event {
            state.last_connection = Some(*snapshot);
        }
        state.sinks.retain(|sink| sink.send(event.clone()));
    }

    pub fn subscriber_count(&self) -> usize {
        self.state.lock().expect("event bus lock poisoned").sinks.len()
    }
}
```

- [ ] **Step 6: 테스트가 통과하는지 확인한다**

Run: `cargo test -p chat_core`
Expected: `test result: ok. 5 passed`

Run: `cargo clippy -p chat_core -- -D warnings`
Expected: 경고 없음.

- [ ] **Step 7: 커밋**

```bash
git add .gitignore Cargo.toml Cargo.lock rust-toolchain.toml core/
git commit -m "feat(core): event bus with connection-state replay"
```

---

### Task 2: 코어 시작 — 프로세스당 코어 하나

**Files:**
- Create: `core/chat_core/src/runtime.rs`
- Modify: `core/chat_core/src/lib.rs`, `core/chat_core/Cargo.toml`

**Interfaces:**
- Consumes: Task 1의 `CoreError`, `ErrorCode`, `CoreEvent`, `ConnectionSnapshot`, `EventBus`, `EventSink`
- Produces:
  - `chat_core::CORE_VERSION: &str`
  - `chat_core::CoreConfig { data_dir: PathBuf }`
  - `chat_core::CoreInfo { version: String, instance_id: String, data_dir: PathBuf }` (`data_dir`은 정규화된 경로)
  - `chat_core::CoreSlot::{new() -> Self, start(&self, CoreConfig, &EventBus) -> Result<CoreInfo, CoreError>, info(&self) -> Option<CoreInfo>}`

- [ ] **Step 1: 테스트 의존성을 추가한다**

`core/chat_core/Cargo.toml` 끝에:

```toml
[dev-dependencies]
tempfile = "3"
```

`core/chat_core/src/lib.rs`를 바꾼다:

```rust
//! Chat core. All app state lives here; the UI only renders what this crate reports
//! (design 12.1).

pub mod error;
pub mod event;
pub mod runtime;

pub use error::{CoreError, ErrorCode};
pub use event::{ConnectionSnapshot, CoreEvent, EventBus, EventSink};
pub use runtime::{CORE_VERSION, CoreConfig, CoreInfo, CoreSlot};
```

- [ ] **Step 2: 실패하는 테스트를 쓴다**

`core/chat_core/src/runtime.rs` (아직 테스트만):

```rust
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
```

- [ ] **Step 3: 테스트가 실패하는지 확인한다**

Run: `cargo test -p chat_core runtime`
Expected: 컴파일 실패 — `cannot find struct, variant or union type `CoreConfig``, `cannot find type `Path`` 등. `Path`, `Mutex`, `CoreEvent` 같은 이름은 구현 쪽 `use`에서 `use super::*`로 들어온다.

- [ ] **Step 4: 구현한다**

`core/chat_core/src/runtime.rs`의 테스트 모듈 **위에** 넣는다:

```rust
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
```

- [ ] **Step 5: 테스트가 통과하는지 확인한다**

Run: `cargo test -p chat_core`
Expected: `test result: ok. 13 passed`

Run: `cargo clippy -p chat_core --all-targets -- -D warnings`
Expected: 경고 없음.

- [ ] **Step 6: 커밋**

```bash
git add core/ Cargo.lock
git commit -m "feat(core): one core per process with canonical data folder"
```

---

### Task 3: Flutter 앱과 flutter_rust_bridge 다리

**Files:**
- Create (생성 도구): `app/` 전체 — `flutter create`와 `flutter_rust_bridge_codegen integrate`가 만든다.
- Create: `app/rust/src/api/chat.rs`, `app/rust/src/convert.rs`, `app/rust/src/global.rs`, `app/integration_test/core_test.dart`
- Modify: `Cargo.toml`, `app/rust/Cargo.toml`, `app/rust/src/lib.rs`, `app/rust/src/api/mod.rs`, `app/lib/main.dart`, `app/analysis_options.yaml`, `app/pubspec.yaml` (`flutter pub add`로)
- Delete: `app/rust/src/api/simple.rs`, `app/lib/src/rust/api/simple.dart`, `app/integration_test/simple_test.dart`, `app/test/widget_test.dart`, `app/rust/Cargo.lock`

**Interfaces:**
- Consumes: Task 2의 `CoreSlot`, `CoreConfig`, `CoreInfo`, `CORE_VERSION`. Task 1의 `EventBus`, `EventSink`, `CoreEvent`, `CoreError`, `ErrorCode`.
- Produces (Rust, `chat_ffi::api::chat`):
  - `fn core_version() -> String` (sync)
  - `fn core_start(data_dir: String) -> Result<CoreInfo, CoreError>`
  - `fn events(sink: StreamSink<ChatEvent>) -> Result<(), CoreError>`
  - 타입: `CoreInfo { version: String, instance_id: String }`, `ErrorCode { InvalidConfig, AlreadyStartedDifferentConfig }`, `CoreError { code: ErrorCode, detail: String }`, `ChatEvent::{ConnectionState { direct_peers: u32, mailbox_ok: bool, queue_ok: Option<bool> }, Error { code: ErrorCode, detail: String }}`
- Produces (Rust, crate 내부): `chat_ffi::global::{BUS: LazyLock<EventBus>, CORE: LazyLock<CoreSlot>}`
- Produces (Dart, `package:chat_app/src/rust/api/chat.dart`):
  - 함수: `String coreVersion()`, `Future<CoreInfo> coreStart({required String dataDir})`, `Stream<ChatEvent> events()`
  - 클래스: `CoreInfo`, `CoreError implements FrbException`, `enum ErrorCode { invalidConfig, alreadyStartedDifferentConfig }`
  - freezed 클래스: `ChatEvent`, `ChatEvent_ConnectionState`, `ChatEvent_Error`

- [ ] **Step 1: Flutter 앱을 만들고 Rust 다리를 붙인다**

```bash
cd /c/ChatProject
flutter create --org dev.chatproject --project-name chat_app --platforms windows,android,ios app
cd app
flutter_rust_bridge_codegen integrate --rust-crate-name chat_ffi --rust-crate-dir rust
```

Expected:
- 마지막 출력에 `Error:`가 없다.
- `app/rust/`, `app/rust_builder/`, `app/flutter_rust_bridge.yaml`, `app/lib/src/rust/frb_generated.dart`가 생긴다.
- `Building with plugins requires symlink support`가 나오면 "시작 전 준비" 1번(개발자 모드)을 하고 이 단계를 다시 한다.

- [ ] **Step 2: 워크스페이스에 넣고 의존성을 정리한다**

`Cargo.toml`의 `members`를 바꾼다:

```toml
members = ["core/chat_core", "app/rust"]
```

`app/rust/Cargo.toml`을 통째로 바꾼다:

```toml
[package]
name = "chat_ffi"
version = "0.1.0"
edition = "2021"
publish = false

[lib]
crate-type = ["cdylib", "staticlib"]

[dependencies]
chat_core = { path = "../../core/chat_core" }
flutter_rust_bridge = "=2.11.1"

[lints.rust]
unexpected_cfgs = { level = "warn", check-cfg = ['cfg(frb_expand)'] }
```

```bash
cd /c/ChatProject
rm -f app/rust/Cargo.lock
rm -f app/rust/src/api/simple.rs app/lib/src/rust/api/simple.dart app/integration_test/simple_test.dart app/test/widget_test.dart
cd app
flutter pub add freezed_annotation path_provider
flutter pub add dev:freezed dev:build_runner 'dev:integration_test:{"sdk":"flutter"}'
```

Expected: `flutter pub add`가 오류 없이 끝난다.

생성 코드는 분석에서 뺀다. `app/analysis_options.yaml` 끝에 붙인다:

```yaml
analyzer:
  exclude:
    - lib/src/rust/**
```

- [ ] **Step 3: 실패하는 통합 테스트를 쓴다**

`app/integration_test/core_test.dart`:

```dart
import 'dart:io';

import 'package:chat_app/src/rust/api/chat.dart';
import 'package:chat_app/src/rust/frb_generated.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  late Directory dataDir;

  setUpAll(() async {
    await RustLib.init();
    dataDir = await Directory.systemTemp.createTemp('chat_core_it_');
  });

  test('starting twice returns the same core', () async {
    final first = await coreStart(dataDir: dataDir.path);
    final again = await coreStart(dataDir: dataDir.path);
    expect(again.instanceId, first.instanceId);
    expect(first.version, coreVersion());
  });

  test('a late listener still gets the connection state', () async {
    await coreStart(dataDir: dataDir.path);
    final event = await events().first.timeout(const Duration(seconds: 5));
    expect(event, isA<ChatEvent_ConnectionState>());
    expect((event as ChatEvent_ConnectionState).directPeers, 0);
  });

  test('another data folder is refused while the core runs', () async {
    await coreStart(dataDir: dataDir.path);
    final other = await Directory.systemTemp.createTemp('chat_core_other_');
    await expectLater(
      coreStart(dataDir: other.path),
      throwsA(isA<CoreError>().having((e) => e.code, 'code', ErrorCode.alreadyStartedDifferentConfig)),
    );
  });
}
```

- [ ] **Step 4: 테스트가 실패하는지 확인한다**

Run: `cd /c/ChatProject/app && flutter test integration_test/core_test.dart -d windows`
Expected: 컴파일 실패 — `package:chat_app/src/rust/api/chat.dart`를 찾지 못함.

- [ ] **Step 5: Rust 쪽 다리를 쓴다**

`app/rust/src/lib.rs`:

```rust
pub mod api;
mod convert;
mod frb_generated;
mod global;
```

`app/rust/src/api/mod.rs`:

```rust
pub mod chat;
```

`app/rust/src/api/chat.rs`:

```rust
//! Commands and events the Flutter UI uses (design 12.1). Keep this file thin:
//! logic lives in chat_core, this module only converts types.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::convert::FrbSink;
use crate::frb_generated::StreamSink;
use crate::global::{BUS, CORE};

#[flutter_rust_bridge::frb(init)]
pub fn init_app() {
    flutter_rust_bridge::setup_default_user_utils();
}

pub struct CoreInfo {
    pub version: String,
    pub instance_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidConfig,
    AlreadyStartedDifferentConfig,
}

#[derive(Debug)]
pub struct CoreError {
    pub code: ErrorCode,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ChatEvent {
    ConnectionState {
        direct_peers: u32,
        mailbox_ok: bool,
        queue_ok: Option<bool>,
    },
    Error {
        code: ErrorCode,
        detail: String,
    },
}

#[flutter_rust_bridge::frb(sync)]
pub fn core_version() -> String {
    chat_core::CORE_VERSION.to_string()
}

/// Starts the core, or returns the running one (see `chat_core::CoreSlot::start`).
pub fn core_start(data_dir: String) -> Result<CoreInfo, CoreError> {
    let config = chat_core::CoreConfig { data_dir: PathBuf::from(data_dir) };
    CORE.start(config, &BUS).map(CoreInfo::from).map_err(CoreError::from)
}

/// The single event stream of the UI (design 12.2 rule 1).
pub fn events(sink: StreamSink<ChatEvent>) -> Result<(), CoreError> {
    BUS.subscribe(Arc::new(FrbSink(Mutex::new(sink))));
    Ok(())
}
```

`app/rust/src/global.rs`:

```rust
//! Process-wide core state. Every entry point (Dart through `api`, Kotlin and Swift
//! through `native`) goes through these two statics.

use std::sync::LazyLock;

use chat_core::{CoreSlot, EventBus};

pub(crate) static BUS: LazyLock<EventBus> = LazyLock::new(EventBus::new);
pub(crate) static CORE: LazyLock<CoreSlot> = LazyLock::new(CoreSlot::new);
```

`app/rust/src/convert.rs`:

```rust
//! Conversions between chat_core types and the types Dart sees.

use std::sync::Mutex;

use crate::api::chat::{ChatEvent, CoreError, CoreInfo, ErrorCode};
use crate::frb_generated::StreamSink;

impl From<chat_core::CoreInfo> for CoreInfo {
    fn from(info: chat_core::CoreInfo) -> Self {
        Self { version: info.version, instance_id: info.instance_id }
    }
}

impl From<chat_core::ErrorCode> for ErrorCode {
    fn from(code: chat_core::ErrorCode) -> Self {
        match code {
            chat_core::ErrorCode::InvalidConfig => Self::InvalidConfig,
            chat_core::ErrorCode::AlreadyStartedDifferentConfig => Self::AlreadyStartedDifferentConfig,
        }
    }
}

impl From<chat_core::CoreError> for CoreError {
    fn from(err: chat_core::CoreError) -> Self {
        Self { code: err.code.into(), detail: err.detail }
    }
}

impl From<chat_core::CoreEvent> for ChatEvent {
    fn from(event: chat_core::CoreEvent) -> Self {
        match event {
            chat_core::CoreEvent::ConnectionState(s) => Self::ConnectionState {
                direct_peers: s.direct_peers,
                mailbox_ok: s.mailbox_ok,
                queue_ok: s.queue_ok,
            },
            chat_core::CoreEvent::Error { code, detail } => Self::Error { code: code.into(), detail },
        }
    }
}

/// Forwards core events into a Dart stream. A closed Dart stream ends the subscription.
pub(crate) struct FrbSink(pub(crate) Mutex<StreamSink<ChatEvent>>);

impl chat_core::EventSink for FrbSink {
    fn send(&self, event: chat_core::CoreEvent) -> bool {
        let sink = self.0.lock().expect("stream sink lock poisoned");
        sink.add(ChatEvent::from(event)).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_state_keeps_every_field() {
        let event = chat_core::CoreEvent::ConnectionState(chat_core::ConnectionSnapshot {
            direct_peers: 2,
            mailbox_ok: true,
            queue_ok: Some(false),
        });
        assert_eq!(
            ChatEvent::from(event),
            ChatEvent::ConnectionState { direct_peers: 2, mailbox_ok: true, queue_ok: Some(false) }
        );
    }

    #[test]
    fn error_keeps_code_and_detail() {
        let err = chat_core::CoreError::new(chat_core::ErrorCode::AlreadyStartedDifferentConfig, "x");
        let converted = CoreError::from(err);
        assert_eq!(converted.code, ErrorCode::AlreadyStartedDifferentConfig);
        assert_eq!(converted.detail, "x");
    }
}
```

`app/lib/main.dart`를 통째로 바꾼다(임시. Task 6에서 다시 바꾼다):

```dart
import 'package:flutter/material.dart';

import 'src/rust/api/chat.dart';
import 'src/rust/frb_generated.dart';

Future<void> main() async {
  await RustLib.init();
  runApp(MaterialApp(home: Scaffold(body: Center(child: Text('core ${coreVersion()}')))));
}
```

- [ ] **Step 6: 바인딩을 생성하고 Rust 테스트를 돌린다**

```bash
cd /c/ChatProject/app
flutter_rust_bridge_codegen generate
dart run build_runner build --delete-conflicting-outputs
```

Expected: `app/lib/src/rust/api/chat.dart`와 `app/lib/src/rust/api/chat.freezed.dart`가 생긴다. `app/rust/src/frb_generated.rs`가 다시 써진다.

Run: `cd /c/ChatProject && cargo test --workspace`
Expected: `chat_core` 13개, `chat_ffi` 2개 통과.

- [ ] **Step 7: 통합 테스트가 통과하는지 확인한다**

Run: `cd /c/ChatProject/app && flutter test integration_test/core_test.dart -d windows`
Expected: `All tests passed!` (3개)

Run: `flutter analyze`
Expected: `No issues found!`

- [ ] **Step 8: 커밋**

```bash
cd /c/ChatProject
git add Cargo.toml Cargo.lock app/
git status --short | grep -iE "secret|\.p8|keystore|google-services" ; echo "secret check exit=$?"
git commit -m "feat(app): Flutter shell with flutter_rust_bridge core API"
```

Expected: `secret check exit=1`.

---

### Task 4: 기기 관리 도구 — 시험 전용 에뮬레이터 자동 실행

**Files:**
- Create: `tools/e2e/pubspec.yaml`, `tools/e2e/analysis_options.yaml`
- Create: `tools/e2e/lib/src/android_sdk.dart`, `tools/e2e/lib/src/process_runner.dart`, `tools/e2e/lib/src/emulators.dart`
- Create: `tools/e2e/bin/devices.dart`
- Test: `tools/e2e/test/emulators_test.dart`

**Interfaces:**
- Consumes: Task 3의 `app/` (명령 `test`가 `app/`에서 `flutter test`를 돌린다)
- Produces:
  - 명령: `dart run tools/e2e/bin/devices.dart <ensure|up|down|test> [--count 1|2] [--keep] [<flutter test 인자>]`
    - `ensure`: 시험 전용 AVD와 `clean` 스냅숏이 없으면 만든다.
    - `up`: 에뮬레이터를 켜고 부팅이 끝날 때까지 기다린다.
    - `down`: 에뮬레이터를 끈다.
    - `test`: 켜고 → `app/`에서 `flutter test <인자> -d emulator-5554` → 이 명령이 켠 에뮬레이터만 끈다(`--keep`이면 남겨 둠).
  - 라이브러리 (`package:chat_e2e/src/…`): `EmulatorSpec(avd, port)` + `.serial`, `testEmulators`, `cleanSnapshot`, `bootArgs(spec)`, `firstBootArgs(spec)`, `parseAdbDevices(output)`, `Emulators{devices, runningAvd, isRunning, ensure, up, waitBooted, stop}`, `AndroidSdk.locate()` + `.adb`·`.emulator`, `defaultAvdHome()`, `ProcessRunner`, `IoProcessRunner`

- [ ] **Step 1: Dart 패키지를 만든다**

`tools/e2e/pubspec.yaml`:

```yaml
name: chat_e2e
description: Device and multi-device test tooling for ChatProject.
publish_to: none

environment:
  sdk: ^3.12.0
```

`tools/e2e/analysis_options.yaml`:

```yaml
include: package:lints/recommended.yaml
```

```bash
cd /c/ChatProject/tools/e2e
dart pub add args path
dart pub add dev:test dev:lints
```

Expected: 오류 없이 끝나고 `pubspec.lock`이 생긴다.

- [ ] **Step 2: 실패하는 테스트를 쓴다**

`tools/e2e/test/emulators_test.dart`:

```dart
import 'dart:async';
import 'dart:io';

import 'package:chat_e2e/src/android_sdk.dart';
import 'package:chat_e2e/src/emulators.dart';
import 'package:chat_e2e/src/process_runner.dart';
import 'package:test/test.dart';

/// Answers every command from [respond] and records what was run.
class FakeRunner implements ProcessRunner {
  FakeRunner(this.respond);

  final ProcessResult Function(String executable, List<String> arguments) respond;
  final calls = <String>[];
  final detached = <List<String>>[];

  @override
  Future<ProcessResult> run(String executable, List<String> arguments,
      {String? stdin, String? workingDirectory}) async {
    calls.add([executable, ...arguments].join(' '));
    return respond(executable, arguments);
  }

  @override
  Future<void> startDetached(String executable, List<String> arguments) async {
    detached.add(arguments);
  }

  @override
  Future<int> runInherited(String executable, List<String> arguments, {String? workingDirectory}) async => 0;
}

ProcessResult ok(String stdout) => ProcessResult(0, 0, stdout, '');
ProcessResult notReady() => ProcessResult(0, 1, '', "error: device 'emulator-5554' not found");

Emulators emulatorsWith(FakeRunner runner, {String avdHome = '/avd', DateTime Function()? now}) => Emulators(
      sdk: AndroidSdk('/sdk', windows: false),
      runner: runner,
      avdHome: avdHome,
      sleep: (_) async {},
      now: now,
    );

void main() {
  test('parses adb devices output', () {
    const out = 'List of devices attached\nemulator-5554\tdevice\r\nPHONESERIAL\tunauthorized\n\n';
    expect(parseAdbDevices(out), {'emulator-5554': 'device', 'PHONESERIAL': 'unauthorized'});
  });

  test('test boots use the clean snapshot and throw changes away', () {
    final args = bootArgs(testEmulators.first);
    expect(args, containsAllInOrder(['-avd', 'chat_e2e_1', '-port', '5554']));
    expect(args, containsAll(['-snapshot', 'clean', '-no-snapshot-save', '-no-window']));
    expect(firstBootArgs(testEmulators.first), contains('-no-snapshot-load'));
  });

  test('waitBooted returns once boot_completed is 1', () async {
    var polls = 0;
    final runner = FakeRunner((exe, args) {
      if (args.contains('getprop')) return ++polls < 3 ? notReady() : ok('1\n');
      return ok('');
    });
    await emulatorsWith(runner).waitBooted(testEmulators.first);
    expect(polls, 3);
  });

  test('waitBooted gives up after the boot timeout', () async {
    var clock = DateTime(2026);
    final runner = FakeRunner((exe, args) {
      clock = clock.add(const Duration(seconds: 61));
      return ok('0\n');
    });
    await expectLater(
      emulatorsWith(runner, now: () => clock).waitBooted(testEmulators.first),
      throwsA(isA<TimeoutException>()),
    );
  });

  test('up leaves an already running test emulator alone', () async {
    final runner = FakeRunner((exe, args) {
      if (args.first == 'devices') return ok('List of devices attached\nemulator-5554\tdevice\n');
      if (args.contains('name')) return ok('chat_e2e_1\r\nOK\r\n');
      if (args.contains('getprop')) return ok('1\n');
      return ok('');
    });
    final started = await emulatorsWith(runner).up(testEmulators);
    expect(started.map((s) => s.avd), ['chat_e2e_2']);
    expect(runner.detached.single, containsAllInOrder(['-avd', 'chat_e2e_2']));
  });

  test('up refuses a port taken by another AVD', () async {
    final runner = FakeRunner((exe, args) {
      if (args.first == 'devices') return ok('List of devices attached\nemulator-5554\tdevice\n');
      if (args.contains('name')) return ok('flutter_emulator\r\nOK\r\n');
      return ok('');
    });
    await expectLater(emulatorsWith(runner).up([testEmulators.first]), throwsA(isA<StateError>()));
    expect(runner.detached, isEmpty);
  });

  test('ensure creates a missing AVD and saves its clean snapshot', () async {
    final avdHome = await Directory.systemTemp.createTemp('avd_home_');
    addTearDown(() => avdHome.delete(recursive: true));
    final runner = FakeRunner((exe, args) {
      if (args.first == 'devices') return ok('List of devices attached\n');
      if (args.contains('getprop')) return ok('1\n');
      return ok('');
    });
    await emulatorsWith(runner, avdHome: avdHome.path).ensure(testEmulators.first);
    expect(runner.calls, contains('flutter emulators --create --name chat_e2e_1'));
    expect(runner.detached.single, contains('-no-snapshot-load'));
    expect(runner.calls.any((c) => c.endsWith('-s emulator-5554 emu avd snapshot save clean')), isTrue);
    expect(runner.calls.any((c) => c.endsWith('-s emulator-5554 emu kill')), isTrue);
  });

  test('locates the SDK from ANDROID_HOME first', () {
    final sdk = AndroidSdk.locate(environment: {'ANDROID_HOME': '/opt/sdk', 'HOME': '/home/u'}, windows: false);
    expect(sdk.root, '/opt/sdk');
  });

  test('finds the AVD folder from ANDROID_AVD_HOME, then the home folder', () {
    expect(defaultAvdHome(environment: {'ANDROID_AVD_HOME': '/avds'}, windows: false), '/avds');
    expect(defaultAvdHome(environment: {'HOME': '/home/u'}, windows: false), endsWith('avd'));
  });
}
```

- [ ] **Step 3: 테스트가 실패하는지 확인한다**

Run: `cd /c/ChatProject/tools/e2e && dart test`
Expected: 컴파일 실패 — `Error: Couldn't resolve the package 'chat_e2e'` 또는 `lib/src/android_sdk.dart`를 찾지 못함.

- [ ] **Step 4: 구현한다**

`tools/e2e/lib/src/process_runner.dart`:

```dart
import 'dart:io';

/// Runs external programs. Tests swap in a fake so they never touch real devices.
abstract interface class ProcessRunner {
  /// Runs [executable] to completion and returns its output.
  Future<ProcessResult> run(String executable, List<String> arguments, {String? stdin, String? workingDirectory});

  /// Starts [executable] and returns at once; the program keeps running (emulators).
  Future<void> startDetached(String executable, List<String> arguments);

  /// Runs [executable] with its output shown in this terminal; returns the exit code.
  Future<int> runInherited(String executable, List<String> arguments, {String? workingDirectory});
}

class IoProcessRunner implements ProcessRunner {
  const IoProcessRunner();

  @override
  Future<ProcessResult> run(String executable, List<String> arguments,
      {String? stdin, String? workingDirectory}) async {
    // runInShell lets Windows find .bat launchers such as flutter.bat.
    final process = await Process.start(executable, arguments,
        workingDirectory: workingDirectory, runInShell: Platform.isWindows);
    if (stdin != null) process.stdin.write(stdin);
    await process.stdin.close();
    final out = process.stdout.transform(systemEncoding.decoder).join();
    final err = process.stderr.transform(systemEncoding.decoder).join();
    final code = await process.exitCode;
    return ProcessResult(process.pid, code, await out, await err);
  }

  @override
  Future<void> startDetached(String executable, List<String> arguments) async {
    await Process.start(executable, arguments, mode: ProcessStartMode.detached);
  }

  @override
  Future<int> runInherited(String executable, List<String> arguments, {String? workingDirectory}) async {
    final process = await Process.start(executable, arguments,
        workingDirectory: workingDirectory, runInShell: Platform.isWindows, mode: ProcessStartMode.inheritStdio);
    return process.exitCode;
  }
}
```

`tools/e2e/lib/src/android_sdk.dart`:

```dart
import 'dart:io';

import 'package:path/path.dart' as p;

/// Paths of the Android SDK tools the device tool drives.
class AndroidSdk {
  AndroidSdk(this.root, {required this.windows});

  /// Finds the SDK from ANDROID_HOME, ANDROID_SDK_ROOT, or the default install folder.
  factory AndroidSdk.locate({Map<String, String>? environment, bool? windows}) {
    final env = environment ?? Platform.environment;
    final onWindows = windows ?? Platform.isWindows;
    final fromEnv = env['ANDROID_HOME'] ?? env['ANDROID_SDK_ROOT'];
    if (fromEnv != null && fromEnv.isNotEmpty) return AndroidSdk(fromEnv, windows: onWindows);
    final base = onWindows ? env['LOCALAPPDATA'] : env['HOME'];
    if (base == null) throw StateError('Android SDK not found. Set ANDROID_HOME.');
    return AndroidSdk(p.join(base, 'Android', 'Sdk'), windows: onWindows);
  }

  final String root;
  final bool windows;

  String get adb => p.join(root, 'platform-tools', windows ? 'adb.exe' : 'adb');
  String get emulator => p.join(root, 'emulator', windows ? 'emulator.exe' : 'emulator');
}

/// Folder that holds `<name>.avd` folders: ANDROID_AVD_HOME, ANDROID_USER_HOME/avd,
/// or `~/.android/avd`.
String defaultAvdHome({Map<String, String>? environment, bool? windows}) {
  final env = environment ?? Platform.environment;
  final avdHome = env['ANDROID_AVD_HOME'];
  if (avdHome != null && avdHome.isNotEmpty) return avdHome;
  final userHome = env['ANDROID_USER_HOME'];
  if (userHome != null && userHome.isNotEmpty) return p.join(userHome, 'avd');
  final home = (windows ?? Platform.isWindows) ? env['USERPROFILE'] : env['HOME'];
  if (home == null) throw StateError('Cannot find the home folder for AVDs. Set ANDROID_AVD_HOME.');
  return p.join(home, '.android', 'avd');
}
```

`tools/e2e/lib/src/emulators.dart`:

```dart
import 'dart:async';
import 'dart:io';

import 'package:path/path.dart' as p;

import 'android_sdk.dart';
import 'process_runner.dart';

/// One test emulator: an AVD name and the console port that fixes its adb serial.
class EmulatorSpec {
  const EmulatorSpec(this.avd, this.port);

  final String avd;
  final int port;

  String get serial => 'emulator-$port';
}

/// Test-only AVDs (test automation design 5). The user's own flutter_emulator* AVDs
/// are never started or stopped by this tool.
const testEmulators = [EmulatorSpec('chat_e2e_1', 5554), EmulatorSpec('chat_e2e_2', 5556)];

/// Snapshot taken right after the first full boot; every test boot starts from it.
const cleanSnapshot = 'clean';

/// Arguments for a test boot: clean snapshot, no window, changes thrown away on exit.
List<String> bootArgs(EmulatorSpec spec) => [
      '-avd', spec.avd,
      '-port', '${spec.port}',
      '-snapshot', cleanSnapshot,
      '-no-snapshot-save',
      '-no-window', '-no-audio', '-no-boot-anim',
    ];

/// Arguments for the one-time full boot that produces the clean snapshot.
List<String> firstBootArgs(EmulatorSpec spec) => [
      '-avd', spec.avd,
      '-port', '${spec.port}',
      '-no-snapshot-load',
      '-no-window', '-no-audio', '-no-boot-anim',
    ];

/// Parses `adb devices` output into serial → state ("device", "offline", ...).
Map<String, String> parseAdbDevices(String output) {
  final devices = <String, String>{};
  for (final line in output.split('\n').skip(1)) {
    final parts = line.trim().split(RegExp(r'\s+'));
    if (parts.length >= 2) devices[parts[0]] = parts[1];
  }
  return devices;
}

class Emulators {
  Emulators({
    required this.sdk,
    required this.runner,
    required this.avdHome,
    this.flutter = 'flutter',
    this.pollInterval = const Duration(seconds: 2),
    this.bootTimeout = const Duration(seconds: 180),
    Future<void> Function(Duration)? sleep,
    DateTime Function()? now,
  })  : _sleep = sleep ?? Future<void>.delayed,
        _now = now ?? DateTime.now;

  final AndroidSdk sdk;
  final ProcessRunner runner;
  final String avdHome;
  final String flutter;
  final Duration pollInterval;
  final Duration bootTimeout;
  final Future<void> Function(Duration) _sleep;
  final DateTime Function() _now;

  Future<Map<String, String>> devices() async {
    final result = await runner.run(sdk.adb, ['devices']);
    return parseAdbDevices(result.stdout as String);
  }

  /// Name of the AVD running on [spec]'s port, or null when the port is free.
  Future<String?> runningAvd(EmulatorSpec spec) async {
    if ((await devices())[spec.serial] != 'device') return null;
    final result = await runner.run(sdk.adb, ['-s', spec.serial, 'emu', 'avd', 'name']);
    return (result.stdout as String).split(RegExp(r'\r?\n')).first.trim();
  }

  /// True when [spec] already runs. Throws when another AVD holds its port, so tests
  /// never run on (or stop) the user's own emulator.
  Future<bool> isRunning(EmulatorSpec spec) async {
    final name = await runningAvd(spec);
    if (name == null) return false;
    if (name != spec.avd) {
      throw StateError('${spec.serial} is taken by AVD "$name". Stop that emulator and run again.');
    }
    return true;
  }

  bool hasCleanSnapshot(EmulatorSpec spec) =>
      Directory(p.join(avdHome, '${spec.avd}.avd', 'snapshots', cleanSnapshot)).existsSync();

  /// Creates the AVD and its clean snapshot when they are missing.
  Future<void> ensure(EmulatorSpec spec) async {
    final list = await runner.run(sdk.emulator, ['-list-avds']);
    final avds = (list.stdout as String).split(RegExp(r'\r?\n')).map((line) => line.trim()).toSet();
    if (!avds.contains(spec.avd)) {
      final created = await runner.run(flutter, ['emulators', '--create', '--name', spec.avd]);
      if (created.exitCode != 0) throw StateError('Cannot create ${spec.avd}: ${created.stderr}');
    }
    if (hasCleanSnapshot(spec)) return;
    if (!await isRunning(spec)) {
      await runner.startDetached(sdk.emulator, firstBootArgs(spec));
      await waitBooted(spec);
    }
    await runner.run(sdk.adb, ['-s', spec.serial, 'emu', 'avd', 'snapshot', 'save', cleanSnapshot]);
    await stop(spec);
  }

  /// Boots the emulators in [specs] that are not running; returns the ones it started.
  Future<List<EmulatorSpec>> up(List<EmulatorSpec> specs) async {
    final started = <EmulatorSpec>[];
    for (final spec in specs) {
      if (await isRunning(spec)) continue;
      await runner.startDetached(sdk.emulator, bootArgs(spec));
      started.add(spec);
    }
    for (final spec in started) {
      await waitBooted(spec);
    }
    return started;
  }

  Future<void> waitBooted(EmulatorSpec spec) async {
    final deadline = _now().add(bootTimeout);
    while (true) {
      final result = await runner.run(sdk.adb, ['-s', spec.serial, 'shell', 'getprop', 'sys.boot_completed']);
      if (result.exitCode == 0 && (result.stdout as String).trim() == '1') return;
      if (!_now().isBefore(deadline)) {
        throw TimeoutException('${spec.serial} did not finish booting within ${bootTimeout.inSeconds} s');
      }
      await _sleep(pollInterval);
    }
  }

  Future<void> stop(EmulatorSpec spec) async {
    await runner.run(sdk.adb, ['-s', spec.serial, 'emu', 'kill']);
    final deadline = _now().add(const Duration(seconds: 30));
    while ((await devices()).containsKey(spec.serial)) {
      if (!_now().isBefore(deadline)) throw TimeoutException('${spec.serial} did not stop within 30 s');
      await _sleep(pollInterval);
    }
  }
}
```

- [ ] **Step 5: 테스트가 통과하는지 확인한다**

Run: `cd /c/ChatProject/tools/e2e && dart test`
Expected: `All tests passed!` (9개)

Run: `dart analyze`
Expected: `No issues found!`

- [ ] **Step 6: 명령줄 도구를 쓴다**

`tools/e2e/bin/devices.dart`:

```dart
import 'dart:io';

import 'package:args/args.dart';
import 'package:chat_e2e/src/android_sdk.dart';
import 'package:chat_e2e/src/emulators.dart';
import 'package:chat_e2e/src/process_runner.dart';
import 'package:path/path.dart' as p;

const usage = '''
Usage: dart run tools/e2e/bin/devices.dart <command> [options] [flutter test arguments]

Commands:
  ensure   create the test AVDs and their clean snapshots if missing
  up       boot the test emulators and wait until they finish booting
  down     stop the test emulators
  test     boot, run `flutter test <arguments> -d emulator-5554` in app/, then stop
           the emulators this command started

Options:
  --count <1|2>   how many test emulators (default 1)
  --keep          test: leave the emulators running afterwards
''';

Future<void> main(List<String> arguments) async {
  final parser = ArgParser()
    ..addOption('count', defaultsTo: '1', allowed: ['1', '2'])
    ..addFlag('keep', negatable: false);
  final ArgResults options;
  try {
    options = parser.parse(arguments);
  } on FormatException catch (e) {
    stderr.writeln('${e.message}\n\n$usage');
    exit(64);
  }
  if (options.rest.isEmpty) {
    stderr.write(usage);
    exit(64);
  }

  final specs = testEmulators.take(int.parse(options['count'] as String)).toList();
  final emulators = Emulators(sdk: AndroidSdk.locate(), runner: const IoProcessRunner(), avdHome: defaultAvdHome());

  switch (options.rest.first) {
    case 'ensure':
      for (final spec in specs) {
        await emulators.ensure(spec);
      }
    case 'up':
      for (final spec in specs) {
        await emulators.ensure(spec);
      }
      final started = await emulators.up(specs);
      stdout.writeln('ready: ${specs.map((s) => s.serial).join(' ')} (started ${started.length})');
    case 'down':
      for (final spec in specs) {
        if (await emulators.isRunning(spec)) await emulators.stop(spec);
      }
    case 'test':
      for (final spec in specs) {
        await emulators.ensure(spec);
      }
      final started = await emulators.up(specs);
      final appDir = p.normalize(p.join(p.dirname(Platform.script.toFilePath()), '..', '..', '..', 'app'));
      final flutterArgs = ['test', ...options.rest.skip(1), '-d', specs.first.serial];
      final code = await const IoProcessRunner().runInherited('flutter', flutterArgs, workingDirectory: appDir);
      if (!(options['keep'] as bool)) {
        for (final spec in started) {
          await emulators.stop(spec);
        }
      }
      exit(code);
    default:
      stderr.write(usage);
      exit(64);
  }
}
```

Run: `cd /c/ChatProject/tools/e2e && dart analyze`
Expected: `No issues found!`

- [ ] **Step 7: 실제 에뮬레이터로 확인한다**

처음 한 번은 AVD를 만들고 완전 부팅하므로 몇 분 걸린다. 남은 메모리가 6 GB 이상인지 먼저 본다.

```bash
cd /c/ChatProject
dart run tools/e2e/bin/devices.dart ensure --count 2
ls ~/.android/avd/chat_e2e_1.avd/snapshots/clean ~/.android/avd/chat_e2e_2.avd/snapshots/clean
```

Expected: 오류 없이 끝나고 두 `clean` 폴더가 있다.

```bash
dart run tools/e2e/bin/devices.dart up --count 2
/c/Users/user/AppData/Local/Android/Sdk/platform-tools/adb.exe devices
dart run tools/e2e/bin/devices.dart down --count 2
/c/Users/user/AppData/Local/Android/Sdk/platform-tools/adb.exe devices
```

Expected:
- `up`: `ready: emulator-5554 emulator-5556 (started 2)`
- 첫 `adb devices`: 두 시리얼이 모두 `device`
- `down` 뒤 `adb devices`: 두 시리얼이 없다

```bash
dart run tools/e2e/bin/devices.dart test integration_test/core_test.dart
```

Expected: 디버그 APK를 빌드해 `emulator-5554`에서 돌리고 `All tests passed!` (3개)를 보인다. 끝나면 에뮬레이터가 꺼진다.

- [ ] **Step 8: 커밋**

```bash
cd /c/ChatProject
git add tools/e2e/
git commit -m "test(tools): device tool that boots clean test emulators and runs Flutter tests"
```

---

### Task 5: UniFFI 다리 — Kotlin이 같은 코어에 닿는지

**Files:**
- Create: `tools/uniffi-bindgen/Cargo.toml`, `tools/uniffi-bindgen/src/main.rs`
- Create: `app/rust/src/native.rs`, `app/integration_test/native_bridge_test.dart`
- Create (생성됨): `app/android/app/src/main/kotlin/uniffi/chat_ffi/chat_ffi.kt`
- Modify: `Cargo.toml`, `app/rust/Cargo.toml`, `app/rust/src/lib.rs`, `app/android/app/build.gradle.kts`, `app/android/app/src/main/kotlin/dev/chatproject/chat_app/MainActivity.kt`

**Interfaces:**
- Consumes: Task 3의 `chat_ffi::global::CORE`, `chat_ffi::api::chat::{core_start, core_version}`. Task 4의 `devices.dart test` 명령.
- Produces:
  - Rust(UniFFI): `native_core_version() -> String`, `native_instance_id() -> Option<String>`
  - Kotlin(패키지 `uniffi.chat_ffi`): `fun nativeCoreVersion(): String`, `fun nativeInstanceId(): String?`
  - Android MethodChannel: 이름 `chat_app/native_core`, 메서드 `instanceId` → `String?`

- [ ] **Step 1: 실패하는 Rust 테스트를 쓴다**

`app/rust/Cargo.toml`의 `[dependencies]`에 한 줄을 넣고, 아래에 `[dev-dependencies]`를 붙인다:

```toml
uniffi = "=0.32.2"
```

```toml
[dev-dependencies]
tempfile = "3"
```

`app/rust/src/lib.rs`:

```rust
pub mod api;
mod convert;
mod frb_generated;
mod global;
mod native;

uniffi::setup_scaffolding!();
```

`app/rust/src/native.rs` (아직 테스트만):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_id_is_none_before_start_and_matches_after() {
        assert_eq!(native_instance_id(), None);
        let tmp = tempfile::tempdir().unwrap();
        let info = crate::api::chat::core_start(tmp.path().to_string_lossy().into_owned()).unwrap();
        assert_eq!(native_instance_id(), Some(info.instance_id));
        assert_eq!(native_core_version(), crate::api::chat::core_version());
    }
}
```

이 크레이트에서 전역 코어를 시작하는 테스트는 이것 하나뿐이어야 한다. 테스트들이 한 프로세스를 같이 쓰기 때문이다.

- [ ] **Step 2: 테스트가 실패하는지 확인한다**

Run: `cd /c/ChatProject && cargo test -p chat_ffi native`
Expected: 컴파일 실패 — `cannot find function `native_instance_id``.

- [ ] **Step 3: UniFFI 함수를 구현한다**

`app/rust/src/native.rs`의 테스트 모듈 **위에** 넣는다:

```rust
//! Entry points for native code that runs without Flutter: the Android push receiver
//! (Kotlin) now, the iOS notification extension (Swift) in M1b (design 12.3).

use crate::global::CORE;

/// Version of the core in this library.
#[uniffi::export]
pub fn native_core_version() -> String {
    chat_core::CORE_VERSION.to_string()
}

/// Instance id of the core running in this process, or `None` before the first start.
/// Equals the id Dart got from `core_start` when both run in one process.
#[uniffi::export]
pub fn native_instance_id() -> Option<String> {
    CORE.info().map(|info| info.instance_id)
}
```

Run: `cargo test -p chat_ffi`
Expected: 3개 통과.

- [ ] **Step 4: Kotlin 바인딩을 생성한다**

`tools/uniffi-bindgen/Cargo.toml`:

```toml
[package]
name = "uniffi-bindgen"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
uniffi = { version = "=0.32.2", features = ["cli"] }
```

`tools/uniffi-bindgen/src/main.rs`:

```rust
fn main() {
    uniffi::uniffi_bindgen_main()
}
```

`Cargo.toml`의 `members`:

```toml
members = ["core/chat_core", "app/rust", "tools/uniffi-bindgen"]
```

```bash
cd /c/ChatProject
cargo build -p chat_ffi
cargo run -p uniffi-bindgen -- generate --library target/debug/chat_ffi.dll --language kotlin --out-dir app/android/app/src/main/kotlin --no-format
grep -n "fun nativeInstanceId\|fun nativeCoreVersion" app/android/app/src/main/kotlin/uniffi/chat_ffi/chat_ffi.kt
```

Expected: `app/android/app/src/main/kotlin/uniffi/chat_ffi/chat_ffi.kt`가 생기고, `grep`이 두 줄을 보인다.

- [ ] **Step 5: Android 쪽을 연결한다**

`app/android/app/build.gradle.kts`의 `defaultConfig`에서 한 줄을 바꾼다:

```kotlin
        minSdk = 26
```

파일 끝에 붙인다. UniFFI가 생성한 Kotlin은 JNA로 `libchat_ffi.so`를 연다. 이 `.so`는 Flutter가 이미 연 것과 같은 파일이다.

```kotlin
dependencies {
    implementation("net.java.dev.jna:jna:5.17.0@aar")
}
```

`app/android/app/src/main/kotlin/dev/chatproject/chat_app/MainActivity.kt`:

```kotlin
package dev.chatproject.chat_app

import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import uniffi.chat_ffi.nativeInstanceId

class MainActivity : FlutterActivity() {
    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        // Lets Dart check that Kotlin reaches the same Rust core (design 12.3).
        MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "chat_app/native_core")
            .setMethodCallHandler { call, result ->
                when (call.method) {
                    "instanceId" -> result.success(nativeInstanceId())
                    else -> result.notImplemented()
                }
            }
    }
}
```

- [ ] **Step 6: Android 통합 테스트를 쓰고 에뮬레이터에서 돌린다**

`app/integration_test/native_bridge_test.dart`:

```dart
import 'dart:io';

import 'package:chat_app/src/rust/api/chat.dart';
import 'package:chat_app/src/rust/frb_generated.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  setUpAll(() async => RustLib.init());

  test('Kotlin reaches the same Rust core as Dart', () async {
    final dataDir = await Directory.systemTemp.createTemp('chat_native_it_');
    final info = await coreStart(dataDir: dataDir.path);
    const channel = MethodChannel('chat_app/native_core');
    final fromKotlin = await channel.invokeMethod<String>('instanceId');
    expect(fromKotlin, info.instanceId);
  }, skip: !Platform.isAndroid);
}
```

Run: `cd /c/ChatProject && dart run tools/e2e/bin/devices.dart test integration_test`
Expected: `emulator-5554`에서 `integration_test/` 안의 두 파일이 모두 돌고 `All tests passed!` (4개).
- `UnsatisfiedLinkError`가 나면 JNA 의존성(Step 5)을 확인한다.
- `instanceId` 값이 다르면 Kotlin이 `.so`를 따로 하나 더 연 것이다. 이 경우 설계 12.3의 전제가 깨지므로 진행하지 말고 보고한다.

- [ ] **Step 7: 커밋**

```bash
cd /c/ChatProject
git add Cargo.toml Cargo.lock tools/ app/
git commit -m "feat(app): UniFFI entry points shared with the Flutter core on Android"
```

---

### Task 6: 앱 셸 — 테마 토큰과 첫 화면

**Files:**
- Create: `app/lib/theme/tokens.dart`, `app/lib/theme/app_theme.dart`, `app/lib/core_client.dart`, `app/lib/home/home_screen.dart`, `app/test/home_screen_test.dart`
- Modify: `app/lib/main.dart`

**Interfaces:**
- Consumes: Task 3의 Dart API(`coreVersion`, `coreStart`, `events`, `CoreInfo`, `CoreError`, `ErrorCode`, `ChatEvent`, `ChatEvent_ConnectionState`). Task 4의 `devices.dart up`·`down` 명령.
- Produces:
  - `abstract interface class CoreClient { String get version; Future<CoreInfo> start(); Stream<ChatEvent> events(); }`
  - `class FrbCoreClient implements CoreClient`
  - `abstract final class AppColors` (색 토큰)
  - `ThemeData buildAppTheme()`
  - `String connectionLabel(ChatEvent_ConnectionState state)`
  - `class HomeScreen extends StatefulWidget { HomeScreen({required CoreClient core}) }`
  - `class ChatApp extends StatelessWidget { ChatApp({required CoreClient core}) }`

- [ ] **Step 1: 색 토큰과 테마를 만든다**

`app/lib/theme/tokens.dart`:

```dart
import 'dart:ui';

/// Every color of the app. Provisional palette (slate + mint, 2026-10-04): chosen for
/// testing and likely to change, so change colors here only.
abstract final class AppColors {
  static const background = Color(0xFF1F2630);
  static const surface = Color(0xFF29313D);
  static const surfaceRaised = Color(0xFF35404E);
  static const bubbleOther = Color(0xFF2F3845);
  static const text = Color(0xFFF1F4F8);
  static const textSecondary = Color(0xFFCDD4DE);
  static const textMuted = Color(0xFFA3ADBB);
  static const accent = Color(0xFF6FD6C5);
  static const accentHover = Color(0xFF9CE6DA);
  static const onAccent = Color(0xFF1F2630);
  static const warning = Color(0xFFF2B04B);
}
```

`app/lib/theme/app_theme.dart`:

```dart
import 'package:flutter/material.dart';

import 'tokens.dart';

ThemeData buildAppTheme() {
  const scheme = ColorScheme.dark(
    primary: AppColors.accent,
    onPrimary: AppColors.onAccent,
    secondary: AppColors.accent,
    onSecondary: AppColors.onAccent,
    surface: AppColors.surface,
    onSurface: AppColors.text,
    error: AppColors.warning,
    onError: AppColors.onAccent,
  );
  return ThemeData(colorScheme: scheme, scaffoldBackgroundColor: AppColors.background);
}
```

- [ ] **Step 2: 실패하는 위젯 테스트를 쓴다**

`app/test/home_screen_test.dart`:

```dart
import 'dart:async';

import 'package:chat_app/core_client.dart';
import 'package:chat_app/home/home_screen.dart';
import 'package:chat_app/src/rust/api/chat.dart';
import 'package:chat_app/theme/app_theme.dart';
import 'package:chat_app/theme/tokens.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

class FakeCoreClient implements CoreClient {
  FakeCoreClient({this.startError});

  final Object? startError;
  final controller = StreamController<ChatEvent>.broadcast();

  @override
  String get version => '9.9.9-test';

  @override
  Future<CoreInfo> start() async {
    final error = startError;
    if (error != null) throw error;
    return const CoreInfo(version: '9.9.9-test', instanceId: 'test');
  }

  @override
  Stream<ChatEvent> events() => controller.stream;
}

Future<FakeCoreClient> pumpHome(WidgetTester tester, {Object? startError}) async {
  final core = FakeCoreClient(startError: startError);
  await tester.pumpWidget(MaterialApp(theme: buildAppTheme(), home: HomeScreen(core: core)));
  await tester.pump();
  return core;
}

void main() {
  testWidgets('shows the empty state and the core version', (tester) async {
    await pumpHome(tester);
    expect(find.text('아직 대화가 없어요'), findsOneWidget);
    expect(find.text('코어 9.9.9-test'), findsOneWidget);
    expect(find.text('코어 시작 중'), findsOneWidget);
  });

  testWidgets('shows the connection state from the event stream', (tester) async {
    final core = await pumpHome(tester);
    core.controller.add(const ChatEvent.connectionState(directPeers: 2, mailboxOk: false));
    await tester.pump();
    expect(find.text('직접 연결 2'), findsOneWidget);
  });

  testWidgets('shows why the core did not start', (tester) async {
    await pumpHome(tester, startError: const CoreError(code: ErrorCode.invalidConfig, detail: 'no folder'));
    await tester.pump();
    expect(find.text('코어를 시작하지 못했어요 · invalidConfig'), findsOneWidget);
  });

  test('connection labels', () {
    expect(connectionLabel(const ChatEvent_ConnectionState(directPeers: 0, mailboxOk: false)), '연결 없음');
    expect(connectionLabel(const ChatEvent_ConnectionState(directPeers: 0, mailboxOk: true)), '메일박스 연결됨');
    expect(
      connectionLabel(const ChatEvent_ConnectionState(directPeers: 0, mailboxOk: false, queueOk: true)),
      '서버 큐 사용 중',
    );
  });

  test('theme takes its colors from the tokens', () {
    final theme = buildAppTheme();
    expect(theme.scaffoldBackgroundColor, AppColors.background);
    expect(theme.colorScheme.primary, AppColors.accent);
  });
}
```

- [ ] **Step 3: 테스트가 실패하는지 확인한다**

Run: `cd /c/ChatProject/app && flutter test test/home_screen_test.dart`
Expected: 컴파일 실패 — `package:chat_app/core_client.dart`를 찾지 못함.

- [ ] **Step 4: CoreClient와 첫 화면을 구현한다**

`app/lib/core_client.dart`:

```dart
import 'dart:io';

import 'package:path_provider/path_provider.dart';

import 'src/rust/api/chat.dart' as rust;

/// The UI reaches the Rust core only through this interface, so widget tests can use
/// a fake core.
abstract interface class CoreClient {
  String get version;
  Future<rust.CoreInfo> start();
  Stream<rust.ChatEvent> events();
}

class FrbCoreClient implements CoreClient {
  @override
  String get version => rust.coreVersion();

  @override
  Future<rust.CoreInfo> start() async {
    final support = await getApplicationSupportDirectory();
    return rust.coreStart(dataDir: '${support.path}${Platform.pathSeparator}core');
  }

  @override
  Stream<rust.ChatEvent> events() => rust.events();
}
```

`app/lib/home/home_screen.dart`:

```dart
import 'dart:async';

import 'package:flutter/material.dart';

import '../core_client.dart';
import '../src/rust/api/chat.dart';
import '../theme/tokens.dart';

/// Short label for the connection status line.
String connectionLabel(ChatEvent_ConnectionState state) {
  if (state.directPeers > 0) return '직접 연결 ${state.directPeers}';
  if (state.mailboxOk) return '메일박스 연결됨';
  if (state.queueOk ?? false) return '서버 큐 사용 중';
  return '연결 없음';
}

class HomeScreen extends StatefulWidget {
  const HomeScreen({super.key, required this.core});

  final CoreClient core;

  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> {
  StreamSubscription<ChatEvent>? _events;
  ChatEvent_ConnectionState? _connection;
  String? _startError;

  @override
  void initState() {
    super.initState();
    // Subscribe before starting: the core replays the latest connection state anyway.
    _events = widget.core.events().listen((event) {
      if (event is ChatEvent_ConnectionState) {
        setState(() => _connection = event);
      }
    });
    widget.core.start().then(
      (_) {},
      onError: (Object error) {
        if (!mounted) return;
        setState(() => _startError = error is CoreError ? error.code.name : '$error');
      },
    );
  }

  @override
  void dispose() {
    _events?.cancel();
    super.dispose();
  }

  String get _status {
    final error = _startError;
    if (error != null) return '코어를 시작하지 못했어요 · $error';
    final connection = _connection;
    if (connection == null) return '코어 시작 중';
    return connectionLabel(connection);
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      body: SafeArea(
        child: Padding(
          padding: const EdgeInsets.fromLTRB(22, 16, 22, 16),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              const Text(
                '대화',
                style: TextStyle(fontSize: 30, fontWeight: FontWeight.w700, color: AppColors.text),
              ),
              const SizedBox(height: 16),
              Container(
                width: double.infinity,
                padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 14),
                decoration: const BoxDecoration(
                  color: AppColors.surface,
                  borderRadius: BorderRadius.all(Radius.circular(16)),
                ),
                child: Text(_status, style: const TextStyle(fontSize: 14, color: AppColors.textSecondary)),
              ),
              const Expanded(
                child: Center(
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Text(
                        '아직 대화가 없어요',
                        style: TextStyle(fontSize: 22, fontWeight: FontWeight.w600, color: AppColors.text),
                      ),
                      SizedBox(height: 12),
                      Text(
                        '만나서 서로 QR을 찍으면 첫 연락처가 생겨요.',
                        textAlign: TextAlign.center,
                        style: TextStyle(fontSize: 15, color: AppColors.textSecondary),
                      ),
                    ],
                  ),
                ),
              ),
              Text('코어 ${widget.core.version}', style: const TextStyle(fontSize: 12, color: AppColors.textMuted)),
            ],
          ),
        ),
      ),
    );
  }
}
```

`app/lib/main.dart`를 통째로 바꾼다:

```dart
import 'package:flutter/material.dart';

import 'core_client.dart';
import 'home/home_screen.dart';
import 'src/rust/frb_generated.dart';
import 'theme/app_theme.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await RustLib.init();
  runApp(ChatApp(core: FrbCoreClient()));
}

class ChatApp extends StatelessWidget {
  const ChatApp({super.key, required this.core});

  final CoreClient core;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'ChatProject',
      theme: buildAppTheme(),
      home: HomeScreen(core: core),
    );
  }
}
```

- [ ] **Step 5: 테스트가 통과하는지 확인한다**

Run: `cd /c/ChatProject/app && flutter test`
Expected: `All tests passed!` (5개)

Run: `flutter analyze`
Expected: `No issues found!`

- [ ] **Step 6: 릴리스 빌드를 확인한다**

```bash
cd /c/ChatProject/app
flutter build windows --release
./build/windows/x64/runner/Release/chat_app.exe &
```

Expected (화면을 보고 확인):
- "대화"
- 상태 줄 "연결 없음"
- "아직 대화가 없어요"
- 맨 아래 "코어 0.1.0"

확인한 뒤 창을 닫는다.

```bash
flutter build apk --release
cd /c/ChatProject
dart run tools/e2e/bin/devices.dart up
ADB=/c/Users/user/AppData/Local/Android/Sdk/platform-tools/adb.exe
"$ADB" -s emulator-5554 install -r app/build/app/outputs/flutter-apk/app-release.apk
"$ADB" -s emulator-5554 shell am start -W -n dev.chatproject.chat_app/.MainActivity
sleep 3
mkdir -p build
"$ADB" -s emulator-5554 exec-out screencap -p > build/m1a-release-emulator.png
dart run tools/e2e/bin/devices.dart down
```

Expected: `build/m1a-release-emulator.png`에 Windows와 같은 화면이 찍혀 있다. `ls -l app/build/app/outputs/flutter-apk/app-release.apk`로 APK 크기를 확인해 커밋 메시지 본문에 적는다(이후 크기 비교 기준).

폰이 연결돼 있으면 같은 APK를 `-s PHONESERIAL`로 설치해 한 번 눈으로 본다. 폰이 없으면 이 줄은 건너뛴다.

- [ ] **Step 7: 커밋**

```bash
cd /c/ChatProject
git add app/
git commit -m "feat(app): theme tokens and first screen bound to the core event stream"
```

---

### Task 7: CI — GitHub Actions (에뮬레이터 포함)

**Files:**
- Create: `.github/workflows/ci.yml`
- Modify: `app/analysis_options.yaml`

**Interfaces:**
- Consumes: Task 1–6의 테스트와 빌드 명령
- Produces: `main` 브랜치 push와 PR마다 도는 CI. 작업은 `rust`, `flutter`, `windows`, `android` 네 개다(자동 테스트 설계 8절의 "매 push" 줄).

- [ ] **Step 1: 워크플로를 쓴다**

`.github/workflows/ci.yml`:

```yaml
name: ci

on:
  push:
    branches: [main]
  pull_request:

env:
  FLUTTER_VERSION: 3.44.6
  RUST_TOOLCHAIN: 1.92.0

jobs:
  rust:
    runs-on: ubuntu-latest
    timeout-minutes: 20
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@master
        with:
          toolchain: ${{ env.RUST_TOOLCHAIN }}
          components: clippy
      - run: cargo test --workspace
      - run: cargo clippy --workspace --all-targets -- -D warnings

  flutter:
    runs-on: ubuntu-latest
    timeout-minutes: 15
    steps:
      - uses: actions/checkout@v4
      - uses: subosito/flutter-action@v2
        with:
          flutter-version: ${{ env.FLUTTER_VERSION }}
          channel: stable
      - name: App analysis and widget tests
        working-directory: app
        run: |
          flutter pub get
          flutter analyze
          flutter test
      - name: Device tool tests
        working-directory: tools/e2e
        run: |
          dart pub get
          dart analyze
          dart test

  windows:
    runs-on: windows-latest
    timeout-minutes: 45
    defaults:
      run:
        working-directory: app
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@master
        with:
          toolchain: ${{ env.RUST_TOOLCHAIN }}
      - uses: subosito/flutter-action@v2
        with:
          flutter-version: ${{ env.FLUTTER_VERSION }}
          channel: stable
      - run: flutter pub get
      - run: flutter test integration_test/core_test.dart -d windows
      - run: flutter build windows --release

  android:
    runs-on: ubuntu-latest
    timeout-minutes: 60
    steps:
      - name: Free disk space
        run: |
          # The emulator image and AVD need room. Never touch /usr/local/lib/android
          # (the SDK), Java, or anything the later steps install.
          df -h /
          sudo rm -rf \
            /usr/share/dotnet \
            /usr/local/.ghcup \
            /opt/ghc \
            /usr/share/swift \
            /usr/local/share/boost \
            /opt/hostedtoolcache/CodeQL \
            /usr/local/share/powershell \
            /usr/local/share/chromium
          sudo docker image prune --all --force
          df -h /
      - uses: actions/checkout@v4
      - name: Enable KVM
        run: |
          echo 'KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"' | sudo tee /etc/udev/rules.d/99-kvm4all.rules
          sudo udevadm control --reload-rules
          sudo udevadm trigger --name-match=kvm
      - uses: actions/setup-java@v4
        with:
          distribution: temurin
          java-version: '21'
      - uses: dtolnay/rust-toolchain@master
        with:
          toolchain: ${{ env.RUST_TOOLCHAIN }}
          targets: aarch64-linux-android,armv7-linux-androideabi,x86_64-linux-android,i686-linux-android
      - uses: subosito/flutter-action@v2
        with:
          flutter-version: ${{ env.FLUTTER_VERSION }}
          channel: stable
      - name: Release APK
        working-directory: app
        run: |
          flutter pub get
          flutter build apk --release
      - name: AVD cache
        uses: actions/cache@v4
        id: avd-cache
        with:
          path: |
            ~/.android/avd/*
            ~/.android/adb*
          key: avd-api36-google_apis-x86_64
      - name: Create AVD snapshot for the cache
        if: steps.avd-cache.outputs.cache-hit != 'true'
        uses: reactivecircus/android-emulator-runner@v2
        with:
          api-level: 36
          target: google_apis
          arch: x86_64
          force-avd-creation: false
          emulator-options: -no-window -gpu swiftshader_indirect -noaudio -no-boot-anim -camera-back none
          disable-animations: false
          script: echo "AVD snapshot created"
      - name: Integration tests on the emulator
        uses: reactivecircus/android-emulator-runner@v2
        with:
          api-level: 36
          target: google_apis
          arch: x86_64
          force-avd-creation: false
          emulator-options: -no-snapshot-save -no-window -gpu swiftshader_indirect -noaudio -no-boot-anim -camera-back none
          disable-animations: true
          working-directory: app
          script: flutter test integration_test -d emulator-5554
```

CI의 Android 작업은 `devices.dart` 대신 `reactivecircus/android-emulator-runner`가 에뮬레이터를 켠다. Linux 러너에서 KVM과 스냅숏 캐시를 이 액션이 다루기 때문이다(자동 테스트 설계 5절).

`android` 작업은 맨 앞에서 러너 디스크를 비운다. 첫 CI에서 에뮬레이터 시스템 이미지를 푸는 도중 `No space left on device`로 멈췄기 때문이다. 지우는 곳은 이 작업이 쓰지 않는 도구뿐이다. `/usr/local/lib/android`(SDK)와 Java, 뒤 단계가 설치하는 것은 건드리지 않는다.

깨끗한 체크아웃에서도 `flutter analyze`가 통과하도록 `app/analysis_options.yaml`의 `exclude` 목록에 `rust_builder/**`를 더한다. vendored cargokit Dart 코드는 `build_tool/`에서 `dart pub get`을 돌린 뒤에야 import가 풀리기 때문이다.

```yaml
analyzer:
  exclude:
    - lib/src/rust/**
    - rust_builder/**
```

로컬에서 clippy가 깨끗한지 먼저 본다:

Run: `cd /c/ChatProject && cargo clippy --workspace --all-targets -- -D warnings`
Expected: 경고 없음. 생성 코드(`frb_generated.rs`)에서 경고가 나면 `app/rust/src/lib.rs`의 `mod frb_generated;` 위에 `#[allow(clippy::all)]`을 붙이고 다시 돌린다.

```bash
git add .github/ app/analysis_options.yaml app/rust/src/lib.rs
git commit -m "ci: Rust and widget tests on Linux, Windows integration, Android emulator integration"
```

- [ ] **Step 2: 비공개 저장소를 만들고 push한다 (사용자 승인 후)**

사용자에게 "GitHub `HyunwookYoo/chatproject` 비공개 저장소를 만들고 push해도 되는지" 묻는다. 승인받은 뒤에만 실행한다.

```bash
cd /c/ChatProject
gh repo create chatproject --private --source . --remote origin
git push -u origin main
git push -u origin m1a-core-skeleton
gh pr create --base main --head m1a-core-skeleton \
  --title "M1a: core skeleton — Rust core, Flutter shell, shared FFI, CI" \
  --body "CI를 돌리려고 여는 PR이다. 병합 여부는 브랜치 마무리 단계에서 정한다."
```

`ci.yml`은 `main` push와 `pull_request`에서만 돌기 때문에, CI를 시작하는 것은 PR이다.

Expected: `✓ Created repository HyunwookYoo/chatproject on GitHub`, 두 번의 push 성공, PR 주소 출력.

- [ ] **Step 3: CI가 녹색인지 확인한다**

`gh run watch`는 대화형이 아닌 셸에서 실행 ID가 있어야 한다. ID는 `gh run list --limit 1`에서 얻는다.

```bash
RUN_ID=$(gh run list --limit 1 --json databaseId --jq '.[0].databaseId')
gh run watch "$RUN_ID" --exit-status
```

Expected: `rust`, `flutter`, `windows`, `android` 네 작업이 모두 `✓`.
- 실패하면 로그(`gh run view --log-failed`)의 첫 오류를 고쳐 커밋하고 다시 push한다.
- 고친 내용은 이 계획의 해당 단계에도 반영한다.
- `android` 작업은 첫 실행 때 AVD 스냅숏을 만드느라 더 오래 걸린다. 두 번째 실행부터는 캐시를 쓴다.

---

## 완료 기준 (로드맵 M1a)

- [ ] `cargo test --workspace` 통과 (chat_core 13, chat_ffi 3)
- [ ] `tools/e2e`에서 `dart test` 통과 (14)
- [ ] Windows `integration_test/core_test.dart` 통과
- [ ] `dart run tools/e2e/bin/devices.dart test integration_test`가 시험 전용 에뮬레이터에서 통과 (4). 폰을 연결하지 않아도 된다
- [ ] 릴리스 빌드가 Windows 앱과 에뮬레이터에서 첫 화면을 띄움. 폰은 선택으로 한 번 눈 확인
- [ ] CI 네 작업(`rust`, `flutter`, `windows`, `android`) 녹색

---

## 계획과 달라진 점

위 태스크의 코드 블록은 처음 쓴 그대로 두었다. 실행하면서 판정(Ruling)으로 코드나 명령이 달라진 곳은 아래와 같다. R번호는 그 판정의 번호다. Task 4–6의 블록이 아래 내용과 다르면 아래가 실제 코드다.

- **R8 — cargokit Gradle 플러그인 패치 (Task 3 Step 1, Task 4 Step 7)**: 계획에 없던 손질이다. Gradle 9.1이 `Project.exec()`를 없앴고 `rust_builder`의 `compileSdkVersion 33`은 androidx가 요구하는 34보다 낮아서, 첫 Android 빌드가 `:chat_ffi:cargokitCargoBuildChat_ffiDebug`와 `:chat_ffi:checkDebugAarMetadata`에서 멈췄다. `app/rust_builder/cargokit/gradle/plugin.gradle`가 `ExecOperations`를 주입받아 `execOperations.exec`를 부르고, `app/rust_builder/android/build.gradle`의 `compileSdkVersion`은 33에서 36이 됐다. 커밋 `e42b075`. **`flutter_rust_bridge_codegen integrate`(Task 3 Step 1)를 다시 돌리거나 flutter_rust_bridge를 올리면 이 패치가 덮어써진다. 그때는 `git show e42b075`의 변경을 같은 두 파일에 다시 적용한다.**
- **R9 — 기기 도구의 실패 처리 (Task 4 Step 4·6)**: 계획의 코드는 중간에 실패하면 켜 둔 에뮬레이터를 그대로 둔다. `up`과 `ensure`는 켜기 전에 모든 포트를 확인하고, 이미 켜져 있거나 `offline`인 에뮬레이터까지 부팅이 끝나기를 기다리며, 중간에 실패하면 자기가 켠 에뮬레이터를 끈다. `devices.dart test`는 `try/finally`로 끄고, 최상위에서 오류 메시지를 한 줄 찍은 뒤 종료 코드 1로 끝낸다. 테스트는 14개가 됐다. Task 4 Step 5의 "9개"와 코드 블록은 처음 그대로이고, 완료 기준은 14로 고쳤다. 커밋 `0aa0221`.
- **R10 — UniFFI 무결성 검사 호출 (Task 5 Step 5)**: `MainActivity.kt`의 `MethodChannel` 핸들러가 첫 UniFFI 호출 전에 `uniffiEnsureInitialized()`를 부른다. 계획의 `MainActivity.kt` 블록에는 이 호출이 없다. 생성된 Kotlin 함수는 UniFFI의 계약 버전·체크섬 검사를 돌리지 않아서, 낡은 `chat_ffi.kt`와 새 `.so`가 어긋나도 알 길이 없었다. 불일치하면 `PlatformException`이 아니라 앱이 첫 채널 호출에서 죽는다(`ExceptionInInitializerError`). 이 채널은 릴리스 빌드에도 등록되며, 디버그 빌드로 한정하는 일은 [미룬 일](2026-10-05-m1a-carry-forward.md)의 M2에 있다. 커밋 `4c477f8`.
- **R11 — 위젯 테스트의 `pump()` (Task 6 Step 2)**: 'shows the connection state from the event stream'에서 이벤트를 넣은 뒤 `pump()` 대신 `pumpAndSettle()`을 쓴다. Flutter 3.44.6에서는 방송 이벤트가 `pump()`가 예약된 프레임을 확인한 뒤에 도착해서, 계획의 테스트는 위젯을 하나도 찾지 못하고 실패한다. 구현은 그대로다. 커밋 `7fa0748`.
- **R12 — 코어 라이브러리 로드 실패 화면 (Task 6 Step 4)**: `main()`이 `RustLib.init()`을 `try/catch`로 감싸고, 실패하면 `StartupErrorApp`을 띄워 `코어를 시작하지 못했어요 · <오류>`를 보인다. 계획의 `main.dart`는 `await RustLib.init();` 한 줄이라, 라이브러리를 못 읽으면 빈 화면이 남고 Windows에서는 창도 뜨지 않았다. `app/lib/startup_error_app.dart`와 `app/test/startup_error_app_test.dart`가 새로 생겨 위젯 테스트는 6개가 됐다. 커밋 `b339308`.
- **R13 — 분석 제외 목록 (Task 7 Step 1)**: `app/analysis_options.yaml`의 `analyzer.exclude`에 `rust_builder/**`가 더해졌다. 깨끗한 체크아웃에서 `flutter analyze`가 벤더링된 cargokit `build_tool`의 Dart 코드에서 68건을 보고했다. 이 코드의 import는 `build_tool/`에서 `dart pub get`을 돌린 뒤에야 풀린다. 커밋 `df308b0`. Task 7 본문에는 `39847e8`에서 이미 반영했다.
- **R14 — 디스크 비우기 단계 (Task 7 Step 1)**: `android` 작업의 첫 단계로 `Free disk space`가 생겼다. 첫 CI에서 에뮬레이터 시스템 이미지를 푸는 도중 `No space left on device`로 멈췄다. 이 단계는 작업이 쓰지 않는 도구(.NET, Haskell, Swift, Boost, CodeQL, PowerShell, Chromium)를 지우고 Docker 이미지를 정리한다. 커밋 `df5912b`. Task 7의 YAML 블록은 `39847e8`에서 `ci.yml`과 같게 맞췄다.
- **R15 — 마무리 수정 (Task 5, Task 7, 문서)**: 최종 리뷰 뒤에 네 가지를 더했다. 첫째, `app/android/app/proguard-rules.pro`가 새로 생겼다. 이 파일의 네 줄은 코드 축소기 R8이 JNA와 UniFFI 클래스를 지우거나 이름을 바꾸지 못하게 한다. Flutter의 릴리스 빌드가 R8을 켜는데, JNA는 클래스와 필드를 이름으로 찾기 때문이다. Flutter Gradle 플러그인이 이 파일이 있으면 알아서 쓰므로 `app/android/app/build.gradle.kts`는 그대로다. 릴리스 APK의 dex에 `Structure`, `Native`, `NativeLibrary`, `RustBuffer`, `UniffiLib`가 원래 이름으로 남는 것을 확인했다. 릴리스에서 Kotlin→Rust 호출을 한 번 돌려 보는 일은 M7의 진입 조건이다. 커밋 `5038483`. 둘째, `ci.yml`의 네 작업에 `timeout-minutes`(`rust` 20, `flutter` 15, `windows` 45, `android` 60)를 더하고 Task 7의 YAML 블록에도 같은 네 줄을 넣었다. 커밋 `e35c7be`. 셋째, 미룬 일을 마일스톤별로 모은 [`2026-10-05-m1a-carry-forward.md`](2026-10-05-m1a-carry-forward.md)를 더했다. 커밋 `e864a28`. 넷째, 이 절이다. 이 절을 넣은 커밋(`docs: list where M1a deviated from its plan`)의 SHA는 여기에 적지 않는다.
