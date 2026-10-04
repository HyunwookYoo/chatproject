# P2P Chat 구현 로드맵

- 작성일: 2026-10-04
- 기준 설계: [`docs/superpowers/specs/2026-10-04-p2p-chat-design.md`](../specs/2026-10-04-p2p-chat-design.md) (rev.3, 승인)
- 함께 읽을 것: 설계 리뷰 [`2026-10-03-p2p-chat-design-review.md`](../specs/2026-10-03-p2p-chat-design-review.md), 스파이크 결과 [`spikes/a-connectivity/RESULTS.md`](../../../spikes/a-connectivity/RESULTS.md) · [`spikes/b-mls-memory/RESULTS.md`](../../../spikes/b-mls-memory/RESULTS.md) · [`spikes/c-push/RESULTS.md`](../../../spikes/c-push/RESULTS.md)
- UI 목업: Design 캔버스 "사용자 흐름 (시나리오 순)" 페이지, 화면 번호 1–28 (비공개 아티팩트 https://claude.ai/artifact/NbgPT2dyzGTGoZs4s6rG2Y)
- 첫 상세 계획: [`2026-10-04-m1a-core-skeleton.md`](2026-10-04-m1a-core-skeleton.md)
- 자동 테스트 설계: [`docs/superpowers/specs/2026-10-05-test-automation-design.md`](../specs/2026-10-05-test-automation-design.md) (2026-10-05 승인)

---

## 1. 진행 방식

- 설계 18절의 순서를 마일스톤으로 나눈다. 마일스톤마다 **동작하고 시험할 수 있는 소프트웨어**가 나온다.
- 상세 계획(TDD 태스크 단위)은 **그 마일스톤을 시작하기 직전에** 쓴다. 앞 마일스톤에서 배운 것이 뒤 계획을 바꾸기 때문이다. 지금 상세 계획은 M1a 하나다.
- 이 문서는 각 마일스톤의 범위, 완료 기준, 들어가는 화면, 사용자가 준비할 것을 고정한다. 상세 계획은 이 문서의 완료 기준을 테스트로 옮긴다.
- 마일스톤 순서를 바꿀 때는 이 문서를 먼저 고친다.
- 자동 시험의 층, 실행 위치, 도입 순서는 자동 테스트 설계를 따른다. 마일스톤마다 있는 "자동 시험" 줄은 그 설계 9절을 옮긴 것이다. 층 번호 ①–⑤는 그 설계 2절의 번호다(① Rust, ② 화면, ③ 기기 1대, ④ 여러 기기, ⑤ 실기기).

## 2. 저장소 구조 (M1a에서 만들고 이후 계속 씀)

```
ChatProject/
├─ Cargo.toml              Rust 워크스페이스 (spikes/는 제외)
├─ rust-toolchain.toml     Rust 1.92.0 고정
├─ core/
│  ├─ chat_core/           앱 로직 전부. 상태는 여기만 가진다 (설계 12.1)
│  └─ (M1b) chat_nse/      iOS 알림 확장용 정적 라이브러리. MLS 복호 + 알림 문구만
├─ app/                    Flutter 앱 (패키지 chat_app)
│  ├─ rust/                chat_ffi 크레이트: Dart용 다리 + Kotlin·Swift용 다리
│  ├─ rust_builder/        Rust를 각 플랫폼 빌드에 끼워 넣는 도구 (cargokit, 생성됨)
│  └─ lib/                 화면. 색은 lib/theme/tokens.dart 한 곳에만
├─ server/                 (M7부터) 푸시 프록시 + 큐 서버
├─ tools/
│  ├─ e2e/                 기기 관리(M1a)와 여러 기기 시험 지휘(M2부터), Dart
│  ├─ chat_node/           (M2부터) 화면 없는 참가자 + 로컬 릴레이, Rust
│  └─ uniffi-bindgen/      Kotlin·Swift 바인딩 생성기
├─ docs/
└─ spikes/                 스파이크 A·B·C (워크스페이스 밖)
```

## 3. 마일스톤 한눈에

| # | 이름 | 끝나면 되는 것 | 들어가는 화면 |
|---|---|---|---|
| M1a | 코어 뼈대 + 다리 (Windows·Android) | Rust 코어를 Flutter와 Kotlin이 같은 인스턴스로 부르고, Windows 앱과 시험 전용 에뮬레이터에서 테스트가 폰 없이 돈다. CI도 같은 테스트를 돈다 | 5 (빈 첫 화면, 단순판) |
| M1b | iOS 파이프라인 + 기기 실측 | 클라우드 CI가 iOS 앱과 알림 확장을 빌드해 TestFlight에 올린다. NSE 메모리와 APNs 보관 개수를 실제 iPhone에서 잰다 | — |
| M2 | iroh 1:1 직접 연결 | 두 기기가 서로의 주소로 연결해 글자를 주고받는다. 저장·암호화 없음. 여러 기기 시험 장치가 생긴다 | (개발용 화면) |
| M3 | 계정과 기기 | 복구 문구, 기기 인증서, 기기 목록 게시, 초대 QR, 안전번호, 기기 연결·해제·복구 | 1–3, 6–8, 22–28 |
| M4 | 저장 + 보내기 큐 | SQLCipher 저장, 로컬 에코, outbox 재시도, 중복 제거, 메시지 상태 | 9, 11, 12 |
| M5 | MLS 1:1 | 1:1 대화를 MLS로 암호화. 늦게·두 번·순서 없이 와도 받아낸다 | (9, 11 내용 그대로) |
| M6 | 메일박스 기기 | PC·상시 Android가 꺼진 폰 대신 받아 두고 넘겨준다 | 15–21 |
| M7 | 푸시 | 푸시 프록시(서울 VPS) → FCM·APNs. iOS 알림 확장이 복호해서 보여 준다 | 4, 10 |
| M8 | 큐 서버 + 메일박스 자동 판정 | 메일박스가 없는 사람은 서버 큐를 자동으로 쓴다 | 5·12의 상태 카드 |
| M9 | MLS 그룹 | 그룹 만들기, 멤버 추가 요청 → 커미터 확정, 시퀀서 | 13, 14 |
| M10 | 첨부파일 | 파일을 암호화해 경로별로 보낸다 | 14의 파일 카드 |
| M11 | 서울 릴레이 (공개 출시 전) | n0 공개 릴레이를 서울 자체 릴레이로 교체 | — |

**M6과 M8이 오프라인 전달을 완성한다** (설계 18절). 메일박스가 있는 사람은 M6에서, 없는 사람은 M8에서 오프라인 메시지를 받는다.

## 4. 마일스톤별 범위와 완료 기준

완료 기준의 수치는 설계 14절(성능 예산)과 스파이크 실측에서 가져왔다. "기기 확인"은 사람이 실제 기기에서 보는 항목이다.

### M1a — 코어 뼈대 + 다리 (Windows·Android)

- 범위: 저장소·워크스페이스, `chat_core`의 이벤트 버스와 "프로세스당 코어 하나" 시작 규칙, `chat_ffi`(flutter_rust_bridge + UniFFI), Flutter 셸(테마 토큰, 빈 첫 화면), 기기 관리 도구 `tools/e2e`(시험 전용 에뮬레이터 `chat_e2e_1`·`chat_e2e_2`), GitHub Actions CI.
- 완료 기준
  - `cargo test --workspace` 통과. `tools/e2e`의 `dart test` 통과.
  - Windows에서 통합 테스트 통과: 코어를 두 번 시작해도 같은 인스턴스, 다른 폴더로 시작하면 `AlreadyStartedDifferentConfig`, 연결 상태 이벤트가 늦게 구독해도 온다.
  - **폰 없이** 시험 전용 에뮬레이터에서 통합 테스트 통과(`dart run tools/e2e/bin/devices.dart test integration_test`): Kotlin(UniFFI)이 읽은 인스턴스 ID가 Dart가 시작한 코어의 ID와 같다. 설계 12.3의 "Android 수신기는 앱 프로세스 안에서 코어를 그대로 쓴다"의 전제다.
  - 릴리스 빌드가 Windows 앱과 에뮬레이터에서 실행되고 첫 화면이 뜬다. 폰은 선택으로 한 번 눈 확인.
  - CI 네 작업이 녹색: Rust 테스트(Linux), 위젯·도구 테스트(Linux), Windows 빌드 + 통합 테스트, Android 에뮬레이터 통합 테스트.
- 사용자 준비: Windows 개발자 모드 켜기, 에뮬레이터 2대를 띄울 때 남은 메모리 6 GB 확보, GitHub 비공개 저장소 생성 승인.

### M1b — iOS 파이프라인 + 기기 실측

- 범위: GitHub Actions macOS 러너에서 iOS 빌드, App Store Connect API 키로 클라우드 서명, TestFlight 업로드. App Group과 알림 확장(NSE) 타깃 추가(Mac이 없으므로 Ruby `xcodeproj` 스크립트로 프로젝트를 고친다). `chat_nse` 크레이트(UniFFI, `nse` 기능만).
- 기기 실측 (설계 18절에서 미룬 것)
  - 스파이크 B 기기판: 알림 확장이 200 leaf OpenMLS 그룹 상태를 열고 복호한 뒤 자기 메모리(`phys_footprint`)를 App Group 파일에 쓴다. 앱이 그 값을 보여 준다. 기준 15 MB.
  - 스파이크 C iOS판: 꺼진 iPhone에 APNs로 여러 통 보낸 뒤 켰을 때 남는 개수(설계 가정: 앱당 1개)와 지연을 잰다.
- 자동 시험(③): iOS 시뮬레이터 통합 테스트 CI 작업(main 브랜치 push와 매일 밤, 앱 서명 불필요). `xcrun simctl push`로 알림 확장 처리 경로까지 시험할 수 있는지 확인한다.
- 완료 기준: TestFlight 빌드가 iPhone에 설치·실행된다. NSE 피크 메모리 < 15 MB. APNs 실측값을 설계 8.2절과 스파이크 C 결과에 적는다. iOS 시뮬레이터 CI 작업이 녹색.
- 사용자 준비: Apple Developer에서 App ID 2개(앱, NSE)와 App Group 만들기, APNs 인증 키(.p8), App Store Connect API 키(.p8)를 GitHub 저장소 시크릿에 넣기.
- 위험: 15 MB를 넘으면 설계 17절 #1의 대안(알림은 "새 메시지"만)으로 간다.

### M2 — iroh 1:1 직접 연결

- 범위: `chat_core`에 tokio 런타임과 iroh 1.3 엔드포인트. 최소 구성(n0 preset 아님, `dns.iroh.link` 안 씀), n0 공개 릴레이(`RelayMode::Default`), pkarr에는 홈 릴레이 URL만 게시(`AddrFilter::relay_only`). 저장·MLS 없음. Android `JNI_OnLoad`에서 `iroh::dns::install_android_jni_context` 호출.
- 여러 기기 시험 장치(자동 테스트 설계 4절): 지휘 프로그램(`tools/e2e` 시나리오 실행), Rust 조종 루프(`chat_core`의 `test-hooks`, 디버그 빌드에만), `chat_node`(조종 참가자 + 로컬 릴레이), 앱 조종 모드 실행 인자(Android 인텐트 추가 값, Windows 실행 인자), CI 매일 밤 작업, 릴리스 표식 검사.
- 완료 기준
  - 자동(①): 같은 PC 안 두 엔드포인트가 서로의 EndpointId로 연결해 메시지를 주고받는 테스트.
  - 자동(④): 에뮬레이터, `chat_node`, Windows 앱 셋이 로컬 릴레이로 서로 연결해 메시지를 주고받는다. 에뮬레이터에서는 직접 경로를 기대하지 않는다.
  - 자동: 릴리스 APK와 Windows 릴리스의 Rust 라이브러리에 조종 코드 표식 `chat-test-hooks:v1`이 없다.
  - 기기 확인 (Windows 가정 회선 ↔ Android KT LTE): 첫 연결 0.5초 이내, 직접 경로 전환 1초 이내(스파이크 A: 0.19–0.46초, 0.5–0.77초). 와이파이 ↔ LTE 전환 뒤 다시 연결된다.
  - `ConnectionState` 이벤트의 `direct_peers`가 실제 연결 수를 따른다.

### M3 — 계정과 기기

- 범위: RecoverySeed → BIP-39 24단어 → IdentityKey(Ed25519). 기기별 DeviceSeed(OS 키저장소) → TransportKey·InstallationKey. InstallationCert 서명·검증. IdentityRecord(pkarr, Mainline DHT, 기기 5대 상한). 초대장(`Invite`, 선택 표시 이름 64바이트 이하). 안전번호. 기기 연결 양방향(카메라 있는 쪽이 찍음, 반대 방향은 확인 코드 대조 필수). 기기 해제. 복구.
- 완료 기준
  - 자동: 복구 문구 → IdentityKey가 결정적이다(같은 문구 → 같은 PeerId). 인증서·초대장 서명이 위조되면 거부한다. 표시 이름 65바이트는 거부한다. 확인 코드는 양쪽에서 같다.
  - 자동: 반대 방향 연결에서 확인 코드 승인 없이 키가 넘어가지 않는다.
  - 자동(④): 에뮬레이터 2대 + `chat_node`로 초대 → 연락처 추가, 기기 연결(두 방향), 복구 문구로 되살리기. QR 내용은 지휘 프로그램이 넘겨준다(카메라 없이).
  - 기기 확인: 폰 ↔ PC 기기 연결(두 방향), 해제, 복구 문구로 되살린 뒤 같은 PeerId.
- 화면: 1–3 (시작·복구 문구·확인), 6–8 (QR·이름·안전번호), 22–28 (새 폰 연결·해제·복구·경고).

### M4 — 저장 + 보내기 큐

- 범위: SQLite + SQLCipher(`rusqlite` bundled-sqlcipher), DB 키는 DPAPI·Android Keystore에. 설계 11.2 스키마 중 이 단계에 필요한 표. 로컬 에코, outbox 지수 백오프(1초부터 최대 5분), `processed_ct` 중복 제거, 메시지 상태 기계(11.3). 대화 목록·대화방 페이지 단위 로드(50개).
- 완료 기준
  - 자동: 보내기 → `queued` 이벤트가 네트워크를 기다리지 않고 먼저 온다. 앱을 다시 켜도 outbox가 이어서 보낸다. 같은 암호문이 두 번 와도 한 번만 저장한다.
  - 자동(①): 코어가 시계를 `Clock`으로 주입받는다. `clock_advance`로 백오프(1초 … 5분)를 실제로 기다리지 않고 확인한다.
  - 자동(④): 보내는 중에 에뮬레이터 앱을 강제 종료 → 다시 켠 뒤 outbox가 이어서 보낸다.
  - 측정: 입력 → 화면 표시 16 ms 미만(설계 14절).
- 화면: 9, 11, 12.

### M5 — MLS 1:1

- 범위: OpenMLS 0.9, 사이퍼수트 `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`. `max_past_epochs = 5`, `out_of_order_tolerance = 200`, `maximum_forward_distance = 1000`. StorageProvider는 `mls_kv` 표에 바이너리로, 메시지 하나 처리마다 SQLite 트랜잭션 하나. 평문 패딩 버킷(1 KiB / 푸시 버킷 / 16 KiB / 60 KiB). Lamport 정렬. 변경 요청은 `System { ChangeRequest }` 애플리케이션 메시지(proposal 브로드캐스트 금지).
- 완료 기준
  - 자동(①): 가짜 네트워크로 지연·중복·순서 뒤바뀜·유실을 넣는다. 늦게(과거 epoch 5개 이내)·두 번·순서 없이(200개 이내) 온 메시지를 모두 받는다. 범위 밖은 오류 이벤트로 알린다. 되돌아온 자기 메시지는 중복으로 걸러진다.
  - 측정: 1 KiB 암·복호 1 ms 미만. 메시지당 저장 쓰기량(스파이크 B: JSON 437 KiB)을 바이너리에서 다시 잰다.

### M6 — 메일박스 기기

- 범위: `MAILBOX_SYNC_REQ/RESP`, `MAILBOX_OUTBOX_HANDOFF`(설계 3.4), 메일박스 로컬 일련번호. Windows: 동의 후 전원 연결 중 `PowerSetRequest`, 트레이 상주. Android: 상시 연결 모드(`remoteMessaging` 포그라운드 서비스 + 고정 알림 + 배터리 최적화 예외). 7일 가동률 기록. `MailboxAnnounce`. 메일박스의 outbox 백오프 상한 30초.
- 완료 기준
  - 자동: 폰이 꺼진 동안 온 메시지를 메일박스가 받아 두고, 폰이 켜지면 일련번호 순서대로 넘긴다. 여러 메일박스가 같은 암호문을 넘겨도 한 번만 저장한다.
  - 자동(④): 에뮬레이터 비행기 모드 → Windows 앱(메일박스)이 받음 → 비행기 모드 해제 뒤 에뮬레이터가 받음. 에뮬레이터 하나를 상시 연결 모드 메일박스로 둔 경우도 같은 시나리오로.
  - 기기 확인: 폰 비행기 모드 → PC가 받음 → 폰 복귀 후 같은 LAN에서 5 ms 안팎으로 받음.
- 화면: 15–21.
- 사용자 준비: Play Console에 `remoteMessaging` 포그라운드 서비스 사용 신고(Play 배포 시).

### M7 — 푸시

- 범위: 서울 VPS에 무상태 푸시 프록시(`server/`). PushHandle(AEAD, 7일 만료), handle별 rate limit, 0–3초 지터. FCM HTTP v1(서비스 계정), Android 수신기(Kotlin → UniFFI → 코어, 같은 프로세스). APNs(HTTP/2, 인증 키) + iOS NSE(메인 DB 읽기만, inbox 파일에 쓰기). FCM 100개 초과 시 `onDeletedMessages` → 메일박스·큐·상대에게 다시 요청. 알림 권한은 복구 문구 확인 직후(설계 8.6).
- 완료 기준
  - 자동: 푸시 버킷 암호문 2,900 B 이하가 4,096 B 페이로드 안에 들어간다. 프록시는 아무것도 저장하지 않는다(재시작 후 상태 없음).
  - 자동(④): CI는 가짜 푸시 경로로 시험한다. 실제 FCM은 이 PC의 Google Play 이미지 에뮬레이터에서만 돌린다(Firebase 비밀 필요). 알림 권한 창과 알림 누르기 자동화(Patrol)는 이때 다시 검토한다.
  - 기기 확인: 잠긴 Android 폰과 iPhone에 알림이 내용과 함께 뜬다(스파이크 C: FCM p50 170–211 ms).
- 화면: 4, 10.
- 사용자 준비: 서울 VPS 1대, 도메인(TLS), Firebase 서비스 계정을 서버 시크릿으로. 스파이크용 서비스 계정 키는 이때 새로 만들고 스파이크 키는 지운다.

### M8 — 큐 서버 + 메일박스 자동 판정

- 범위: 큐 서버 명령(`CREATE/SEND/RECV/ACK/SUB/DELETE/COMMIT_APPEND`), TTL 24시간(최대 48시간), 암호문 64 KiB 이하, 큐당 미수신 256개 이하, ACK 즉시 삭제, 30일 미사용 큐 삭제. 7일 가동률 90% 미만이면 큐 자동 생성 + QueueRef를 MLS로 알림, 회복하면 해제. 큐 로테이션. 수동 설정(자동/항상/안 함).
- 완료 기준
  - 자동: 받는 사람의 메일박스가 없으면 큐에 넣고, 메일박스가 회복되면 큐를 해제한다. 서버는 큐 ID와 공개키 두 개 말고는 모른다(저장 항목 검사).
  - 자동(④): 로컬 큐 서버를 띄운다. 메일박스 없는 참가자가 꺼진 동안 온 메시지를 켜진 뒤 받는다. 가동률 판정은 `clock_advance`로 7일을 건너뛰어 확인한다.
  - 기기 확인: 메일박스 없는 폰이 꺼진 동안 온 메시지를 켜진 뒤 받는다.
- 화면: 5·12의 상태 카드("메일박스 없음 · 서버가 대신 받아 둬요").

### M9 — MLS 그룹

- 범위: 그룹 생성(커미터 규칙과 시퀀서를 그룹 컨텍스트에), 변경 요청 → 커미터가 by-value로 커밋. 커미터는 메일박스 installation 중 EndpointId가 가장 작은 것, 없으면 큐 서버 시퀀서. 같은 epoch 커밋 충돌은 `hash(commit)`이 작은 쪽, 직전 상태 보관. 24시간 무응답 시 인수인계. 키 갱신(Update)은 본인이 보내고 커미터가 즉시 커밋. 초기 상한 50명.
- 완료 기준
  - 자동: 커밋 두 개 경쟁 → 모든 멤버가 같은 쪽으로 수렴. 커미터가 사라져도 24시간 뒤 다음 순위가 이어받는다. 변경 대기 중에도 메시지는 오간다.
  - 자동(④): 에뮬레이터 2대 + `chat_node` 3개로 5명 그룹. 변경 요청 두 개를 동시에 보내 경쟁시킨다.
- 화면: 13, 14.

### M10 — 첨부파일

- 범위: 랜덤 키 AES-256-GCM, 키는 MLS 안 `Content::Attachment`로. 경로: 둘 다 켜짐 → iroh 직접 스트림, 메일박스 → 메일박스, 큐 사용자 → 큐 서버 blob(TTL), 그 밖 → 보낸 기기 보관·재시도. 초기 상한 25 MB.
- 완료 기준: 각 경로마다 25 MB 파일 왕복과 해시 일치. 서버 blob에 키가 없음.

### M11 — 서울 릴레이 (공개 출시 전)

- 범위: 서울 자체 iroh-relay(무상태, `enable_quic_addr_discovery` 켬, TLS, UDP 7842). 릴레이 맵을 원격 설정으로 받게 해 앱 업데이트 없이 바꾼다.
- 완료 기준: 릴레이 경유 왕복 약 9 ms(설계 5.2). 직접 연결 비율이 n0 때보다 떨어지지 않는다.
- 사용자 준비: 서울 VPS(푸시·큐와 같은 VPS 또는 별도), 도메인.

## 5. 기술 스택 (확정)

| 영역 | 기술 | 버전 | 쓰는 곳 |
|---|---|---|---|
| 화면 | Flutter (Dart) | 3.44.6 / Dart 3.12.2 | 앱 전체 UI |
| 화면 코드 생성 | freezed, build_runner | pub 최신 | Rust enum → Dart 클래스 |
| Dart ↔ Rust | flutter_rust_bridge | =2.11.1 (Rust·Dart·codegen 같게) | 명령과 이벤트 스트림 |
| Kotlin·Swift ↔ Rust | UniFFI | 0.32.2 | Android 푸시 수신기, iOS 알림 확장 |
| Rust 빌드 연결 | cargokit (flutter_rust_bridge가 생성) | — | 각 플랫폼 빌드에 Rust 끼우기 |
| 코어 언어 | Rust | 1.92.0 | `chat_core` |
| 비동기 | tokio | 1.x | M2부터 |
| 연결 | iroh | 1.3 | QUIC, 홀펀칭, 릴레이 |
| 주소·기기 목록 | pkarr (Mainline DHT) | 8.x | IdentityRecord |
| 그룹 암호 | OpenMLS | 0.9 | 1:1과 그룹 |
| 서명 키 | Ed25519 (ed25519-dalek) | iroh와 같은 메이저 | IdentityKey, 인증서 |
| 복구 문구 | BIP-39 (bip39 크레이트) | 3.x | 24단어 |
| 저장 | SQLite + SQLCipher (rusqlite, bundled-sqlcipher) | 0.40 | 로컬 DB |
| 키 보관 | DPAPI / Android Keystore / iOS Keychain | OS | DB 키, 기기 키 |
| 푸시 | FCM HTTP v1, APNs, (선택) UnifiedPush | — | 깨우기·미리보기 |
| 서버 | 푸시 프록시 + 큐 서버, 서울 VPS 1대 | — | M7, M8 |
| CI·배포 | GitHub(비공개) + GitHub Actions, TestFlight | — | 빌드·서명·배포 |
| 기기 시험 | Android Emulator(이 PC는 WHPX, CI는 KVM), `reactivecircus/android-emulator-runner` | API 36 x86_64 | 기기 1대·여러 기기 시험 |
| 시험 도구 | `tools/e2e`(Dart: args, path, test), `chat_node`(Rust, M2부터) | — | 에뮬레이터 관리, 여러 기기 시나리오 |

서버 구현 언어는 설계에 정해져 있지 않다. 이 로드맵은 **Rust(axum + tokio)**를 기본으로 제안한다. 봉투(`Envelope`)와 큐 명령 타입을 코어와 같은 크레이트로 나눠 쓸 수 있기 때문이다. M7 상세 계획에서 확정한다.

## 6. UI 목업에서 나온 결정 (설계에 반영됨, 2026-10-04 사용자 승인)

| 결정 | 설계 위치 | 마일스톤 |
|---|---|---|
| 초대장에 선택 표시 이름(64바이트 이하). 받는 쪽이 바꿀 수 있음 | 4.4 | M3 |
| 기기 연결은 카메라 있는 쪽이 찍음. 반대 방향은 확인 코드 대조 + 기존 기기 승인 필수 | 4.5 | M3 |
| 알림 권한은 복구 문구 확인 직후에 물음 | 8.6 | M7 |
| 색은 `app/lib/theme/tokens.dart` 한 곳에만. 청회색 + 민트는 테스트용 잠정 색 | — | M1a부터 |

## 7. 마일스톤 밖에서 계속 챙길 것

- Apple entitlement 신청(알림 필터링, multicast): M1b에서 신청, 승인 전에는 현행 폴백(설계 17절 #12).
- n0 공개 릴레이 rate limit: 걸리면 M11을 앞당긴다.
- 스파이크 A 미측정분(LTE ↔ LTE, SKT·LG U+): M2 기기 확인 때 함께 잰다.
- 스파이크 C의 Firebase 서비스 계정 키: M7에서 운영용 키를 만들 때 삭제한다.
- `docs/study/p2p-chat/` 학습 문서는 rev.2 기준이다. 필요할 때 rev.3로 다시 만든다.
