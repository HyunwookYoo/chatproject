# M2 iroh Transport Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking. 승인 후 실행된 계획이며 현재 상태는 아래 검증 기록을 따른다.

**Goal:** Windows·Android·iOS 전경에서 개발용 문자열을 주고받고, 연결 경로와 복귀를 실제 기기 및 격리된 자동 시험으로 확인한다.

**Architecture:** 프로세스당 한 코어가 Tokio 런타임을 소유하고, 하나의 관리 루프가 네트워크 상태와 세션 수명을 결정한다. Flutter, FRB, Debug 조종 참가자는 같은 명령과 이벤트를 사용한다. 시험 설정은 Endpoint 생성 전에 주입하며 제품 공개망 설정과 구분한다.

**Tech Stack:** Rust 1.92.0, Flutter 3.44.6, FRB 2.11.1, UniFFI 0.32.2, iroh/iroh-relay 1.3.0, Mainline lookup 0.6.0, iroh-tickets 1.0.0, Tokio, Dart의 HttpServer/WebSocket.

**Spec:** [승인된 M2 설계](../specs/2026-10-12-m2-iroh-transport-design.md). 승인 날짜는 한국 시간 2026-10-13이다. 아래 작업별 PASS는 계획 당시 기대 결과이며 실제 실행 판정은 [현재 검증 결과](../results/2026-10-16-m2-transport.md)를 따른다.

**Status (2026-10-17):** 설계·계획 승인 후 Tasks 1–12 구현·검토 완료, Task 13 코드 지적 수정과 7a 두 야간 성공/최신 일반 CI Android 최종 결과 대기, Task 14 진행 중. 751 ARM64 본문·복귀·이동·해제·재시작은 확인했다. 현재 제품 EE의 연결 목표 미달 RCA·수용 판단/실패한 fresh-client DHT 등 방향별 공개망, 최신 일반 CI의 Android 최종 결과, signed M2 iPhone·최종 전체 브랜치 검토와 통합은 미완료다. [남은 일과 39개 대조표](2026-10-13-m2-carry-forward.md)를 참조한다. M2 완료 선언이 아니다.

## Global Constraints

- Rust 1.92.0, FRB 2.11.1, UniFFI 0.32.2 유지. `libc` 0.2.189 유지. 전체 `cargo update` 금지.
- iroh와 iroh-relay는 `=1.3.0`, lookup은 `=0.6.0`, tickets는 `=1.0.0`. iroh는 `default-features = false`, `features = ["tls-ring"]`. `fast-apple-datapath`와 raw mDNS 사용 금지.
- 공개망은 Minimal + Default relay + Mainline DHT, 게시 필터는 relay_only. N0 preset, dns.iroh.link, 별도 n0 pkarr HTTP 게시 금지. 자동 시험에는 DHT·공용 릴레이·공용 DNS 없음.
- `core_start`에는 네트워크 부작용 없음. 같은 폴더 시작의 idempotence, 다른 폴더 거부, FRB/UniFFI 공유 코어 유지. 키는 프로세스 동안만 유지한다.
- ALPN `chatproject/m2/1`, 스트림 magic `CPM2`, 본문 1–4096 UTF-8 바이트, u32 big-endian 길이. 상대 8개, 명령 64개, 상대별 쓰기 16개, UI 최근 200개.
- 연결·스트림 준비 10초, 쓰기 5초. 재연결 간격 1·2·4·8초(이후 8초), 전체 30초. 자동 메시지 재전송·ACK·DB·MLS·메일박스·실제 채팅 푸시 없음.
- `direct_peers`는 현재 선택된 Direct 경로의 고유 상대 수. 연결 중/릴레이는 세지 않는다. 상태 revision은 코어 인스턴스 내 단조 증가, 메시지 이벤트는 재생하지 않는다.
- Android JNI Context는 실제 applicationContext GlobalRef. JNI_OnLoad reserved는 사용하지 않는다. 초기화는 Flutter/Endpoint 전에 한다. 시험 채널과 조종 인텐트는 Debug만.
- iOS 전경·TestFlight 포함. Apple 알림 필터링·multicast 신청은 보류. NSBonjourServices는 추가하지 않는다. Release JNA/UniFFI 푸시 진입점 검증은 M7 조건으로 남긴다.
- 자동 시험 제어 127.0.0.1:47000, HTTP relay 127.0.0.1:3340. 이미 사용 중인 포트의 프로세스를 종료하지 않는다. 소유한 프로세스·AVD·adb reverse만 정리한다.
- CI `contents: read`, 외부 액션/재사용 workflow는 SHA 고정, checkout persist-credentials false. schedule은 e2e.yml만. 야간 시험에 TestFlight/Firebase 비밀값 없음.
- 실제 Windows 가정 회선↔Android KT LTE 10회, 첫 연결 목표 0.5초/직접 전환 목표 1초. 미달은 원인과 사용자 판단을 기록한다. ARMv7 빌드는 유지, 실제 실행은 호환 장비 확보 후 배포 전으로 이관.
- 생성 코드 수동 수정 금지. 관련 없는 형식 변경 금지. 로그·기기 시리얼·티켓·실제 IP·개인 경로는 `build/e2e/` 또는 gitignored 작업 폴더에만 둔다.

## Review Focus

1. 상태 구독 직후 늦게 도착한 snapshot이 새 상태를 덮어쓰지 않아야 한다 — Task 6 revision 경합 시험.
2. 같은 상대의 동시 연결·이전 연결의 늦은 종료는 새 세션을 지우지 않아야 한다 — Task 3 generation/선택 시험.
3. 연결 준비 중 stop, 마지막 재시도 중 30초 만료는 모든 자식 작업을 끝내야 한다 — Task 3 취소 시험.
4. 사용자 AVD가 이미 켜져 있거나 첫 번째 정리 동작이 실패해도 소유하지 않은 리소스와 최초 실패 코드는 보존돼야 한다 — Task 10 정리 시험.
5. Release R8·JNI 로딩·네트워크 권한은 Debug 성공으로 대체할 수 없다 — Task 5 정적 조건 + Task 13 Release 실행 스모크 + Task 14 실제 폰 네트워크.

## 시작 기준과 파일 책임

구현 시작 전 M1b의 현재 수정 문서 5개와 무시된 측정 기록을 보존하고, M1b 검증·리뷰·통합을 끝낸 기준 커밋을 확인한다. 현재 `m1b-ios`의 dirty 상태에서 M2 소스를 섞지 않는다. Orca-managed 별도 작업 트리에서 `m2-iroh-transport` 브랜치를 만든다. 기존 Claude 세션과 transcript는 읽기 전용이다. 이 문서 작성은 브랜치 통합이나 배포를 수행하지 않는다.

| 파일 | 책임 |
|---|---|
| `core/chat_core/src/transport/types.rs` | 직렬화 가능한 상태·명령·결과 |
| `transport/frame.rs`, `endpoint.rs`, `session.rs`, `manager.rs` | 각각 wire framing, Endpoint 생성, 한 세션의 I/O, 상태·세대·취소 소유 |
| `runtime.rs`, `event.rs`, `error.rs` | 기존 시작 계약과 transport 연결 |
| `core/chat_core/src/puppet/mod.rs`, `protocol.rs` | Debug 조종 접속·RPC wire |
| `app/rust/src/api/chat.rs`, `convert.rs`, `android_jni.rs` | FRB 계약·완전한 enum 변환·Android 로더 |
| `app/lib/network/network_client.dart`, `network_model.dart`, `network_screen.dart` | UI 의존성 주입·revision과 목록·개발 화면 |
| `app/lib/bootstrap.dart`, `main.dart` | 시험 인자 및 시작 오류 처리 |
| `tools/chat_node/src/main.rs` | headless 참가자·로컬 relay CLI |
| `tools/e2e/lib/src/controller.dart`, `owned_resources.dart`, `scenario.dart` | 조종 서버·소유권·각 조합의 검증 |
| `tools/e2e/bin/run.dart`, 기존 기기 도구 | 빌드·기기 준비·참가자 실행·실패 진단 |
| `tools/ci/`, `.github/workflows/` | 좁은 workflow 정책·생성 재현·Release/야간 확인 |

코드 블록은 각 단계의 고정 계약과 핵심 구현이다. import와 기존 보일러플레이트는 해당 파일의 스타일을 따른다. 아래에서 새로 명명한 내부 함수도 소유 작업의 Interfaces에 시그니처를 고정한다. 작업마다 RED 결과를 확인한 뒤 구현하고 GREEN 결과를 기록한다. 컴파일 실패가 RED라면 새 API 부재 때문인지 확인한다.

### Task 1: 상태·오류·FRB 변환 계약

**Files:** Create `core/chat_core/src/transport/mod.rs`, `core/chat_core/src/transport/types.rs`; Modify `core/chat_core/Cargo.toml`, `core/chat_core/src/lib.rs`, `core/chat_core/src/event.rs`, `core/chat_core/src/error.rs`, `app/rust/src/api/chat.rs`, `app/rust/src/convert.rs`; Test `app/rust/src/convert.rs`; Regenerate `app/rust/src/frb_generated.rs`, `app/lib/src/rust/`, `app/android/app/src/main/kotlin/uniffi/`, `app/ios/NotificationService/Generated/`.

**Interfaces:** Produces `NetworkState::{Stopped,Starting,Online,Failed}`, `PeerPhase::{Connecting,Connected,Reconnecting,Disconnected}`, `PeerPath::{Unknown,Relay,Direct}`. 모두 Clone/Debug/PartialEq/Eq/Serialize/Deserialize. `NetworkInfo {revision:u64,state:NetworkState,endpoint_id:Option<String>,ticket:Option<String>,peers:Vec<PeerInfo>}`, `PeerInfo {peer_id:String,phase:PeerPhase,path:PeerPath,connect_ms:Option<u64>,direct_ms:Option<u64>}`. `CoreEvent`에 `NetworkChanged(NetworkInfo)`, `PeerChanged {revision:u64,peer:PeerInfo}`, `ProbeTextReceived {peer_id:String,text:String}`. 상태 enum과 필드는 FRB mirror에도 동일하다. `ErrorCode`는 기존 2개 + 설계의 10개, 누락 없는 match.

- [ ] 아래 표 테스트를 먼저 넣는다. 모든 ErrorCode 12개와 CoreEvent 5개를 실제 fixture로 열거한다. 기존 ConnectionState의 mailbox_ok/queue_ok도 유지한다.

```rust
#[test]
fn network_snapshot_keeps_revision_and_peer() {
    let peer = chat_core::PeerInfo {
        peer_id: "a".repeat(64), phase: chat_core::PeerPhase::Connected,
        path: chat_core::PeerPath::Relay, connect_ms: Some(12), direct_ms: None,
    };
    let info = chat_core::NetworkInfo {
        revision: 7, state: chat_core::NetworkState::Online,
        endpoint_id: None, ticket: None, peers: vec![peer],
    };
    let ChatEvent::NetworkChanged { info } = ChatEvent::from(
        chat_core::CoreEvent::NetworkChanged(info)) else { panic!("wrong event") };
    assert_eq!(info.revision, 7);
    assert_eq!(info.peers[0].path, PeerPath::Relay);
}
```

- [ ] Run `cargo test -p chat_ffi network_snapshot_keeps_revision_and_peer`; expect missing type/variant RED.
- [ ] 오류 변환의 실제 fixture는 다음 표로 만든다. 코드와 detail을 각각 확인하고 wildcard 변환을 두지 않는다.

```rust
#[test]
fn every_error_code_keeps_its_variant() {
    use chat_core::ErrorCode as C;
    use crate::api::chat::ErrorCode as F;
    let pairs = [
        (C::InvalidConfig, F::InvalidConfig),
        (C::AlreadyStartedDifferentConfig, F::AlreadyStartedDifferentConfig),
        (C::CoreNotStarted, F::CoreNotStarted),
        (C::NetworkNotStarted, F::NetworkNotStarted),
        (C::InvalidPeerAddress, F::InvalidPeerAddress),
        (C::SelfConnection, F::SelfConnection),
        (C::PeerNotConnected, F::PeerNotConnected),
        (C::MessageTooLarge, F::MessageTooLarge),
        (C::Busy, F::Busy), (C::NetworkTimeout, F::NetworkTimeout),
        (C::TransportFailed, F::TransportFailed),
        (C::NotAvailable, F::NotAvailable),
    ];
    for (core, ffi) in pairs { assert_eq!(F::from(core), ffi); }
}
```
- [ ] 타입과 변환을 추가한다. 에러 문자열에는 입력 주소/본문을 붙이지 않는다. serde는 `chat_core`에 `serde = {version = "1", features = ["derive"]}`로 추가한다. Root CoreEvent serde tag는 `kind`, rename_all snake_case; FRB mirror는 기존 생성기 관례를 따른다.

```rust
impl From<chat_core::PeerPath> for PeerPath {
    fn from(path: chat_core::PeerPath) -> Self {
        match path {
            chat_core::PeerPath::Unknown => Self::Unknown,
            chat_core::PeerPath::Relay => Self::Relay,
            chat_core::PeerPath::Direct => Self::Direct,
        }
    }
}
pub(crate) struct FrbSink(pub(crate) StreamSink<ChatEvent>);
impl chat_core::EventSink for FrbSink {
    fn send(&self, event: chat_core::CoreEvent) -> bool {
        self.0.add(event.into()).is_ok()
    }
}
```

- [ ] EventSink 문서에 nonblocking·버스/슬롯 재진입 금지를 명시한다. 버스/슬롯의 poisoned mutex는 내부 불변식 오류로 fail-fast 유지하되 FFI sink의 불필요한 Mutex/expect는 제거한다. bus는 Arc 공유 가능한 Clone을 지원하고 닫힌 sink의 false를 제거한다. `native.rs` 테스트가 전역 코어 시작을 독점한다는 주석을 남긴다.
- [ ] `cd app; flutter pub get` 후 저장소 루트에서 `bash tools/gen_bindings.sh`; `cargo test -p chat_core -p chat_ffi`. 생성 전 codegen 정확히 2.11.1, cargo-expand 1.0.126 설치 여부 확인. Expect 기존+새 변환 시험 PASS. pub get에서 생긴 내용 없는 Windows plugin 파일 diff는 포함하지 않는다.
- [ ] 위 Files와 실제 생성된 파일만 stage하고 commit `feat(core): define M2 network state and event contracts`.

### Task 2: 길이 제한이 있는 전송 프레임

**Files:** Create `core/chat_core/src/transport/frame.rs`; Modify `core/chat_core/Cargo.toml`, `core/chat_core/src/transport/mod.rs`.

**Interfaces:** Consumes CoreError/ErrorCode. Produces `ALPN:&[u8]`, `MAGIC:&[u8;4]`, `validate_text(&str)->Result<(),CoreError>`, `read_text<R:AsyncRead+Unpin>(&mut R)->Result<Option<String>,CoreError>`, `write_text<W:AsyncWrite+Unpin>(&mut W,&str)->Result<(),CoreError>`. None은 프레임 경계의 정상 EOF에만 반환한다. `protocol_error()->CoreError`는 TransportFailed와 고정 설명을 반환한다.

- [ ] tokio `1` features `rt-multi-thread,sync,time,net,io-util,macros`와 dev tokio `test-util`을 추가하고 다음 시험을 먼저 쓴다.

```rust
#[tokio::test]
async fn rejects_length_before_reading_body() {
    let mut input = &4097_u32.to_be_bytes()[..];
    assert_eq!(read_text(&mut input).await.unwrap_err().code,
               ErrorCode::MessageTooLarge);
}
#[test]
fn checks_bytes_not_characters() {
    assert!(validate_text(&"가".repeat(1365)).is_ok());
    assert_eq!(validate_text(&"가".repeat(1366)).unwrap_err().code,
               ErrorCode::MessageTooLarge);
    assert!(validate_text("").is_err());
}
```

- [ ] `cargo test -p chat_core transport::frame`; expect missing codec RED.
- [ ] 다음 길이 검사를 읽기·쓰기의 공통 경계로 사용한다. `read_text`는 header 첫 바이트를 `read`로 읽어 0이면 None, 나머지 3바이트 read_exact, 길이 검사 후 최대4096 allocation, 본문 read_exact, String::from_utf8 순서다. 중간 EOF와 잘못된 UTF-8은 protocol_error. 쓰기는 header→본문 순서이고 flush는 메시지 수신 확인으로 취급하지 않는다.

```rust
pub const ALPN: &[u8] = b"chatproject/m2/1";
pub const MAGIC: &[u8; 4] = b"CPM2";
pub fn validate_text(text: &str) -> Result<(), CoreError> {
    if !(1..=4096).contains(&text.len()) {
        return Err(CoreError::new(ErrorCode::MessageTooLarge,
                                  "probe text must contain 1..4096 UTF-8 bytes"));
    }
    Ok(())
}
pub async fn write_text<W: tokio::io::AsyncWrite + Unpin>(
    out: &mut W, text: &str,
) -> Result<(), CoreError> {
    use tokio::io::AsyncWriteExt;
    validate_text(text)?;
    out.write_all(&(text.len() as u32).to_be_bytes()).await
        .map_err(|_| protocol_error())?;
    out.write_all(text.as_bytes()).await.map_err(|_| protocol_error())
}
```

- [ ] 추가 시험: 1/4096 성공, 0/4097 거부, 한 header의 각 중간 EOF, body 중간 EOF, invalid UTF-8 `[0xff]`, 연속2프레임 순서, 정상끝 None, duplex의 여러 조각 읽기. `cargo test -p chat_core transport::frame` PASS.
- [ ] commit `feat(core): add bounded M2 text framing`.

### Task 3: Endpoint와 취소 가능한 연결 관리 루프

**Files:** Create `core/chat_core/src/transport/endpoint.rs`, `core/chat_core/src/transport/session.rs`, `core/chat_core/src/transport/manager.rs`, `core/chat_core/tests/transport.rs`; Modify `core/chat_core/Cargo.toml`, `core/chat_core/src/transport/mod.rs`, `core/chat_core/src/transport/types.rs`, `Cargo.lock`.

**Interfaces:** Consumes Task1 types/events and Task2 codec. Produces `NetworkOp::{Start,Info,Connect{target:String},Send{peer_id:String,text:String},Disconnect{peer_id:String},Stop}`, `NetworkReply::{Info(NetworkInfo),Peer(PeerInfo),Done}`, `NetworkHandle::spawn(handle:&tokio::runtime::Handle,profile:NetworkProfile,bus:EventBus)->Self`, `execute(&self,op:NetworkOp)->impl Future<Output=Result<NetworkReply,CoreError>>`. `NetworkProfile::Public`; `Local {relay:Option<iroh::RelayUrl>}` is cfg(any(test,all(feature="test-hooks",debug_assertions))). Internal `bind_endpoint(profile:&NetworkProfile,key:iroh::SecretKey)->Result<iroh::Endpoint,CoreError>`, `parse_target(&str)->Result<iroh::EndpointAddr,CoreError>`, `prepare_session(Connection)->Result<(iroh::endpoint::SendStream,iroh::endpoint::RecvStream),CoreError>`. `SessionTag {epoch:u64,generation:u64,peer_id:String}` accompanies every completion. Internal commands carry oneshot replies; notes are bounded mpsc64; peer writer mpsc16. Tasks live in JoinSet, share CancellationToken children. Add `tokio-util` feature rt and `futures-util` for path streams.

- [ ] Add pinned dependencies, then write local-only direct pair using actual public manager API. Task fixture starts Tokio-managed handles with Local relay None and cloned EventBus; await NetworkChanged Online with a 10s test deadline, obtain Info, parse ticket, then Connect. Events are collected by an EventSink with try_send into bounded channel256; sink returns false on overflow, which fails the test.

```rust
#[test]
fn earlier_session_cannot_remove_replacement() {
    let old = SessionTag { epoch: 1, generation: 4, peer_id: "p".into() };
    let current = SessionTag { epoch: 1, generation: 5, peer_id: "p".into() };
    assert!(!old.is_current(&current));
    assert!(current.is_current(&current));
}
impl SessionTag {
    fn is_current(&self, current: &Self) -> bool {
        self.epoch == current.epoch && self.generation == current.generation
            && self.peer_id == current.peer_id
    }
}
```

- [ ] 이 작업에서 `chat_core`에 `test-hooks=[]` feature를 먼저 선언한다. integration test는 library cfg(test)를 켜지 않으므로 transport test target의 required-features를 test-hooks로 지정한다. `cargo test -p chat_core --features test-hooks --test transport`; expect missing manager RED. manager.rs unit tests use paused Tokio time; direct pair test uses real sockets.
- [ ] Public Endpoint uses this explicit policy. Local uses Minimal, `RelayMode::Disabled` for None or custom one-server RelayMap for Some, no lookup builder. Direct-only local ticket includes loopback bound sockets (rewrite unspecified bind IPs to loopback for tests). Online is Endpoint ready; local relay mode awaits relay readiness with a bounded startup deadline30s. Inspect actual pinned builder API, compile before continuing.

```toml
iroh = { version = "=1.3.0", default-features = false, features = ["tls-ring"] }
iroh-tickets = "=1.0.0"
iroh-mainline-address-lookup = "=0.6.0"
```

```rust
let builder = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
    .secret_key(key).alpns(vec![ALPN.to_vec()]);
let builder = builder.relay_mode(iroh::RelayMode::Default).address_lookup(
    iroh_mainline_address_lookup::DhtAddressLookup::builder()
        .addr_filter(iroh::address_lookup::AddrFilter::relay_only()));
let endpoint = builder.bind().await.map_err(|_| CoreError::new(
    ErrorCode::TransportFailed, "endpoint bind failed"))?;
```

- [ ] 핵심 송수신 시험은 실제 관리 루프를 거친다. 테스트 전용 `online` helper는 다음 정의를 사용한다. EventSink와 수신 단언을 합쳐 양방향으로 수행한다.

```rust
async fn online(net: &NetworkHandle) -> NetworkInfo {
    net.execute(NetworkOp::Start).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let NetworkReply::Info(info) = net.execute(NetworkOp::Info).await.unwrap() {
                if info.state == NetworkState::Online { return info; }
                assert_ne!(info.state, NetworkState::Failed);
            }
            tokio::task::yield_now().await;
        }
    }).await.unwrap()
}
#[tokio::test]
async fn local_pair_connects_without_discovery() {
    let a = NetworkHandle::spawn(&tokio::runtime::Handle::current(),
        NetworkProfile::Local { relay: None }, EventBus::new());
    let b = NetworkHandle::spawn(&tokio::runtime::Handle::current(),
        NetworkProfile::Local { relay: None }, EventBus::new());
    let ai = online(&a).await;
    let bi = online(&b).await;
    let reply = a.execute(NetworkOp::Connect { target: bi.ticket.unwrap() }).await.unwrap();
    let NetworkReply::Peer(peer) = reply else { panic!("not a peer") };
    assert_eq!(peer.phase, PeerPhase::Connected);
    assert_ne!(ai.endpoint_id, Some(peer.peer_id));
    a.execute(NetworkOp::Stop).await.unwrap();
    b.execute(NetworkOp::Stop).await.unwrap();
}
```

- [ ] `NetworkHandle::execute` uses try_send and oneshot; Full→Busy, Closed→TransportFailed. Start creates a bind task and returns Starting immediately; repeated Start returns existing Info. Info clones actor state. While binding, Connect/Send return NetworkNotStarted. Invalid ticket/ID→InvalidPeerAddress; own ID→SelfConnection; 9th distinct active peer→Busy. Duplicate pending dial→Busy. Dial/stream preparation is a spawned 10s task, never awaited inside manager command handler.

```rust
struct Envelope {
    op: NetworkOp,
    reply: tokio::sync::oneshot::Sender<Result<NetworkReply, CoreError>>,
}
#[derive(Clone)]
pub struct NetworkHandle { commands: tokio::sync::mpsc::Sender<Envelope> }
impl NetworkHandle {
    pub async fn execute(&self, op: NetworkOp) -> Result<NetworkReply, CoreError> {
        let (reply, received) = tokio::sync::oneshot::channel();
        self.commands.try_send(Envelope { op, reply }).map_err(|error| {
            let code = match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => ErrorCode::Busy,
                tokio::sync::mpsc::error::TrySendError::Closed(_) => ErrorCode::TransportFailed,
            };
            CoreError::new(code, "network command unavailable")
        })?;
        received.await.map_err(|_| CoreError::new(
            ErrorCode::TransportFailed, "network manager stopped"))?
    }
}
```

Stop에 의해 취소된 진행 중 명령은 NetworkNotStarted, 특정 peer 해제로 취소된 쓰기는 PeerNotConnected, 정해진 deadline 초과는 NetworkTimeout이다. pending oneshot을 그냥 drop해 상세 원인이 TransportFailed로 바뀌지 않도록 관리자가 종료 전에 명시적으로 reply한다.
- [ ] Session open_uni writes CPM2 before accept_uni; peer magic read has same10s deadline. One reader, one writer, one extra-stream guard and path observer are tracked children. Invalid magic/additional stream closes Connection. Writer uses timeout5s and resolves each reply once; disconnect/stop drops pending replies with a defined CoreError. Session notes carry tag; stale notes are ignored. `paths().iter().find(|p|p.is_selected())` decides Direct/Relay/Unknown; every path notification including lag repolls current paths.
- [ ] Actor chooses duplicates using lower EndpointId as preferred initiator. Keep a lone valid connection even if initiated by larger key; replace only when a competing preferred connection is ready. Generation increments on replacement, network epoch increments on endpoint stop/rebind. Stop cancels binding/dials/retries/I/O, closes Endpoint, joins children and replies after quiescence. 이미 Stopped인 Stop도 현재 Offline ConnectionState를 발행해 닫힌 구독 정리의 두 번째 발행을 만들지만 새 Endpoint를 열지 않는다. Keep SecretKey separately from Endpoint. Retry is cancellable and bounded by the original deadline:

```rust
pub(crate) fn retry_delay(attempt: u32) -> std::time::Duration {
    std::time::Duration::from_secs([1, 2, 4, 8][attempt.min(3) as usize])
}
pub(crate) async fn within_retry_budget<F: std::future::Future>(
    deadline: tokio::time::Instant, cancel: &tokio_util::sync::CancellationToken,
    work: F,
) -> Option<F::Output> {
    tokio::select! {
        _ = cancel.cancelled() => None,
        _ = tokio::time::sleep_until(deadline) => None,
        result = work => Some(result),
    }
}
```

- [ ] Every state change bumps revision once, emits NetworkChanged full snapshot and relevant PeerChanged with same revision, then ConnectionState unique direct count (mailbox false, queue None). Every valid received frame emits ProbeTextReceived once; no replay. Never hold state mutex while polling QUIC or publishing a callback.
- [ ] Connect 명령 수락 때 std::time::Instant를 잡고 session-ready note의 connect_ms와 최초 selected Direct note의 direct_ms를 같은 시작점으로 계산한다. 재시도는 별도 시도의 측정이며 기존 실패 표본을 성공으로 덮지 않는다. 수신 연결은 두 값 None을 허용한다. 화면에는 선택 상대의 ms를 표시해 Task14에 그대로 기록한다.
- [ ] Tests pin: two-way Korean text; ticket/ID parser; invalid/self address; wrong ALPN and magic; extra stream; 8/64/16 caps; simultaneous connects converge to one peer; old tag ignored; relay not counted; path repoll; stop during bind/read/write/dial; retries1/2/4/8 and in-flight cancellation exactly at30s; disconnected send refused and not replayed. `cargo test -p chat_core`, `cargo tree -p chat_core -e features` PASS and single iroh1.3, no unwanted features. Commit `feat(core): manage iroh sessions and reconnect lifetimes`.

### Task 4: 공유 코어의 런타임과 비동기 FRB API

**Files:** Modify `core/chat_core/src/runtime.rs`, `core/chat_core/src/lib.rs`, `app/rust/src/api/chat.rs`, `app/rust/src/global.rs`, `app/rust/src/native.rs`; Test runtime.rs and Create `app/integration_test/network_test.dart`; Regenerate bindings at Task1 paths.

**Interfaces:** `CoreSlot::network(&self)->Result<NetworkHandle,CoreError>` clones the handle after releasing slot mutex. `RunningCore {info:CoreInfo,network:NetworkHandle,runtime:Option<Runtime>}` owned by Arc. FRB async signatures: `network_start()->Result<NetworkInfo,CoreError>`, `network_info()->Result<NetworkInfo,CoreError>`, `connect_peer(target:String)->Result<PeerInfo,CoreError>`, `send_probe_text(peer_id:String,text:String)->Result<(),CoreError>`, `disconnect_peer(peer_id:String)->Result<(),CoreError>`, `network_stop()->Result<(),CoreError>`. Helpers `run_network(NetworkOp)->Result<NetworkReply,CoreError>` and exhaustive reply conversion; no new core_start arguments.

- [ ] Extend existing same-folder tests: clone handle, obtain Stopped Info, no endpoint_id/ticket, call Start/Stop local test profile, verify unchanged core.instance_id. A fresh slot.network() returns CoreNotStarted. Add an integration test canceling a Dart event subscription and doing network_info/network_stop twice while Stopped; a fresh subscription receives current ConnectionState. 송수신 후 메시지 재생 없음은 Task7의 격리 프로필이 준비된 뒤 같은 시험 파일에 추가한다. 이 단계의 앱 통합 시험에서 공용 Endpoint를 시작하지 않는다.
- [ ] Run `cargo test -p chat_core runtime`; missing network API RED. Preserve the existing first-start one Offline publication and repeated-start no re-publication assertions.
- [ ] CoreSlot starts runtime builder enable_all, NetworkHandle Public on its runtime, stores Arc RunningCore. Runtime construction error→TransportFailed, core remains unstarted. Clone EventBus state, not a new bus. Drop runtime via shutdown_background to avoid blocking Tokio context.

```rust
impl Drop for RunningCore {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}
async fn run_network(op: chat_core::NetworkOp) -> Result<chat_core::NetworkReply, CoreError> {
    let network = CORE.network().map_err(CoreError::from)?;
    network.execute(op).await.map_err(CoreError::from)
}
pub async fn network_info() -> Result<NetworkInfo, CoreError> {
    match run_network(chat_core::NetworkOp::Info).await? {
        chat_core::NetworkReply::Info(info) => Ok(info.into()),
        _ => Err(CoreError { code: ErrorCode::TransportFailed,
                             detail: "unexpected network reply".into() }),
    }
}
```

- [ ] Regenerate through bash script. `cargo test -p chat_core -p chat_ffi`, `flutter test integration_test -d windows` from app. Expect all core/closed-stream tests PASS. Generated bindings expose async commands; Kotlin existing native_instance_id still same global core. Commit `feat(ffi): expose shared asynchronous network commands`.

### Task 5: Android JVM/Context와 플랫폼 접속 조건

**Files:** Create `app/rust/src/android_jni.rs`; Modify `app/rust/src/lib.rs`, `app/rust/Cargo.toml`, `app/android/app/src/main/kotlin/dev/chatproject/chat_app/MainActivity.kt`, `app/android/app/src/main/AndroidManifest.xml`, `app/android/app/proguard-rules.pro`, `app/ios/Runner/Info.plist`; Create `tools/ci/check_network_platform.py`, `tools/ci/test_check_network_platform.py`.

**Interfaces:** Android JNI_OnLoad stores JavaVM process pointer and returns JNI_VERSION_1_6; exported `Java_dev_chatproject_chat_1app_MainActivity_initializeNetworkContext(JNIEnv,JObject,JObject)->jboolean`. `ContextState {vm:JavaVM,context:GlobalRef}` process-lifetime OnceLock under init Mutex; concurrent initialization is one success and no duplicate ndk initialization. Only Android compiles module. JNI dependency `=0.21.1` target android; `ndk-context` matching iroh-dns resolver and iroh dependency are target android. Actual DNS init uses `iroh::dns::install_android_jni_context` before endpoint creation.

- [ ] Python fixtures assert missing main INTERNET, load after super.onCreate, Debug channel unguarded, JNI reserved-as-context and absent native R8 keep rule fail. Fixture also asserts iOS usage string and absence of added BonjourServices. Run `python -m unittest discover -s tools/ci -p test_check_network_platform.py`; missing checker RED.
- [ ] MainActivity imports Bundle and Context, loads/init before super; external member is a normal instance method, so Rust JNI `_this:JObject`, not JClass. Debug native channel and debug getDartEntrypointArgs use FLAG_DEBUGGABLE.

```kotlin
private external fun initializeNetworkContext(context: Context): Boolean
override fun onCreate(savedInstanceState: Bundle?) {
    System.loadLibrary("chat_ffi")
    check(initializeNetworkContext(applicationContext)) { "Network context initialization failed" }
    super.onCreate(savedInstanceState)
}
```

```proguard
-keepclasseswithmembernames class dev.chatproject.chat_app.MainActivity {
    native <methods>;
}
```

- [ ] JNI_OnLoad never dereferences reserved. Explicit JNI function verifies VM was loaded, checks non-null context, creates GlobalRef before handing pointers to resolver, retains GlobalRef after successful init. Mutex serializes initialization; subsequent call returns JNI_TRUE. On JNI/lock failure returns JNI_FALSE and reports a fixed message, no Rust panic across extern boundary. Implement checker using parsed XML for permissions/plist and bounded source assertions for loader rules; checker complements actual execution.

```rust
static JVM_PTR: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
static CONTEXT: std::sync::OnceLock<ContextState> = std::sync::OnceLock::new();
static INIT: std::sync::Mutex<()> = std::sync::Mutex::new(());
#[no_mangle]
pub extern "system" fn JNI_OnLoad(vm: *mut jni::sys::JavaVM,
    _reserved: *mut std::ffi::c_void) -> jni::sys::jint {
    if vm.is_null() { return jni::sys::JNI_ERR; }
    match JVM_PTR.get() {
        Some(saved) if *saved != vm as usize => jni::sys::JNI_ERR,
        Some(_) => jni::sys::JNI_VERSION_1_6,
        None => {
            if JVM_PTR.set(vm as usize).is_err() { return jni::sys::JNI_ERR; }
            jni::sys::JNI_VERSION_1_6
        }
    }
}
```

JNI member가 init lock을 얻은 뒤 수행할 고정 순서는 `JavaVM::from_raw(ptr)` → `env.new_global_ref(context)` → `unsafe { install_android_jni_context(vm.get_java_vm_pointer().cast(), global.as_obj().as_raw().cast()) }` → `CONTEXT.set(ContextState {vm,context:global})`다. set은 이 lock의 단일 작성자 조건상 성공해야 하며 실패가 가능한 경로를 추가하지 않는다. 함수는 JNI_TRUE/False로 결과를 변환한다. ndk-context 초기화를 별도로 두 번 호출하지 않는다.
- [ ] Add Korean iOS LAN usage explanation: `개발용 연결 시험에서 같은 네트워크의 기기에 접속합니다.` No additional entitlement. Run checker tests, actual `flutter build apk --release --target-platform android-arm64,android-arm`, existing Debug native bridge integration; macOS unsigned iOS release via existing job. Release JNI/DNS runtime evidence is recorded Task13/14, not claimed from static test. Commit `fix(android): initialize JVM context before networking`.

### Task 6: 시작 흐름과 개발용 연결 화면

**Files:** Create `app/lib/bootstrap.dart`, `app/lib/network/network_client.dart`, `app/lib/network/network_model.dart`, `app/lib/network/network_screen.dart`, `app/test/network_model_test.dart`, `app/test/network_screen_test.dart`, `app/test/bootstrap_test.dart`, `app/test/support/fake_network_client.dart`; Modify `app/lib/core_client.dart`, `app/lib/main.dart`, `app/lib/home/home_screen.dart`.

**Interfaces:** `NetworkClient`의 `Future<rust.NetworkInfo> startNetwork()`, `Future<rust.NetworkInfo> networkInfo()`, `Future<rust.PeerInfo> connectPeer(String target)`, `Future<void> sendProbeText(String peerId,String text)`, `Future<void> disconnectPeer(String peerId)`, `Future<void> stopNetwork()`, `Stream<rust.ChatEvent> events()`. FrbCoreClient implements CoreClient and NetworkClient, optional dataDir constructor. `NetworkModel(NetworkClient)` owns subscription, `Future<void> open()`, `void applySnapshot(rust.NetworkInfo)`, `void appendLine(String)`, `Future<void> close()`, `rust.NetworkInfo? snapshot`, `List<String> recentLines`, `String? error`; ChangeNotifier. `Future<Widget> bootstrap(List<String>, {required Future<void> Function() initializeRust, required CoreClient Function(String?) createClient})` permits startup failure injection. Debug hooks startup is attached Task7.

- [ ] Fake client uses broadcast StreamController; its networkInfo Future can be delayed. Test new state arrives before older snapshot; body history never exceeds200; failed send never resends on state recovery; onError/onDone visible; closing model cancels subscription.

```dart
test('old snapshot cannot replace newer state', () {
  final model = NetworkModel(FakeNetworkClient());
  model.applySnapshot(const rust.NetworkInfo(
    revision: 9, state: rust.NetworkState.online,
    endpointId: 'new', ticket: null, peers: []));
  model.applySnapshot(const rust.NetworkInfo(
    revision: 8, state: rust.NetworkState.starting,
    endpointId: null, ticket: null, peers: []));
  expect(model.snapshot!.endpointId, 'new');
});
```

- [ ] Run `flutter test test/network_model_test.dart test/bootstrap_test.dart`; missing model RED.
- [ ] Model subscribes first, then reads snapshot; state events update by revision. Equal revision accepted for consistent PeerChanged/full snapshot; full snapshots authoritative. CoreEvent_Error, command errors and stream errors are visible, commands leave Busy disabled buttons until finally clears. UTF-8 byte validation is final Rust authority. 송신 성공 문구는 `쓰기 완료`, 설명은 `상대 화면 표시나 저장을 확인한 상태는 아니에요`로 한다.

```dart
void applySnapshot(rust.NetworkInfo next) {
  if (snapshot != null && next.revision < snapshot!.revision) return;
  snapshot = next;
  notifyListeners();
}
void appendLine(String line) {
  recentLines.add(line);
  if (recentLines.length > 200) recentLines.removeAt(0);
  notifyListeners();
}
```

- [ ] Home injects optional NetworkClient and navigates to page; page copies ID/ticket with Clipboard, connects/disconnects selected peer, renders paths and bounded list. LayoutBuilder at720px gives side-by-side panels otherwise column scrolling. Existing theme/tokens retained; no new visual style or full contacts mockup. Permission denied/network failures show retry action. DataDir override for Windows accepts absolute `--data-dir`; Debug-only hooks/control args rejected in Release. EndpointId changes after process restart is visible copy.
- [ ] Bootstrap catches `(error,stack)`, debugPrintStack with fixed startup context, renders StartupErrorApp. Tests inject Rust init failure/client.start failure. Widgets cover all12 error codes, 360px and1280px no overflow, disabled disconnected send, stream failure, idle Home existing labels. `flutter test`, `flutter analyze`, Windows integration folder PASS. Commit `feat(app): add foreground network probe screen`.

### Task 7: Debug 조종 wire와 격리 초기화

**Files:** Create `core/chat_core/src/puppet/protocol.rs`, `core/chat_core/src/puppet/mod.rs`, `app/rust/cargokit.yaml`, `core/chat_core/tests/puppet.rs`; Modify `core/chat_core/Cargo.toml`, `app/rust/Cargo.toml`, `core/chat_core/src/runtime.rs`, `core/chat_core/src/lib.rs`, `core/chat_core/src/transport/manager.rs`, `app/rust/src/api/chat.rs`, `app/lib/bootstrap.dart`, `app/android/app/src/main/kotlin/dev/chatproject/chat_app/MainActivity.kt`, `app/integration_test/network_test.dart`; Regenerate bindings at Task1 paths.

**Interfaces:** feature `chat_core/test-hooks`, `chat_ffi/test-hooks=["chat_core/test-hooks"]`; disabled by default. `async CoreSlot::test_hooks_start(&self,control_url:String,name:String)->Result<(),CoreError>` and always-generated async FRB function same signature. Disabled feature or non-debug_assertions returns NotAvailable. Enabled: connect WS, receive configuration, configure Local profile while Stopped, then return ready after the same core NetworkOps can be used. `async NetworkHandle::configure_local(relay:Option<RelayUrl>)->Result<(),CoreError>`, `async set_offline(bool)->Result<NetworkInfo,CoreError>` cfg hooks; false rebinds retained key. 이 메서드들은 cfg된 `NetworkOp::ConfigureLocal{relay:Option<RelayUrl>}`/`SetOffline{offline:bool}` 명령을 같은 관리자에게 전달하고 Done/Info 응답을 받는다. Wire JSON records defined below; `event.seq` starts1 per core instance and survives control reconnect, increments only enqueued transmitted events. Async RPC concurrency cap64, each request ID unique.

```json
{"type":"hello","protocol":"chat-test-hooks:v1","name":"win1","instance":"process-instance","relay":"http://127.0.0.1:3340"}
{"type":"cmd","id":1,"op":"send_probe_text","args":{"peer_id":"peer","text":"hi"}}
{"type":"result","id":1,"ok":true,"value":null}
{"type":"result","id":2,"ok":false,"error":{"code":"PeerNotConnected","detail":"peer is disconnected"}}
{"type":"event","seq":1,"instance":"process-instance","event":{"kind":"probe_text_received","peer_id":"peer","text":"hi"}}
```

- [ ] Protocol tests reject unknown JSON shape/id duplication, accept named commands and return typed results. Test release/off build NotAvailable. Local profile test asserts before Start state has no endpoint; lookup/relay public policy cannot be reached. Hook loop event sink mpsc256 try_send overflow→cancel/close WS and visible TransportFailed; callback never blocks/reenters bus.
- [ ] Run `cargo test -p chat_core --features test-hooks --test puppet`; absent hook RED. Add tokio-tungstenite with connect and JSON serde_json (no TLS needed for WS loopback); accept control URL only ws loopback and fixed47000. Hello startup waits server configuration ack carrying relay URL, checks only http loopback3340, configures stopped actor, then marks ready. Auto net start after ready can call core Start. Unknown op→NotAvailable; malformed typed args→InvalidConfig.

```yaml
cargo:
  debug:
    extra_flags:
      - --features
      - test-hooks
```

```rust
#[cfg(not(all(feature = "test-hooks", debug_assertions)))]
pub async fn test_hooks_start(&self, _control_url: String, _name: String)
    -> Result<(), CoreError> {
    Err(CoreError::new(ErrorCode::NotAvailable, "test hooks are disabled"))
}
```

- [ ] Reachable hook module and marker use `cfg(all(feature="test-hooks",debug_assertions))`. No non-hook reference to marker literal. Bootstrap debug args `--control-url`, `--name` await hooks ready after core_start but before opening network screen. Android Debug intent extras `chat_control_url`, `chat_name` become Dart args; Release ignores extras. UI network_start does not overwrite Local profile. 첫 profile 설정은 Stopped에서만 가능하고 이미 같은 Local profile인 control 재접속은 idempotent하게 확인만 한다. 공개망 Online 인스턴스를 시험 profile로 교체하지 않는다. Loop uses FuturesUnordered to dispatch pending commands while events continue; handle.disconnect fails pending requests, no silent orphan.
- [ ] Tests: receive events while RPC connect pending; sameinstance reconnect eventseq continuing; newinstance seq1; set_offline rejects sends, key retained after false, no replay; control down doesn't stop normal core unless explicitly stopped. Run Rust hooks tests, regenerate, Flutter bootstrap/widget tests PASS. Commit `feat(test): add isolated debug network puppet`.

### Task 8: headless 참가자와 로컬 relay CLI

**Files:** Create `tools/chat_node/Cargo.toml`, `tools/chat_node/src/main.rs`, `tools/chat_node/tests/cli.rs`; Modify `Cargo.toml`, `Cargo.lock`.

**Interfaces:** binary `chat_node relay --bind 127.0.0.1:3340`; `chat_node puppet --control-url ws://127.0.0.1:47000 --name node1 --data-dir <absolute>`. Node exports no product API. Cargo feature `harness` gates bin and enables chat_core/test-hooks; default empty. `iroh-relay = =1.3.0` server feature is only node dependency, iroh default disabled. Relay prints one readiness JSON after successful spawn, then SIGINT/shutdown. Puppet starts CoreSlot before hook, no public Endpoint.

- [ ] CLI integration spawns relay on an already bound port and asserts nonzero, no readiness; missing name/relative data_dir nonzero; help exit0. Run `cargo test -p chat_node --features harness`; missing crate RED.
- [ ] Register workspace member; use clap derive and Tokio. Rust server config is non_exhaustive: instantiate Default then assign relay; HTTP only, no QUIC listener/TLS, 선택적 metrics listener는 비활성화한다. Add readiness output; avoid prints of private addresses beyond local settings.

```toml
[features]
default = []
harness = ["chat_core/test-hooks"]

[[bin]]
name = "chat_node"
path = "src/main.rs"
required-features = ["harness"]
```

```rust
let mut config = iroh_relay::server::ServerConfig::default();
config.relay = Some(iroh_relay::server::RelayConfig::new(bind));
let server = iroh_relay::server::Server::spawn(config).await?;
println!("{}", serde_json::json!({"type":"ready","role":"relay"}));
tokio::signal::ctrl_c().await?;
server.shutdown().await?;
```

- [ ] Add tokio signal feature. Tests start real relay local ephemeral test port, connect two hooked headless participants through controller fixture, exchange text and cleanly stop. Product build command remains `cargo build -p chat_ffi --release` without harness; never workspace --all-features Release. `cargo test -p chat_node --features harness`, `cargo tree -p chat_ffi -e features` PASS. Commit `feat(test): add headless core and loopback relay runner`.

### Task 9: 루프백 조종 서버와 RPC/이벤트 진단

**Files:** Create `tools/e2e/lib/src/controller.dart`, `tools/e2e/lib/src/protocol.dart`, `tools/e2e/test/controller_test.dart`.

**Interfaces:** `Controller.start({int port=47000,required Directory logDir})->Future<Controller>`, `waitParticipant(String,{Duration timeout=10s})->Future<Participant>`, `close()->Future<void>`. `Participant {String name,String instance}`, `call(String op,Map<String,Object?> args,{Duration timeout=15s})->Future<Object?>`, `expectEvent(bool Function(Map<String,Object?>),{Duration timeout=15s})->Future<Map<String,Object?>>`. Buffer256 events, pending64 commands, increasing id, per-instance eventseq. Every sent/received wire saved JSONL ignored. Hello→config ack `{"type":"config","relay":"http://127.0.0.1:3340"}`. Config can be null relay for direct-only unit fixtures.

- [ ] Dart test uses actual HttpServer/WebSocket on loopback ephemeral port0 and mock participant; asserts unmatched id fails, disconnect fails outstanding calls, timeout removes pending, duplicate/out-of-order/gap seq fails entire scenario, same name/new instance resets seq, late previous socket cannot detach replacement, occupied port does not kill holder.

```dart
test('sequence gaps fail before accepting text', () {
  final guard = SequenceGuard();
  guard.accept('one', 1);
  expect(() => guard.accept('one', 3), throwsStateError);
});
class SequenceGuard {
  String? instance;
  int last = 0;
  void accept(String nextInstance, int seq) {
    if (instance != nextInstance) { instance = nextInstance; last = 0; }
    if (seq != last + 1) throw StateError('event sequence gap or duplicate');
    last = seq;
  }
}
```

- [ ] `dart test test/controller_test.dart` from tools/e2e; absent controller RED.
- [ ] Server binds InternetAddress.loopbackIPv4 only; WebSocketTransformer.upgrade rejects other paths than `/`. Require hello protocol/version and nonempty bounded name/instance; register socket after ack. Exact JSON records Task7. SequenceGuard는 참가자 socket마다 새로 만들지 않고 core instance별로 보존해 같은 instance 재접속의 순번을 계속 검사한다. 이전 socket의 record는 현재 socket identity를 확인해 버리고 새 참가자를 detach하지 못하게 한다. Pending Completer map entry removed in finally; stream onDone/onError completes all once and records failure. expectEvent searches remaining buffer and removes matched event, so an old receive cannot satisfy another assertion. Overflow fails, no silent text drops. Close is idempotent and closes owned sockets/server/logs.
- [ ] Complete actual-socket tests, `dart analyze`, `dart test` PASS. Commit `feat(e2e): coordinate puppet commands and sequenced events`.

### Task 10: 외부 프로세스·AVD 소유권과 기기 CLI

**Files:** Modify `tools/e2e/lib/src/process_runner.dart`, `tools/e2e/lib/src/android_sdk.dart`, `tools/e2e/lib/src/emulators.dart`, `tools/e2e/bin/devices.dart`, `tools/e2e/test/emulators_test.dart`; Create `tools/e2e/lib/src/owned_resources.dart`, `tools/e2e/test/owned_resources_test.dart`, `tools/e2e/test/devices_cli_test.dart`, `tools/e2e/test/process_runner_test.dart`, `tools/e2e/avd_config.json`, `tools/e2e/README.md`.

**Interfaces:** Existing run/startDetached/runInherited preserved for current callers. Add managed `start(executable,List<String>,{required Directory logDir})->Future<OwnedProcess>`; `OwnedProcess {int pid,Future<int> exitCode,Future<void> stop()}` drains stdout/stderr into named files and tracks parent-owned PID. `OwnedResources.add(String label,Future<void> Function() cleanup)`, `close()->Future<List<String>>` reverses order, tries every cleanup, returns secondary failures. `runDevices(List<String>,{required ProcessRunner runner,required AndroidSdk sdk})->Future<int>` injectable CLI. SDK gets avdmanager/system-image lookup; AVD config canonical values in `tools/e2e/avd_config.json` (local API36/google_apis_playstore/x86_64, CI API36/google_apis/x86_64).

- [ ] Extend FakeRunner tests for alreadyrunning AVD success/failure, snapshot save nonzero/missing directory, started-but-adb-not-yet-listed process, wrong image, empty console name, list-avds failure. Real ProcessRunner test launches `Platform.resolvedExecutable --version`, captures exit/output; unknown executable fails visibly. CLI `test -- --name needle` forwards exact args; unknown option before separator rejected with usage explaining `--`.

```dart
test('cleanup tries all resources and preserves first failure', () async {
  final calls = <String>[];
  final owned = OwnedResources();
  owned.add('first', () async { calls.add('first'); });
  owned.add('second', () async { calls.add('second'); throw StateError('stop'); });
  final failures = await owned.close();
  expect(calls, ['second', 'first']);
  expect(failures, hasLength(1));
});
```

- [ ] `dart test test/owned_resources_test.dart test/emulators_test.dart`; new cases RED.
- [ ] ensure explicitly creates `avdmanager create avd --name <name> --package system-images;android-36;<flavor>;x86_64`, checks config.ini image/system arch against config. firstBootArgs adds -no-snapshot-save. Every required command checks exit; snapshot save checks actual clean directory. `ensure` closes only when launched, both success and failure. up preflights all ports before launch; records managed PID immediately so early boot failure can stop it even before adb lists it. Empty/wrong console response names a dead console and suggests manual adb diagnosis; never runs global adb kill-server.

```dart
class OwnedResources {
  final _items = <(String, Future<void> Function())>[];
  void add(String label, Future<void> Function() cleanup) => _items.add((label, cleanup));
  Future<List<String>> close() async {
    final failures = <String>[];
    for (final item in _items.reversed) {
      try { await item.$2(); } catch (error) { failures.add('${item.$1}: $error'); }
    }
    _items.clear();
    return failures;
  }
}
```

- [ ] CLI catches primary code before finally; cleanup failures change success to1 but never replace existing nonzero. Handle Ctrl+C via supported signal subscription, cancel run, close owned resources, exit130; Windows signal support catches UnsupportedError and documents terminal hard termination. Ownership JSON ignored records PID/AVD/reverse without public publishing; next run warns stale ownership and offers explicit owned cleanup, never blindly kills reused PID. PID/path verification required before stop. Unknown user's AVD and preexisting reverse are preserved; claim reverse only if absent before add.
- [ ] CLI tests invoke real Dart CLI subprocess with fake command executables and assert code preservation, remaining cleanup after first failure, handled Ctrl+C cleanup on supported hosts, existing AVD no stop. Run all e2e tool tests PASS. Commit `fix(e2e): preserve resource ownership and failure diagnostics`.

### Task 11: 한 명령의 혼합/야간 E2E 시나리오

**Files:** Create `tools/e2e/bin/run.dart`, `tools/e2e/lib/src/scenario.dart`, `tools/e2e/test/scenario_test.dart`; Modify `tools/e2e/README.md` and `.gitignore` only if build/e2e not already ignored.

**Interfaces:** `dart run bin/run.dart --profile mixed|android|windows --repo-root <absolute>`; default mixed on Windows. mixed: Windows1+Android1+node1, android: Android2+node1, windows: Windows2+node1. `runScenario(Controller,List<Participant>)->Future<void>` starts network, waits Online, connects all unordered pairs, sends both directions unique body, awaits exact received body, verifies path has known selection. Every later send uses fresh token. `RunCancellation` has `Future<void> get cancelled`, `void cancel()` backed by a single Completer, connects signal→owned close; cancel is idempotent.

- [ ] Fake participant scenario test supplies wrong body/peer/sequence or failed command; all cause nonzero and logs. Test all3 unordered pairs are exercised bothways (6 sends), scenario no offline failed text replay, restarted instance changes address and needs new Connect.
- [ ] Run `dart test test/scenario_test.dart`; missing scenario RED.
- [ ] Build node Debug harness and app Debug before launching AVDs. Windows builds exe once, copies complete Debug folder per run if necessary, launches per-instance `--data-dir`. Android builds APK, ensure/up, installs, reverses3340/47000 only if unowned mapping absent, launches Debug intent extras. Start relay readiness then controller then participants. Each external exit/timeout produces immediate stage failure. Each ready state uses timed event expectation, not arbitrary sleep.

```dart
for (var a = 0; a < participants.length; a++) {
  for (var b = a + 1; b < participants.length; b++) {
    final left = participants[a];
    final right = participants[b];
    final info = await right.call('network_info', {}) as Map<String, Object?>;
    await left.call('connect_peer', {'target': info['ticket']});
    final peer = info['endpoint_id']! as String;
    final text = '${left.name}>${right.name}:$a:$b';
    await left.call('send_probe_text', {'peer_id': peer, 'text': text});
    await right.expectEvent((e) => e['kind'] == 'probe_text_received' && e['text'] == text);
  }
}
```

- [ ] Add reversed-direction loop with independent bodies; then set_offline true on one participant, assert send fails, set_offline false, reconnect/recovery, new text arrives, failed text absent. Restart one owned app/node process with same name but new core instance; wait replacement hello and newticket; oldsocket ignored, exchange using newaddress. disconnect_peer stops retry intention. Finish network_stop on survivors; then close allowned resources finally.
- [ ] Failure directory `build/e2e/<timestamp>/<case>` stores controller wire, process stdout/stderr, commands/times, ownership, PID/activity-scoped Android logcat, relevant screenshots. No full device logs. Run Windows mixed locally, Windows-only profile, Linux Android profile on CI. Expect all sends/restore/newaddr/no-replay PASS. Commit `test(e2e): exercise mixed foreground network sessions`.

### Task 12: workflow 정책·생성 도구 인계 항목

**Files:** Modify `tools/ci/check_workflows.py`, `tools/ci/test_check_workflows.py`, `tools/ci/test_pick_simulator.py`, `tools/gen_bindings.sh`, `app/rust_builder/cargokit/build_tool/lib/src/builder.dart`; Create `tools/ci/test_gen_bindings.py`, `tools/ci/test_cargokit_toolchain.py`.

**Interfaces:** Workflow check(path,doc) returns string list, main(root optional for tests) int, malformed YAML produces one filename error. Ordinary trigger allowlist push/pull_request/workflow_dispatch; e2e stem permits schedule additionally; TestFlight tag rules retained. Job permissions may omit/inherit or narrow; reject any write/extra permission. Reusable uses accepts local .github/workflows path or pinnedexternal; local steps ./ action exempt SHA. Generator uses cargo metadata target_directory and strict codegen exact2.11.1, invokes pub get itself.

- [ ] Add existing checker fixture matrix: every default policy, schedule onlye2e, forbidden arbitrary event, jobwrite/reusableunpinned/localaccepted, .yaml/.YAML, nofiles, empty/scalar/list/invalidYAML diagnostic without traceback, existing iOS/testflight conditions. Simulator fixture uses innocuous device name but watchOS runtime, tests stdout UDIDonly.

```python
def test_codegen_rejects_similar_version(self):
    for version in ("2.11.10", "2.11.1-dev", "2.12.0"):
        self.assertNotEqual(self.run_generator(version).returncode, 0)
    self.assertEqual(self.run_generator("2.11.1").returncode, 0)
```

- [ ] `run_generator(version)` is fixture helper within test_gen_bindings.py: temporary fake PATH executables for uname/flutter/codegen/cargo/dart, recording arguments; fake cargo metadata reports target folder with spaces, creates platform fake libs during build. Assert Darwin/MINGW/Linux selectedpaths, pubgetbeforegenerate, tools invoked once; no native product compilation. Run Python discovery; cases RED.
- [ ] Generator verifies version with anchored expected tool output, not substring; metadata target_directory with Python JSON parse then quoted host libraries. Reject unsupported empty CARGOKIT_TOOLCHAIN at vendored builder boundary with clear StateError, preserve1.92.0 override. Update all command documentation to `bash tools/gen_bindings.sh`. Drift job prints `git diff --` actual lines then fails.

```bash
version="$(flutter_rust_bridge_codegen --version)"
if [[ "$version" != "flutter_rust_bridge_codegen 2.11.1" ]]; then
  echo "flutter_rust_bridge_codegen 2.11.1 is required, found: $version" >&2
  exit 1
fi
target_dir="$(cargo metadata --no-deps --format-version 1 | python -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
(cd app && flutter pub get && flutter_rust_bridge_codegen generate && dart run build_runner build)
```

로컬 도구의 실제 출력은 `flutter_rust_bridge_codegen 2.11.1`로 확인했다. 2.11.10이나 prerelease는 허용하지 않는다.

```python
allowed = {"push", "pull_request", "workflow_dispatch"}
if path.stem.lower() == "e2e":
    allowed.add("schedule")
if set(triggers(doc)) - allowed:
    problems.append(f"{path.name}: unsupported workflow trigger")
```

- [ ] Do not fix upstream yanked codegen lock by updating workspace. Test allmodified helper behaviors then real generator on Windows and once macOS Darwin (CI job), exact repeat produces no content diff. cargo-expand install fixed `cargo install cargo-expand --version 1.0.126 --locked`. Commit `fix(ci): enforce workflow and binding generation contracts`.

### Task 13: 일반 CI·야간 분리·Release 검증

**Files:** Modify `.github/workflows/ci.yml`; Create `.github/workflows/e2e.yml`, `tools/ci/check_release_hooks.py`, `tools/ci/test_check_release_hooks.py`, `tools/ci/release_smoke.dart`, `tools/ci/test_release_smoke.py`, `app/integration_test/release_contract_test.dart`; Modify `tools/ci/check_ios_app.sh` only to add marker checking for app's native symbols, keep NSE checks unchanged.

**Interfaces:** `python tools/ci/check_release_hooks.py <APK|exe-release-directory|app-directory>` scans every native library (.so/.dll/.dylib/app Mach-O), marker absent required. No extraction outside temp. `dart tools/ci/release_smoke.dart --platform windows|android --artifact <path> [--serial <emulator>]`는 자신이 실행한 앱만 소유하고 Home 표시와 native core 호출을 확인한다. `app/integration_test/release_contract_test.dart` invoked through existing test_driver using `flutter drive --release` asserts test_hooks_start→NotAvailable and network_info→Stopped with no endpoint. No network_start or public endpoint in automated Release smoke. Dedicated tests do not register Debug test channel. Release UI tests use widget keys for deterministic driver access. 실제 DNS와 송수신은 Task14의 실기기 Release 시험에서 증명한다.

- [ ] Checker fixtures zipped multiABI libraries with marker in onlyone .so fail; cleanfiles pass; missing library/unsupported artifact fails. Release smoke fake process outputs cover startup crash, late native exception, wrong activity/no page, insufficient readiness (PIDalone fails), cleanup after test failure. Fixture runtime checks contract result distinctly from binary marker result.

```dart
testWidgets('release has no controller and starts offline', (tester) async {
  expect(kReleaseMode, isTrue);
  await RustLib.init();
  final support = await getApplicationSupportDirectory();
  await rust.coreStart(dataDir: '${support.path}/release-contract');
  await expectLater(
    rust.testHooksStart(controlUrl: 'ws://127.0.0.1:47000', name: 'forbidden'),
    throwsA(isA<rust.CoreError>().having(
      (e) => e.code, 'code', rust.ErrorCode.notAvailable)));
  final info = await rust.networkInfo();
  expect(info.state, rust.NetworkState.stopped);
  expect(info.endpointId, isNull);
});
```

이 파일은 `flutter test integration_test`의 Debug 전체 실행에서도 발견되므로 main에 `if (!kReleaseMode) return;`을 둔다. Binding 초기화는 기존 IntegrationTestWidgetsFlutterBinding을 쓴다. CI 명령은 app에서 `flutter drive --release --driver=test_driver/integration_test.dart --target=integration_test/release_contract_test.dart -d windows`와 Android emulator의 실제 ID다. Release용 build/run 출력은 Debug job과 별도 산출물에 남긴다.
- [ ] Run helper tests before scripts exist → RED. Implement native marker scanner and Release drive harness. Add Windows Release fullfolder run, Android x86_64 Release emulator run (ARM64 realdevice Task14), manifest permission/Debuggable check; collect output limited to own PID and testwindow. Release core test calls hook method but cannot start WS or set Local profile. 실제 Release relay hostname 조회와 송수신은 Task14 필수 검증이다. CI 스모크의 실행·권한·JNI 로더 성공을 DNS 성공으로 확대 해석하지 않는다. 자동 시험은 공용 Endpoint를 만들지 않는다.

```yaml
name: Network E2E
on:
  workflow_dispatch:
  schedule:
    - cron: '30 17 * * *'
permissions:
  contents: read
concurrency:
  group: network-e2e-${{ github.ref }}
  cancel-in-progress: true
env:
  FLUTTER_VERSION: '3.44.6'
  RUST_TOOLCHAIN: '1.92.0'
  CARGOKIT_TOOLCHAIN: '1.92.0'
```

- [ ] Nightly two jobs: Linux Android2+node via android profile; Windows App2+node via windows profile. Use SHA-pinned actions already in ci.yml, no floating/new unverified SHA. Windows exe never scheduled on Linux. API/flavor/arch read avd_config.json and key caches from exactconfig hash; build before AVD boot, stop Gradle daemon before2AVDs. Capture memory/Gradle peak and reduce existing8G heap only if observed pressure requires. Own emulator management for secondAVD, no hidden reuse of wrong cachedimage. timeout bounded, cleanup always, failure logs retained using existing approved artifact policy with sanitized local-only diagnostic contents.
- [ ] Regular CI runs core/FFI + hook unit tests, Dart tools/widgets, full Windows integration folder, Android integration, Release marker/contract/smoke, iOS simulator+unsigneddevice existing jobs. cargo feature union checked at package boundaries; no Release --all-features. Darwin binding generation once in macOS job. `CARGOKIT_VERBOSE` build logs in Windows/iOS verify Rust1.92.0 actualcommand; reject empty toolchain early. Format onlytouched hand-written files; agree existing Max Rust heuristic/Dartwidth before any formatting, no repositorywide rewrite.
- [ ] Run workflowchecker/Python/Darttests locally; dispatch nightly bothplatform jobs at reviewedcommit; obtain ordinary CI jobs green and final generator drift clean. These are real execution results recorded later, not the current plan's state. Commit `ci: validate release networking and nightly platform E2E`.

### Task 14: 실제 3플랫폼 확인·측정·마무리

**Files:** Create `docs/superpowers/results/2026-10-13-m2-transport.md` (실제 실행일로 날짜 변경), `docs/superpowers/plans/2026-10-13-m2-carry-forward.md`; Modify `docs/superpowers/plans/2026-10-04-p2p-chat-roadmap.md`, `docs/superpowers/plans/2026-10-05-m1a-carry-forward.md`, `docs/superpowers/plans/2026-10-11-m1b-carry-forward.md`의 M2 상태만.

**Interfaces:** Result table columns platform/build/corecommit/testcase/path/expected/actual/evidence/decision. Timing local monotonic start before connect, connected after magic-ready, first selected Direct observation; no cross-device wallclock subtraction. `build/e2e/<run>/real-network.jsonl` stores `sample,connected_ms,direct_ms,path,success,error_code` and confidential raw info separately. Public report has aggregated times/device model/OS/build and no tickets/IP/serial.

- [ ] Check all prior tasks and baseline M1b remain complete. Build actual Windows Releasefolder and Android ARM64 Release APK. Install updates without clearing user data; capture SHA256 and versions. Do Android Release ownID/peerconnect/hostnameDNS/send; assert noDebug channel and no marker. ARMv7 build retained but no claim of actual64-onlydevice execution. First appwindow/error if deniedLAN must be visible.
- [ ] With user's available devices, Windows home↔Android KT LTE10runs using freshconnect eachtime, two-way Korean/4096byte text, Direct/Relay display and 0.5s/1s targets. 10회는 상대가 직접 전달한 ticket 입력을 사용하고 같은 조건을 유지한다. 별도로 EndpointId만 입력해 DHT 조회로 연결되는지 확인하며 두 입력 방식의 시간을 혼합하지 않는다. Start/send count and receipt body must match. Record failures and fallback paths. WiFi→LTE→WiFi: send newtext after recovery, failed offline text absent. Same-process offline restore sameEndpointId; processrestart newEndpointId and manualnewaddress. Manual disconnect noautoretry.
- [ ] Existing signed TestFlightpipeline builds approved M2commit; follow its existing tag/upload authorization rules when publishing. Windows↔iPhone foreground bothways; LANpermission refusal then recovery; screenbackground has no guaranteed reception. Existing M1b notification measurement path preserved until M7 and not conflated with M2chat test. iOSactual Rusttoolchain1.92 log attached. Tests requiring unavailabledevice remain explicitly incomplete, not waived.
- [ ] Results reject performance sample failures without cause/userdecision. Run closurechecks once on finalrevision: workspaceRust tests with packagefeature separation, relevantDarttests, bindingdrift, helpers, CI jobs, bothnightly profiles, Releasechecks, realdevices. Review entirebranch according to chosen execution skill; fix findings then rerun affectedchecks. No unrelated broadtesting if alreadygreen.
- [ ] Update39carryitems mapping below with evidence/status; retain M4folderlock, M7ReleaseKotlin, ARMv7runtime and entitlementapplications as deferred. Save important outcome to agentmemory `[ChatProject]` with concepts ChatProject,M2,iroh. Commit `docs: record M2 transport validation and remaining gates`; use finishing-a-development-branch for integration after checks. No claim M2done until actualrequiredchecks have evidence.

## 승인 설계와 인계 항목의 추적

| 근거 | 구현·검증 작업 |
|---|---|
| M2설계1–3 범위/한코어/선행조건 | 시작기준, Tasks3–4,14 |
| 설계4 Endpoint/보안/조회/키 | Task3, featuretree Task8/13 |
| 설계5 명령/오류/이벤트/replay | Tasks1,4,6–7 |
| 설계6 프레임/경합/재연결/종료 | Tasks2–3,11 |
| 설계7 플랫폼/개발화면 | Tasks5–6,13–14 |
| 설계8 조종/격리/소유/로그 | Tasks7–11 |
| 설계9 Release/CI/실제망/iOS | Tasks12–14 |
| 설계10 파일책임 | 파일책임표와각Task Files |
| M1a M2 #1–4 | Task5 + Task13 Release 실행 |
| M1a #5 | Task14 ARM64 실제, ARMv7 실행 배포 전 |
| M1a #6–9 | Tasks1,4: sink계약·closedstream·모든enum·전역test독점 |
| M1a #10 | Task13 touchedformat만, 기존전역churn후속 |
| M1a #11 | M4 DB전folderlock, Task6/11별도data-dir |
| M1a #12–14 | Task6 오류·스택·bootstrap주입 |
| M1a #15–17 | Task10 CLIseparator·nosnapshotsave·외부exit |
| M1a #18–20 | Task10 processlogs·CLIrealtest·최초exit유지 |
| M1a #21–25 | Task10 deadconsole·기존AVD보존·초기PID·SIGINT·image고정 |
| M1a #26–28 | Tasks10/13 AVDconfigcache·Windows전체integration·메모리실측 |
| M1b M2 #1–5 | Task12 allowlist/jobpermissions/uses/localactions/defaults/glob/YAML |
| M1b #6–8 | Tasks12–13 cargoexpand/pubget/version/targetdir/diff/Darwin; yanked일괄update금지 |
| M1b #9 | Task12 runtime-only Simulator/stdout fixture |
| M1b #10–11 | Tasks12–13 emptytoolchain/Windows&iOSactualRustlog |

## 계획 자체 검토 기록

이 절은 구현 완료 체크가 아니다. 승인 설계 각 절과 M1a28개/M1b11개를 위 표로 대조했다. 14개 작업에 Files/Interfaces와 실패→구현→성공 단계를 넣고 링크·코드 fence·미정 표식을 검사했다. 검토 중 수정한 항목은 Local integration test의 feature 조건, control-ready를 기다리는 비동기 bootstrap, core instance별 순번 보존, Release 자동 시험의 공용망 부작용 제거, 동일 시계의 실제 연결 측정값이다. codegen 실제 출력도 `flutter_rust_bridge_codegen 2.11.1`로 확인했다. 이 문단의 검토는 계획 작성 시점의 기록이다. 이후 구현·실행 결과와 잔여 의존성은 문서 머리의 2026-10-17 상태 및 연결된 결과/인계 문서를 따른다. 계획 승인과 코드 지적 수정은 전체 브랜치 최종 검토·실제 기기 검증의 완료를 뜻하지 않는다.
