# M3 A — Keys and Protected Storage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 재시작해도 같은 기기 키를 쓰는 최소 보호 저장과 계정 생성·복구 기반을 만든다.

**Architecture:** `account/keys`는 파생/비밀 타입, `account/storage`는 암호화와 소유권, `AccountHandle`은 직렬화된 계정 상태를 맡는다. OS adapter는 Rust에서 직접 호출하고 키를 Dart/MethodChannel로 왕복시키지 않는다.

**Tech Stack:** Rust 1.92.0, 기존 tokio/iroh, 아래 고정 crypto crates, Android Keystore/JNI, iOS Keychain/C ABI, Windows DPAPI.

**Spec:** [M3 승인 설계 §3·4·8](../specs/2026-10-18-m3-account-devices-design.md), [전체 계획](2026-10-18-m3-account-devices.md).

## Global Constraints

전체 계획의 Global Constraints를 모두 적용한다. 특히 RecoverySeed **32B**, 문구 **24개**, 확인 **3위치**, 파일 **4MiB**, 연락처 **1000**, pending **32**, tombstone **256**, AES-256-GCM **96비트 nonce**를 고정한다. iOS 최소 15.0/Android minSdk 26 및 기존 iOS App Group을 유지한다. 실제 Keychain access group은 아직 없으므로 A3에서 추가한다.

## Review Focus

- 강제 종료/부분 쓰기: 이전 정상 snapshot 또는 명시적 미완료 상태만 열림 — A2.
- 다른 프로세스/폴더 별칭: secret read 전에 두 번째 writer 거부 — A2/A3.
- Keychain 잠김/Keystore key 유실/DPAPI 실패: 자동 새 키 생성 금지 — A3.
- 전환 중 옛 network event: generation으로 무효화, 진단 peer를 인증된 계정으로 승격하지 않음 — A4.
- 문구 취소/확인/복구 재시작: phrase를 저장하지 않고 계정/게시 상태를 과장하지 않음 — A5.

## 의존성 고정과 검증 근거

아래는 **구현 시 추가할 직접 pin**이다. 기존 전이 의존성이 있다는 이유로 API를 직접 쓸 수 있다고 가정하지 않는다. `Cargo.lock` 전체 upgrade 금지. 기존 iroh/FRB/UniFFI pin은 바꾸지 않는다.

| crate | pin / feature | 확인한 호환성·라이선스 |
|---|---|---|
| bip39 | `=2.2.2`, `std,zeroize`, English만 | CC0-1.0; crate는 MSRV 미선언, zeroize feature 기준 1.51 설명. Rust 1.92 compile gate 필수 |
| ed25519-dalek | `=2.2.0`, `zeroize` | MSRV 1.81, BSD-3-Clause; seed/from_bytes와 strict verify 사용 |
| hkdf / sha2 | `=0.12.4` / `=0.10.9` | MIT OR Apache-2.0, metadata MSRV 미선언; lock에 존재 |
| aes-gcm | `=0.10.3` | MSRV 1.56, MIT OR Apache-2.0 |
| getrandom | `=0.3.4` | MSRV 1.63, MIT OR Apache-2.0; `fill`로 OS CSPRNG |
| zeroize / data-encoding | `=1.9.0` / `=2.11.1` | MSRV 1.85 / 1.48, Apache-2.0 OR MIT / MIT. 기존 lock 버전, zeroize feature `zeroize_derive` |
| base64 / minicbor | `=0.22.1` / `=0.26.5`(`std`) | base64 MSRV1.48 MIT OR Apache-2.0; minicbor MSRV 미선언 BlueOak-1.0.0. 수동 고정 배열 codec, 자동 derive로 wire 확장 금지 |
| windows-sys | Windows only `=0.61.2` | MSRV1.71, MIT OR Apache-2.0; `Win32_Security_Cryptography`, `Win32_System_Memory`, `Win32_Storage_FileSystem`, `Win32_Foundation` |
| pkarr | B1: `=8.1.0`, default-features=false, `dht` | MSRV1.85, MIT. 계정 DHT용; 기존 iroh 주소 조회와 별개 |
| snow | B5: `=0.10.0`, default-features=false, `std,use-chacha20poly1305,use-sha2,use-curve25519,use-getrandom` | MSRV1.85, MIT OR Apache-2.0. 승인된 Noise suite만 활성화 |

근거는 2026-10-18 [crates.io version metadata](https://crates.io/api/v1/crates/pkarr/8.1.0), [bip39 API](https://docs.rs/bip39/2.2.2/bip39/struct.Mnemonic.html), [snow manifest](https://docs.rs/crate/snow/0.10.0/source/Cargo.toml), [Rust 1.92 File locks](https://doc.rust-lang.org/1.92.0/std/fs/struct.File.html#method.try_lock)다. metadata상 MSRV 충족은 transitive/target 컴파일 보증이 아니다. 미선언 crate는 성공 전 호환 판정하지 않는다.

### Task 1: A1 — 키 파생·식별자·공개 벡터

**Files:** Create `core/chat_core/src/account/{mod,types,keys,clock}.rs`, `core/chat_core/tests/account_keys.rs`, `core/chat_core/tests/fixtures/m3-key-vectors.json`; Modify `core/chat_core/{Cargo.toml,src/lib.rs}`, root `Cargo.lock`, `core/chat_core/src/error.rs` (통합 담당만). 신규 core 오류 enum을 추가할 때 `app/rust/src/api/chat.rs`, `app/rust/src/convert.rs`와 생성 binding도 같은 commit에서 동기화한다(C1에서는 계정 API를 추가하며 다시 생성).

**Interfaces:** `PeerId([u8;32])`, `InstallationId([u8;32])`, `Generation([u8;16])`, `FlowId([u8;16])`, `TxId([u8;16])`, `StoreId([u8;16])`, `DeviceKind::{Desktop,Mobile}`. public IDs는 Eq/Ord/표시 가능, `Secret32(Zeroizing<[u8;32]>)`는 Debug/Serialize/Clone을 제공하지 않는다. `AccountKeys { identity: Secret32, peer: PeerId }`, `DeviceKeys { seed: Secret32, transport: Secret32, installation: Secret32, installation_id: InstallationId }`는 비공개 필드를 갖는다. `Clock: Send+Sync { fn unix_seconds(&self)->u64; fn monotonic_ms(&self)->u64; }`의 production SystemClock과 test FakeClock을 제공한다.

- [ ] 공개 BIP39 256-bit fixture의 `entropy_round_trip_24_words`, `fixed_hkdf_domains`, `ids_are_52_lowercase_base32`, `device_keys_are_distinct`, `invalid_word_checksum_and_23_or_25_rejected` 시험을 작성한다. 각 fixture에 entropy/word string/예상 public keys를 고정하고 독립 HKDF/Ed25519 계산으로 교차 확인한다. PBKDF2 seed API 호출이 없는지 확인한다.
- [ ] `cargo test -p chat_core --test account_keys` 실행 → 미구현 API/벡터 assertion FAIL 확인.
- [ ] `keys.rs`에 `encode_recovery(&Secret32)->Result<String,CoreError>`, `decode_recovery(&str)->Result<Secret32,CoreError>`, `derive_account(&Secret32)->Result<AccountKeys,CoreError>`, `derive_device(Secret32)->Result<DeviceKeys,CoreError>`, `random_secret()->Result<Secret32,CoreError>`를 구현한다. labels는 설계 §3 그대로, English NFKD/공백/소문자 정규화만 수행한다. BIP39의 `from_entropy_in`/`parse_in`/`to_entropy`를 쓰고 초과 길이는 파싱 전 거부한다.
- [ ] 같은 명령 PASS, `cargo check --locked -p chat_core`, `cargo +1.92.0 check --locked -p chat_core` PASS 확인. 새 transitive MSRV/license/target 제약을 기록하고 pin 불가면 해당 의존 task를 중단해 원인을 보고한다.
- [ ] 위 파일만 stage하여 `feat(account): add recovery and device key derivation` commit.

### Task 2: A2 — 암호화 snapshot과 writer 소유권

**Files:** Create `core/chat_core/src/account/storage/{mod,schema,atomic,ownership}.rs`, `core/chat_core/tests/account_storage.rs`, `core/chat_core/tests/support/account_store.rs`; Modify `core/chat_core/Cargo.toml`, `Cargo.lock`(Windows atomic replace용 target-specific windows-sys 추가).

**Interfaces:** `KeyProtector: Send+Sync { protect(StoreId,&Secret32)->Result<Vec<u8>,CoreError>; unprotect(StoreId,&[u8])->Result<Secret32,CoreError>; delete(StoreId)->Result<(),CoreError>; }`. `Store::open(root:&Path,protector:Arc<dyn KeyProtector>)->Result<Store,CoreError>`, `Store::load()->Result<Option<StoredPlaintext>,CoreError>`, `Store::commit(&StoredPlaintext)->Result<(),CoreError>`, `Store::discard_pending()->Result<(),CoreError>`, `Store::suspend()/resume()->Result<(),CoreError>`. 메서드는 Rust의 `&mut self`로 직렬화한다. `StoredPlaintext { installation:[u8;32],payload:Zeroizing<Vec<u8>> }`는 저장 계층의 경계이며 일반 event/FFI 타입이 아니다. A2는 파일 상한과 보호 envelope만 구현한다. A5가 최초 typed AccountState codec을 정의하고 B2/B4/B6가 그 schema에 각 typed 상태와 collection 상한을 추가한다. 미래 B 타입 때문에 A2가 컴파일되지 않는 순환 의존성을 만들지 않는다. AccountState를 generic JSON value로 대체하지 않는다.

- [ ] `crash_at_each_replace_boundary_keeps_complete_snapshot`, `two_processes_canonical_folder_one_writer`, `tamper_aad_nonce_version_rejected`, `limits_preserve_old_snapshot`, `missing_wrapping_key_never_creates_new_account`를 작성하고 `cargo test -p chat_core --test account_storage` → FAIL을 확인한다. 프로세스 종료 시험은 자식 프로세스의 임시 폴더만 사용한다.
- [ ] storage를 구현한다. public `store.id`는 random16B, `key.wrap`은 OS 보호 blob, `account.bin`은 `[1,store_id16,installation_pub32,nonce12,ciphertext_with_tag]`의 CBOR다. AAD는 ASCII `ChatProject/store/v1` + 고정 길이 version/store_id/installation이다. StoreId는 sandbox 절대 경로에서 파생하지 않아 iOS 업데이트 시 path 변경을 견디도록 한다. secret 생성→OS protect→durable candidate 도중 중단되면 미완료로 취급하고 고립 자원의 명시적 정리만 허용한다. 유효한 이전 snapshot이 없는 손상을 신규 상태로 취급하지 않는다.
- [ ] 같은 canonical folder의 고정 `.account.lock`에서 secret read 전에 `File::try_lock`으로 writer lock을 획득한다. Windows/Android는 Store 소유 중 유지하고 iOS는 foreground의 짧은 read/commit 동안만 유지한다. atomic 저장은 같은 folder의 임시 파일→sync_all→replace, Unix 부모 dir sync, Windows MoveFileExW(REPLACE_EXISTING|WRITE_THROUGH) 순서다. 임시 파일도 암호문만 담는다. allocation 전 전체 파일 4MiB를 검사한다. payload의 collection 상한은 typed codec을 소유한 A5/B2/B4/B6에서 검사하고 commit 전 전체 크기도 검사한다.
- [ ] `cargo test --locked -p chat_core --test account_storage` → PASS. lock 미지원/flush/replace 실패를 성공으로 취급하지 않는다. iOS suspend/resume의 fake ownership 시험은 native 증거와 구분한다.
- [ ] 작업 파일을 stage하고 `feat(account): persist encrypted account snapshots atomically` commit.

### Task 3: A3 — Windows·Android·iOS의 OS 보호 adapter

**Files:** Create `app/rust/src/platform_keys/{mod,windows,android,ios}.rs`, `app/android/app/src/main/kotlin/dev/chatproject/chat_app/AccountKeyStore.kt`, `app/android/app/src/androidTest/kotlin/dev/chatproject/chat_app/AccountKeyStoreTest.kt`, `app/ios/Runner/AccountKeyStore.swift`, `app/ios/RunnerTests/AccountKeyStoreTests.swift`; Modify `app/rust/{Cargo.toml,src/lib.rs,src/android_jni.rs}`, `app/android/app/src/main/{AndroidManifest.xml,kotlin/dev/chatproject/chat_app/MainActivity.kt}`, `app/android/app/{build.gradle.kts,proguard-rules.pro}`, `app/ios/Runner/{Runner.entitlements,AppDelegate.swift,SceneDelegate.swift,Runner-Bridging-Header.h}`, `app/ios/Runner.xcodeproj/project.pbxproj`, `app/lib/core_client.dart`.

**Interfaces:** `platform_keys::provider(root:&Path)->Result<Arc<dyn KeyProtector>,CoreError>`를 Rust 측에 제공한다. Windows는 직접 DPAPI, Android는 기존 VM/GlobalRef를 사용하는 JNI, iOS는 Runner C ABI `cp_key_protect/unprotect/delete`다. protect/unprotect는 `(store_id:*const u8,input:*const u8,input_len:usize,out:*mut u8,out_cap:usize,out_len:*mut usize)->i32`, delete는 `(store_id:*const u8)->i32`다. StoreId는 항상 16B, buffer 대여는 호출 중에만 허용하며 최대 blob16KiB, status0성공/1locked/2missing/3corrupt/4unavailable/5buffer-too-small이다. 실패 시 out_len=0. Dart에 seed/storage key를 반환하지 않는다.

- [ ] OS별 round-trip/다른 scope/키 삭제 후 open 실패/변조/재시작 유지 시험을 추가하고 미구현 FAIL을 확인한다. Android는 `app/android/`에서 `gradlew.bat :app:connectedDebugAndroidTest`, iOS는 macOS의 `xcodebuild test -workspace app/ios/Runner.xcworkspace -scheme Runner -destination 'platform=iOS Simulator,name=<owned simulator>' -only-testing:RunnerTests/AccountKeyStoreTests`. Windows는 `cargo test -p chat_ffi platform_keys::windows`. device가 준비되지 않았다면 fixture 검증 완료와 실기기 미실시를 구분한다.
- [ ] DPAPI는 현재 사용자 scope, UI 금지, 반환 buffer를 LocalFree로 해제한다. Android는 Keystore AES/GCM key alias=`ChatProject.account.<StoreId hex>`, random IV로 storage key만 wrap하고 StrongBox 지원 시 시도하되 미지원일 때만 일반 Keystore로 fallback한다(UI에 실측 보호 등급 표시). iOS는 같은 alias로 storage key를 Keychain에 저장하고 opaque version marker를 반환한다. AfterFirstUnlockThisDeviceOnly, synchronizable=false, access group=`$(AppIdentifierPrefix)dev.chatproject.chatapp.account`를 Runner에 추가한다. NSE에서 account key를 읽거나 쓰지 않는다.
- [ ] Android는 `getNoBackupFilesDir()/account`, Windows는 LocalAppData/ChatProject/account, iOS는 기존 App Group 내 account 전용 folder를 backup에서 제외한다. 기존 core folder에 M3 상태가 있으면 자동 이동하지 않고 그 위치를 사용한다. native는 path/flags/capability만 반환해도 된다. R8 keep과 Swift source 등록, 서명 profile entitlement 정합성, OS 오류→structured CoreError를 같은 작업에서 구현한다.
- [ ] iOS scene background에서 Rust C ABI `cp_account_suspend()->i32`를 동기 호출해 신규 쓰기 금지/진행 중 쓰기의 유한 시간 내 완료/모든 lock 해제를 수행하고 resume에서 `cp_account_resume()->i32`를 호출한다. 두 함수는 Store gate에 전달하며 성공0/보류1/실패2를 반환하고 main thread를 무기한 기다리게 하지 않는다. 네트워크 권한/전달도 suspend 동안 보류한다. 기한 안에 소유권을 확인할 수 없으면 readonly/pending으로 두고 무리한 secret transfer를 하지 않는다. Android app preview FLAG_SECURE, iOS scene switch overlay용 `setSensitiveScreen(bool)`도 native에 등록하고 C2에서 사용한다.
- [ ] 각 OS에서 위 시험 PASS, Windows 실제 DPAPI module test PASS. Flutter account_storage integration은 C1에서 추가한다. Simulator로는 부팅 후 첫 잠금 해제 이전 상태나 물리적 보호를 증명할 수 없음을 결과에 남긴다. platform files와 manifest/lock 변경은 통합 담당이 stage하고 `feat(account): protect storage keys with platform facilities` commit.

### Task 4: A4 — 저장한 기기 키와 Endpoint의 직렬 전환

**Files:** Create `core/chat_core/src/account/manager.rs`, `core/chat_core/tests/account_endpoint.rs`; Modify `core/chat_core/src/{runtime.rs,lib.rs}`, `core/chat_core/src/transport/{manager,endpoint}.rs`, `app/rust/src/platform_keys/ios.rs`, `app/ios/Runner/SceneDelegate.swift`, `app/ios/Runner/Runner-Bridging-Header.h`.

**Interfaces:** `NetworkHandle::activate_key(key:iroh::SecretKey)->Result<(),CoreError>`를 기존 actor command에 추가한다. `CoreSlot::account()->Result<AccountHandle,CoreError>`를 추가하고 RunningCore에 Store/AccountHandle을 보관한다. 이 task가 `core/chat_core/src/account/manager.rs`에 AccountHandle의 actor shell과 Store 소유권을 만든다(A5가 계정 명령을 확장). `CoreSlot::start`의 진단 호환성을 유지하고 account 초기화는 `CoreSlot::open_account(protector:Arc<dyn KeyProtector>)->Result<AccountHandle,CoreError>`로 한 번만 수행한다. 고정 key를 읽어도 네트워크 시작은 기존의 명시적 동작을 유지한다.

- [ ] `active_and_candidate_restart_keep_endpoint_new_instance`, `transition_closes_old_probe_and_ignores_old_epoch`, `transition_busy_and_bind_failure_keeps_saved_key`를 추가한다. `cargo test -p chat_core --features test-hooks --test account_endpoint` → FAIL을 확인한다.
- [ ] manager의 현재 `SecretKey::generate()`는 진단 모드에 남기고 candidate/active에서는 저장된 transport seed를 주입한다. 전환은 기존 stop/epoch를 사용해 pending dials/session 종료→재bind 순서로 수행하고 실패 시 같은 key로 offline 재시도한다. Start/stop/reset의 일반 공개 API를 늘리지 않으며 active account의 create/recover/link를 거부한다.
- [ ] iOS scene background에서 Rust C ABI `cp_account_suspend()->i32`를 호출해 새 쓰기/비밀 전달 금지, 진행 저장 정리, lock 해제를 수행한다. `cp_account_resume()->i32`는 마지막 완전 snapshot/소유권을 다시 확인한다. 반환값은 성공0/보류1/실패2이며 main thread를 무기한 기다리게 하지 않는다. 확인 불가면 readonly/pending이다. suspend 전후 lock 소유 프로세스 시험과 네트워크 전달 차단 시험을 추가한다.
- [ ] 같은 시험 PASS, 기존 `cargo test --locked -p chat_core --features test-hooks --test transport` PASS. M2 익명 재시작의 키 갱신은 유지한다.
- [ ] `feat(account): bind stable installation transport keys` commit.

### Task 5: A5 — 생성·문구 확인·복구의 로컬 상태 기계

**Files:** Create `core/chat_core/src/account/{creation,recovery}.rs`, `core/chat_core/tests/account_lifecycle.rs`; Modify `core/chat_core/src/account/{manager,mod}.rs`, `core/chat_core/src/account/storage/schema.rs`.

**Interfaces:** `AccountHandle::begin_create()->Result<RecoveryDisplay,CoreError>`(`flow:FlowId,words:Vec<String>,positions:[u8;3]`); `confirm_create(flow:FlowId,answers:[String;3])->Result<(),CoreError>`; `cancel_create(flow:FlowId)->Result<(),CoreError>`; `recover(words:String,confirm_replacement:bool)->Result<(),CoreError>`; `snapshot()->Result<AccountView,CoreError>`. AccountView는 revision/u64, phase(`Unconfigured,Candidate,PendingImport,LocalReady,PublishPending,Active,Conflict,Locked`), optional publicPeerId/installation, devices/contacts/pair의 공개 view만 담는다. 비밀 표시 응답은 일반 event와 별개로 한 번만 제공한다. B2에서 publication 완료를, B6에서 pending import 전환을 완성한다.

- [ ] `three_positions_required_then_phrase_absent_on_disk`, `cancel_before_confirm_no_publish`, `recovery_same_peer_new_device_no_contacts`, `active_folder_overwrite_rejected`, `lost_flow_cannot_approve_new_flow`를 작성하고 `cargo test -p chat_core --test account_lifecycle` → FAIL을 확인한다. 아직 B2 publisher가 없으므로 confirm 전 publish intent 0개, offline 복구에서 PublishPending과 replacement intent 1개를 assert한다.
- [ ] B1 cert가 준비된 뒤 typed `AccountState::{Candidate,PendingImport,Active}` schema의 로컬 계정 부분을 구현한다. 이 단계의 PendingImport는 아직 생성 경로를 열지 않고 B6에서 완성한다. memory 내의 수명 짧은 creation flow를 구현하고 확인 성공 후 Identity signing seed와 DeviceSeed만 commit한 뒤 entropy/words를 해제한다. recover는 새 DeviceSeed/cert와 replacement 의도를 저장하되 record 성공 전에 active로 표시하지 않는다. 기존 Key를 잃은 active folder의 “복구”는 별도의 빈 저장 영역으로 명시적으로 안내하고 원래 영역을 덮어쓰지 않는다.
- [ ] 같은 시험 PASS, `cargo test --locked -p chat_core` PASS. 공개 snapshot/Debug/serialization에 words/private key가 포함되지 않는다는 assert를 포함한다.
- [ ] `feat(account): add creation and recovery lifecycle` commit.


## 공통 오류와 고정 검증값

A1에서 core/FFI 오류 `InvalidRecovery,StorageLocked,StorageMissing,StorageCorrupt,StorageFull,AccountExists,WriterBusy,InvalidInvite,InviteExpired,DeviceLimit,PairMismatch,PairExpired,RecordConflict,RecordUnavailable,AuthFailed,AccountRestricted`를 추가한다. A1 이후에는 해당 오류를 직접 사용한다. C1은 한국어 UI와 계정 명령을 연결한다.

다음 값은 실제 사용자 비밀이 아닌 공개 fixture용 HKDF 계산이다. Python 표준 HMAC-SHA256의 RFC5869 extract/expand로 계획 작성 중 계산했다. 제품 구현 시험 통과를 뜻하지 않는다. A1 테스트는 아래 hex와 `assert_eq!`로 비교한다.

| 입력 / info | 예상 32B output hex |
|---|---|
| Recovery entropy `00`×32 / `identity-ed25519` | `3c5a13c879ac3255f7e9259772c0f6f0e8d8cac255d81a9ee81078da74fd6a01` |
| DeviceSeed `ff`×32 / `transport-ed25519` | `5a57439114b6680ab0d408ea9a8c121f44b9808dfdbfd0038a305d91ef2e7d1c` |
| DeviceSeed `ff`×32 / `installation-ed25519` | `074f0dc1fc5fe8dea217a5a8016f56d861d96d6e283ee85b30aa407ce10587c1` |

추가 assertion: A2 `assert_eq!(after_crash, old_or_complete_new)` 및 `assert!(file_len<=4*1024*1024)`; A3 `assert_eq!(unprotect(protect(key)),key)`와 삭제 후 `StorageMissing`; A4 `assert_eq!(before.endpoint_id,after.endpoint_id)`와 `assert_ne!(before.instance_id,after.instance_id)`; A5 확인 전 `assert_eq!(publish_intents.len(),0)`, offline 복구 뒤 `assert_eq!(contacts.len(),0)`와 PublishPending. 실제 테스트에서는 각 task의 fixture 반환값을 사용한다.
