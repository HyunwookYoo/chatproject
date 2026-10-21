# M4 Encrypted Storage and Persistent Outbox Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** M3 계정을 안전하게 SQLCipher로 이관하고, 같은 인증 경계를 사용하는 1:1 텍스트 저장·페이지 조회·무기한 보내기 큐·전달/읽음 UI를 제공한다.

**Architecture:** 기존 account actor가 계정 권한과 메시지 명령을 직렬화하고, 단일 SQL connection을 소유하는 bounded DB worker에 영속 작업을 맡긴다. 계정은 기존 versioned payload 한 행으로 이관하며 메시지·outbox·receipt는 같은 DB의 원자 transaction으로 처리한다. M3 인증 connection의 별도 frame namespace만 사용하고, Flutter는 하나의 Rust 이벤트 구독에서 파생한 bounded 화면 상태만 가진다.

**Tech Stack:** Rust1.92.0, Flutter3.44.6/Dart3.12.2, FRB2.11.1, UniFFI0.32.2, rusqlite=0.40.2, libsqlite3-sys=0.38.2/SQLCipher4.14.0 community/SQLite3.51.3, 기존 iroh1.3·minicbor·SHA-256·zeroize 및 OS KeyProtector.

**Spec:** [승인된 M4 전체 설계](../specs/2026-10-21-m4-storage-outbox-design.md). 구현자는 이 문서와 spec을 모두 읽는다. 사용자의 2026-10-21 “진행해줘”는 전체 설계 승인과 계획 작성 진행으로 기록한다. **구현 방식과 이 계획의 검토는 아직 미확정이다.** 이 계획 작성 중 제품 코드·의존성·기기·CI 실행은 변경하지 않았다.

## Global Constraints

- 대기 메시지에 **시간 TTL이나 재시도 횟수 만료를 두지 않는다.** 간격은1초,2초,4초…최대5분; 앱 재시작 후 같은 payload/msg_id로 재개.
- outbox 대기/보류 합계 **10,000건 또는128MiB**. 한도/디스크 부족이면 새 enqueue를 거부하고 입력을 유지한다. 기존 대기·기록 자동 퇴거 금지.
- 텍스트 UTF-8 **16KiB 이하**, frame **60KiB 이하**, 메시지 page **50개**, 최대5대라는 M3 권한 한도 유지.
- 전달은 **인증된 상대 기기 하나의 지속 저장 ack**다. write/QUIC ACK를 delivered로 표시하지 않는다. 초기 M4는 양쪽의 상호 연락처 등록 필요.
- MLS는M5; 기기 전체 기록 동기화·그룹·첨부·편집·검색·푸시·메일박스·서버 큐·백업 복원·상시 background 서비스는 이번에 구현하지 않는다.
- M3 계정/키 파생/wire·확인된 record conflict/high-water/pair disclosure 규칙 유지. SQL authority 뒤 stale legacy fallback/무언의 새 계정·DB 생성 금지.
- secret/key/본문/연락처 이름은 로그·제어 wire·exception detail로 남기지 않는다. public IDs, revision, 개수, 고정 오류 코드와 artifact hash만 증거로 남긴다.
- SQL·OS key·fsync는 tokio 네트워크/UI 실행기 밖에서 실행; network await를 건너 SQL transaction/file lock 유지 금지. suspend timeout은 release 완료가 아니다.
- 작업 root는 `C:/Users/hyunw/orca/workspaces/ChatProject/m2-iroh-transport`. 이미 격리된 worktree다. 원본 `C:/ChatProject`와 사용자의3개 기존 dirty M2 문서에는 쓰거나 stage하지 않는다.
- 제품/도구/생성물을 필요한 task에만 수정. `git add .` 금지, exact paths만 commit. 제품 소스 기준은9d1f394, 계획 직전 문서 기준4c67568. 기존 M2/M3 외부 수용 게이트는 계속 열려 있다.
- 공개 DHT/릴레이, 실제 사용자 저장소 이관, 새 실물 설치, 서명·업로드·push·merge·registry 변경은 이 계획의 자동 실행 범위가 아니다. 준비·격리 local/native 검증과 외부 증거의 명확한 분리를 유지한다.

## Review Focus

1. **commit 결과가 불명확한 disk-full/IO 실패:** 성공을 먼저 알리지 않고, 동일 요청을 재조회해 중복 없이 회복 — T2/T5의 `ambiguous_commit_reopens_without_duplicate`.
2. **Candidate/pending 폐기와 새 계정 생성:** SQL mode/key/guard를 버려 빈 계정으로 오인하지 않고 모든 M3 durable 상태를 이관 — T3/T4의 `pending_discard_keeps_sql_authority`.
3. **90일 offline/시계 역행과 만료된 권한:** 대기열은 보존하지만 stale proof로 전송하지 않음 — T7의 `ninety_days_preserves_jobs_but_not_auth`.
4. **취소와 늦은 read/Delivered receipt 경쟁:** 새 송신은 멈추고 실제 도착 사실만 단조롭게 반영 — T7의 `cancel_then_late_receipt_keeps_both_facts`.
5. **snapshot/page 도중 수신·중복·구독 재연결:** 기존 메시지가 빠지거나 버블이 늘어나지 않고 bounded 재조회로 수렴 — T8/T10의 `page_event_race_converges_without_duplicates`.

---

## 1. 작업 전 상태와 의존성 결정

현재 `account/storage/Store`는 legacy AES-GCM 파일 저장이며 manager는 concrete Store를 사용한다. M3 Clock과 canonical root ownership은 이미 있다. 메시지 저장이나 SQLCipher는 아직 없다. 기존 sync `CoreSlot::open_account`의 오류 계약과 public async FRB account API를 유지한다. 초기 open만 blocking pool에서 호출하고 이후 SQL 요청은 async worker를 사용한다.

패키지는 계획 작성 시 **공식 crates archive를 메모리에서 읽어** 확인했다. 설치/lockfile 변경/컴파일은 하지 않았다. `libsqlite3-sys-0.38.2`의 `sqlcipher/sqlite3.c`는 SQLite3051003와 SQLCipher4.14.0 community, source ID `2026-03-13 10:38:09 737ae4a34738ffa0c3ff7f9bb18df914dd1cad163f28fd6b6e114a344fe6alt1`이다. 일반 bundled SQLite의 다른 버전과 혼동하지 않는다.

| 입력 | SHA256 |
|---|---|
| rusqlite0.40.2 archive | `23f2a97da3e3873c73cb2a2e71b35c40ff95e0b1eefa8d72d8499a6928c3b5b3` |
| libsqlite3-sys0.38.2 archive | `f1d20bef17f513b9b3004532233187769cd072d790971f4e4da0e346eb6401e8` |
| SQLCipher C source | `ea0bf0b08f688ca5d9312b2e33e7f81b3f4ae54b5016fb062ae1f2632a30a1b9` |

원본: [rusqlite 공식 archive](https://static.crates.io/crates/rusqlite/rusqlite-0.40.2.crate), [libsqlite3-sys 공식 archive](https://static.crates.io/crates/libsqlite3-sys/libsqlite3-sys-0.38.2.crate). 읽기 조사 결과는 `.superpowers/sdd/2026-10-21-m4-design/dependency-package-recon.json`에 보존한다.

Cargo에서는 두 crate를 정확히 pin한다. Apple target은 `rusqlite/bundled-sqlcipher`와 CommonCrypto, 나머지 지원 host/Android는 `bundled-sqlcipher-vendored-openssl`을 사용한다. Apple build별로 OPENSSL_DIR/LIB_DIR/INCLUDE_DIR 영향을 제거하고 실제 `cipher_provider`가 commoncrypto인지 검증한다. 다른 target은 openssl과 static native linkage를 검증한다. Perl/NDK/MSVC 등의 누락은 자동 plain-SQLite/system-library fallback이 아니라 T1 선행 오류다. 임의 시스템 도구 설치로 해결하지 않고 정확한 누락과 가장 작은 해결 범위를 보고한다. 허용된 target별 provider 선택을 바꾸려면 검증 기록을 갱신한다.

## 2. 파일별 책임과 작업 순서

| 영역/경로 | 책임 | 소유 task |
|---|---|---|
| `core/chat_core/src/storage/{mod,engine,keys,worker,migration}.rs` | SQL 설정, 보호 material, 단일 writer/lifecycle, 이관 | T1–T3 |
| `core/chat_core/src/storage/schema.sql` | storage/account/message/outbox/receipt 스키마; T1에는 storage/account만 | T1/T5 |
| `core/chat_core/src/account/storage/{mod,ownership,atomic}.rs` | legacy reader/guard/lock helper의 최소 추출; codec 유지 | T2–T4 |
| `core/chat_core/src/account/manager.rs` | SQL StoreHandle·async persist·메시지 명령의 기존 actor 통합 | T4/T6–T8 |
| `core/chat_core/src/account/manager/messages.rs` | actor의 M4 명령·인증 frame 적용; 거대한 기존 파일을 재배치하지 않음 | T6/T8 |
| `core/chat_core/src/message/{mod,types,store,wire,scheduler,service}.rs` | typed view, SQL 작업, canonical codec, 재시도, public handle | T5–T8 |
| `app/rust/src/api/message.rs`, `api/chat.rs`, `convert.rs`, `api/mod.rs` | typed bridge·event/error 변환 | T9 |
| `app/lib/message/{message_client,message_model,error_messages}.dart` | 단일 Rust 이벤트 기반 bounded projection | T10 |
| `app/lib/message/screens/{conversation_list_screen,conversation_screen}.dart` | 목록/대화/상태/입력 UX | T11 |
| `core/chat_core/src/puppet/message.rs`, `tools/e2e/lib/src/message_scenario.dart` | owning actor 기반 Debug 조종·재시작 증거 | T12 |
| `.github/workflows/{ci,e2e}.yml`, `tools/ci/check_sqlcipher.py` | 실제 엔진·빌드·Release/OS 검증 통합 | T13 |

모두 한 기능의 선행/후행 의존성이 있어 별도 독립 프로젝트로 나누지 않는다. DAG는 `T1→T2→T3→T4→T5→T6→T7→T8→T9→T10→T11→T12→T13`다. task별 시험은 그 시점의 산출물로 실행 가능해야 하며 나중 task의 미정 API를 임시로 stub해 통과시키지 않는다. shared source 파일을 다른 작업자가 동시에 수정하지 않는다.

## 3. 공통 타입·wire·DB 계약

아래는 이 계획이 정한 이름/값이다. `R<T>` 표기는 설명상 `Result<T, CoreError>`이며 실제 public alias는 불필요하다.

```rust
// message/types.rs; existing PeerId/InstallationId are reused.
struct ConversationId([u8; 32]);
struct MessageId([u8; 16]); // caller's random idempotency token IS the msg_id
struct MessageKey { conversation: ConversationId, sender: PeerId,
                    installation: InstallationId, id: MessageId }
struct MessageCursor { lamport: u64, installation: InstallationId, id: MessageId }
enum DeliveryState { Queued, SentDirect, Delivered, Read, Blocked, Failed, Cancelled }
enum BlockReason { Account, ContactRequired, Storage }
struct MessageView { key: MessageKey, lamport: u64, sent_at_ms: u64,
                     body: String, outgoing: bool, state: DeliveryState,
                     cancel_requested: bool, block: Option<BlockReason> }
struct MessagePage { revision: u64, items: Vec<MessageView>, next: Option<MessageCursor> }
struct ConversationView { id: ConversationId, peer: PeerId,
                          last: Option<MessageView>, unread: u64, change_revision: u64 }
struct ConversationCursor { change_revision: u64, id: ConversationId }
struct ConversationPage { revision: u64, items: Vec<ConversationView>,
                          next: Option<ConversationCursor> }
// wire.rs / scheduler.rs, internal only:
struct TextFrame { key: MessageKey, lamport: u64, prev_hash: [u8;32],
                   sent_at_ms: u64, body: String }
enum ReceiptKind { Delivered, Read }
struct ReceiptFrame { kind: ReceiptKind, key: MessageKey, text_hash: [u8;32],
                      recipient: InstallationId }
enum RejectReason { ContactRequired, UnsupportedVersion, InvalidMessage,
                    StorageUnavailable }
enum Frame { Text(TextFrame), Receipt(ReceiptFrame),
             ReceiptApplied(ReceiptFrame), Reject(RejectReason) }
struct Dispatch { key: MessageKey, peer: PeerId, wire: Zeroizing<Vec<u8>>,
                  ticket: u64 }
struct SessionLease { session: crate::account::connection::Session, ticket: u64 }
enum AttemptOutcome { Written, Receipt(ReceiptFrame), ReceiptApplied(ReceiptFrame),
                      Transient(ErrorCode), Blocked(BlockReason), Permanent(ErrorCode) }
struct AttemptResult { key: MessageKey, lease: SessionLease, outcome: AttemptOutcome }
```

IDs derive the same Clone/Eq/Ord/serde patterns as existing public IDs. Body/wire/SQL/key material is never included in Debug/error formatting. Core events contain IDs/revision/status only, not full message bodies. Rust/Dart page objects may contain body for display and have no whole-process erasure promise.

- ConversationId = SHA256(`b"ChatProject/dm/v1" || min(localPeer32,remotePeer32) || max(...)`). Same PeerId is rejected as SelfConnection. Names/transport/installation never change this ID.
- MessageKey is UNIQUE; additionally reject an inconsistent duplicate ordering tuple `(conversation,lamport,installation,msg_id)` rather than relying on ambiguous cursors. Body is preserved exactly; empty UTF-8 is invalid, size16,384 is accepted and16,385 rejected. SQL Lamport is0…i64::MAX, receive jump above current max+1,000,000 or overflow is rejected.
- Outer frame uses existing account length prefix and `auth::envelope(16, inner)` = canonical `[1,16,bstr(inner)]`. Existing tags2/4/revoke parsing stay unchanged. M4 inner Text is `[1,0,conversation32,msg16,senderPeer32,senderInstall32,lamport,prevHash32,sentMs,text]`; hash = SHA256(inner bytes). `prevHash` chains text within `(conversation,senderInstallation)`, starts zero and is not a receipt chain. A receive gap is recorded but does not reject an otherwise valid authenticated message.
- The60KiB limit and stored `outbox.wire` refer to the complete canonical account body including outer CBOR, excluding its existing4-byte network length prefix. Compute text hash from inner bytes only. Do not accidentally hash a re-encoded or length-prefixed alternate representation on retry.
- Inner receipt is `[1,kind,conversation32,msg16,originPeer32,originInstall32,textHash32,recipientInstall32]`, kind1=Delivered/2=Read. Canonical exact lengths/fields only, no trailing bytes. Recipient peer/install come from the current M3 AuthGrant and must match a recorded attempted receiver of that exact message. Read implies Delivered. ACK after DB commit only; expired/changed grants after a DB await cannot send an ACK.
- Receipt application ACK is `[1,4,conversation32,msg16,originPeer32,originInstall32,textHash32,recipientInstall32,receiptKind]`. It is a response to that authenticated receipt RPC, after original sender's DB commit; recipient removes receipt_job only on this matching ACK. ACK loss causes the same receipt RPC again, never an ACK-of-ACK queue. Text RPC returns Delivered, but the persisted Delivered receipt job can later repeat as a receipt RPC until applied ACK; this also covers sender crash just after the text response. Read uses the same reliable path. Unsolicited ReceiptApplied requests are rejected.
- Fixed rejection response is `[1,3,code]`, code1=ContactRequired/2=UnsupportedVersion/3=InvalidMessage/4=StorageUnavailable. It is correlated only to its authenticated RPC stream and local Dispatch, never accepted as a delivery receipt. Invalid account proof closes the session instead. Existing `reader` must encode valid M4 application rejection as a response rather than dropping it into an indistinguishable connection close; unparseable traffic still mutates nothing.
- Unknown/unsupported/invalid frames cause a fixed error and no message/dedupe/receipt mutation. ContactRequired is a fixed application rejection converted to a retained Blocked job at sender; user Retry or a normal permitted reconnect can recheck, with no hidden contact/request-inbox feature.
- Wire payload, hash, msg_id, Lamport and prev_hash are immutable after enqueue. Delivery/cancel facts do not rewrite the text. `processed_ct` key includes codec1, senderPeer, senderInstallation, hash. Do not prune it while corresponding history is retained. Retry with same msg_id/different body is MessageConflict.
- DB user_version1; `storage_meta` binds DB ID/current account state and has global `message_revision`. `account_state` holds existing private payload + installation in SQL, not a second legacy encrypted file. State bindings Unconfigured/Candidate/Active update atomically with account state.
- `message` retains text/hash/attempted receivers/receipt and cancel facts; `outbox` references it and stores immutable wire, attempts, delay/due and block reason. At most5 attempted receiver installations from accepted membership are retained; unknown/excluded receivers cannot complete a job. Cancel/delivery delete only the retry job, preserving message/hash/receiver facts for late receipts.
- `receipt_job` UNIQUE(message key,recipient install,kind), coalesces Read over Delivered and is not counted as user text quota. Process max32 due jobs/turn, max4 network attempts globally, max1 per target PeerId; async mailboxes32. Receipt transfer is priority-bounded (at most8 consecutive receipts before eligible text), so no starvation.
- Public/core errors added: `InvalidMessage`, `MessageConflict`, `MessageNotFound`, `ContactRequired`, `UnsupportedMessageVersion`, `StorageUncertain`; reuse `StorageFull`, `StorageLocked`, `AccountRestricted`, `MessageTooLarge`, `Busy`. Mirror every affected exhaustive enum/FFI/parser. Error detail is static safe text.

DDL decisions for `storage/schema.sql` (standard SQL type spellings; all BLOB IDs/hashes get exact length CHECKs as above):

```sql
-- T1 creates the first two tables; T5 extends version1 while still pre-shipping.
storage_meta(singleton INTEGER PRIMARY KEY CHECK(singleton=1), schema_version INTEGER,
 db_id BLOB, binding BLOB, legacy_hash BLOB NULL, migration_ready INTEGER,
 message_revision INTEGER NOT NULL CHECK(message_revision>=0));
account_state(singleton INTEGER PRIMARY KEY CHECK(singleton=1), installation BLOB,
 payload BLOB NOT NULL CHECK(length(payload)<=4194304)); -- absent row = Unconfigured
conversation(id BLOB PRIMARY KEY, peer BLOB UNIQUE NOT NULL, lamport_max INTEGER,
 local_prev_hash BLOB, change_revision INTEGER, unread INTEGER);
message(conversation BLOB, sender_peer BLOB, sender_install BLOB, msg_id BLOB,
 lamport INTEGER, prev_hash BLOB, payload_hash BLOB, sent_at_ms INTEGER, recv_at_ms INTEGER,
 body TEXT, outgoing INTEGER, outcome INTEGER, delivered_at_ms INTEGER NULL,
 read_at_ms INTEGER NULL, seen_local INTEGER, cancel_requested INTEGER,
 block_reason INTEGER NULL, failure_code TEXT NULL, attempted_receivers BLOB,
 PRIMARY KEY(conversation,sender_peer,sender_install,msg_id),
 UNIQUE(conversation,lamport,sender_install,msg_id),
 FOREIGN KEY(conversation) REFERENCES conversation(id));
outbox(conversation BLOB, sender_peer BLOB, sender_install BLOB, msg_id BLOB,
 codec INTEGER, wire BLOB CHECK(length(wire)<=61440), attempts INTEGER,
 retry_delay_ms INTEGER, next_retry_at_ms INTEGER, dispatch_state INTEGER,
 PRIMARY KEY(conversation,sender_peer,sender_install,msg_id),
 FOREIGN KEY(conversation,sender_peer,sender_install,msg_id)
 REFERENCES message(conversation,sender_peer,sender_install,msg_id));
processed_ct(codec INTEGER,sender_peer BLOB,sender_install BLOB,hash BLOB,seen_at_ms INTEGER,
 PRIMARY KEY(codec,sender_peer,sender_install,hash));
receipt_job(conversation BLOB,sender_peer BLOB,sender_install BLOB,msg_id BLOB,
 recipient_install BLOB,kind INTEGER,next_retry_at_ms INTEGER,attempts INTEGER,
 PRIMARY KEY(conversation,sender_peer,sender_install,msg_id,recipient_install,kind),
 FOREIGN KEY(conversation,sender_peer,sender_install,msg_id)
 REFERENCES message(conversation,sender_peer,sender_install,msg_id));
```

These are column/constraint contracts, not executable CREATE syntax; add NOT NULL for every non-NULL column, bool/enum/range CHECKs and referenced parent existence. Store wall milliseconds/Lamport/revision as checked0…i64::MAX; attempts saturates at u32::MAX. Indexes: message descending `(conversation,lamport,sender_install,msg_id)`, outbox due `(dispatch_state,next_retry_at_ms,conversation)`, receipt due `(next_retry_at_ms,conversation)`, conversation recency `(change_revision DESC,id DESC)`. outgoing0/1, outcome0Queued/1SentDirect/2Blocked/3Failed, block_reason1Account/2ContactRequired/3Storage, dispatch_state0Due/1InFlight/2Blocked. failure_code stores an allowlisted stable ErrorCode variant name, never an enum ordinal/detail/body. Terminal failure removes retry job and retains Failed message facts; cancellation/delivery likewise retain message/history, not redundant retry wire. Derive read/delivered/cancel precedence from facts, not a single overwriteable rank.

## 4. Tasks

### Task 1: Pin and prove the encrypted SQL engine

**Files:** Modify `core/chat_core/Cargo.toml`, `Cargo.lock`, `core/chat_core/src/lib.rs`; Create `core/chat_core/src/storage/{mod,engine}.rs`, `core/chat_core/src/storage/schema.sql`, `core/chat_core/tests/sql_engine.rs`, `tools/ci/{check_sqlcipher.py,test_check_sqlcipher.py}`.
**Interfaces:** Consumes existing CoreError/Secret32. Produces private `engine::open(path:&Path,key:&Secret32,salt:&[u8;16])->R<Connection>` and `EngineInfo {sqlite_version:i32,cipher_version:String,provider:String}`, `engine::info(&Connection)->R<EngineInfo>`. schema version1 storage_meta/account_state only. No product account routing changes yet. Tests may choose local fixture names; production signatures/contracts in this plan are fixed.

- [ ] **Step 1:** Add `real_cipher_reopens_and_plain_sqlite_is_rejected`: create temp DB, insert public sentinel, close/reopen with same key, reject wrong key; assert runtime SQLite3051003/cipher starts4.14.0, no sentinel in db/wal/journal; platform header32 only iOS; no successful ordinary SQLite open. `plaintext_pragma_noop_is_not_encryption` rejects missing/empty cipher_version.
- [ ] **Step 2:** Run `cargo +1.92.0 test -p chat_core --test sql_engine -- --nocapture`; retain actual pre-change failure (not a fabricated run).
- [ ] **Step 3:** Pin Cargo target dependencies as section1. Resolve with the existing lockfile (`cargo +1.92.0 check -p chat_core`, first resolution necessarily without --locked), inspect lock diff for only new/required dependencies; no blanket generate-lockfile/update. Key using SQLCipher raw32B+16B salt syntax in a zeroizing temporary, then cipher_plaintext_header_size=32 on iOS/0 elsewhere, actual key validation/binding; WAL/FULL/foreign_keys/ temp_store MEMORY, cache_size=-2048; never trace key SQL. `check_sqlcipher.py --manifest <Cargo.lock> --source <registry/sqlcipher/sqlite3.c>` checks exact native crate version/C hash/macros from section1. Record resolved crypto transitive versions/checksums in Cargo.lock.
- [ ] **Step 4:** Same Rust command GREEN; `python -m unittest discover -s tools/ci -p test_check_sqlcipher.py -v` GREEN. Use current MSVC/NDK/Apple build prerequisites; if absent stop this task before migration, preserve concrete prerequisite failure. Compilation/packaging cross-target completion is T13, not claimed here.
- [ ] **Step 5:** Commit exact files: `feat(storage): add verified SQLCipher engine`.

### Task 2: OS-protected DB material and bounded single writer

**Files:** Create `storage/{keys,worker}.rs`, `tests/sql_worker.rs`; Modify `storage/mod.rs`, `account/storage/{ownership,atomic}.rs`, `error.rs`. Test helper Create `tests/support/m4_store.rs`.
**Interfaces:** Consumes T1 engine and existing `KeyProtector`. Produces `StorageHandle::open_blocking(root:PathBuf,protector:Arc<dyn KeyProtector>)->R<(StorageHandle,Option<StoredPlaintext>)>`, cloneable handle with async `load_account()->R<Option<StoredPlaintext>>`, `commit_account(state:StoredPlaintext)->R<()>`, `suspend()->R<()>`, `resume()->R<()>`; `invalidate(ticket:u64)` fences requests. Open_blocking must be called on blocking pool/thread. All subsequent SQL runs on one named worker thread. Account reads/writes are first typed DbOp variants, extended by T5.

- [ ] **Step 1:** Tests `db_key_and_salt_use_distinct_owned_slots`, `worker_queue_busy_does_not_block_network`, `suspend_closes_actual_connection`, `ambiguous_commit_reopens_without_duplicate`. Assert two StoreIds differ from legacy ID; wrong/missing salt fails; stale ticket replies cannot reopen writes; interrupted commit never ACKs success and must reopen/requery before new work.
- [ ] **Step 2:** Run `cargo +1.92.0 test -p chat_core --test sql_worker`, retain RED.
- [ ] **Step 3:** `keys::load_or_prepare(root,protector)->R<DbSecrets>` uses existing32B KeyProtector twice: random DB key32; salt16 padded with16 zero bytes in a separate Secret32. `DbSecrets` never Debug/Clone. Metadata canonical `[1,dbId16,keySlot16,saltSlot16,wrappedKey,wrappedSalt]`, each wrapped blob≤16,384, metadata≤65,536. Write `db.keys.pending` with owned IDs before protect, then atomic `db.keys`; reject nonzero salt padding. Reuse small atomic/ownership helpers without changing legacy codec. Worker channel32, one connection, safe state Closed/Open/Uncertain; any ambiguous commit error gates, closes and requires verified reopen, never assuming rollback from an error string.
- [ ] **Step 4:** GREEN same tests + `cargo +1.92.0 test -p chat_core --test account_storage`. Inject IO without emitting secrets. On iOS use per-operation ownership and release every SQL transaction/connection on suspend; timeout keeps pending/failed, never Completed. Two-native-thread lock test is real OS lock evidence only for that host.
- [ ] **Step 5:** Commit `feat(storage): own encrypted DB keys and writer lifecycle`.

### Task 3: Crash-safe legacy migration and downgrade guard

**Files:** Create `storage/migration.rs`, `tests/account_sql_migration.rs`; Modify `storage/{mod,worker}.rs`, minimal `account/storage/mod.rs` legacy accessor. Use existing `account/storage/schema.rs` unchanged codec.
**Interfaces:** `migration::open_or_migrate(root,protector)->R<OpenedStore>` for worker only; OpenedStore owns Connection, DbSecrets, ownership; `MigrationStage` test-only checkpoints. Reuse existing StoredPlaintext/AccountState codec; fingerprint SHA256 length-prefixed names+bytes of store.id/key.wrap/account.bin, not secret plaintext.

- [ ] **Step 1:** Tests `all_legacy_states_round_trip` for Unconfigured/Candidate/Pinned/PendingImport/Active with history/floors/conflicts/receipts; `every_migration_boundary_restarts_idempotently`, `sql_authority_never_falls_back`, `old_binary_guard_survives_cleanup`, `pending_discard_keeps_sql_authority`. Assert exact public keys/install/revision/EndpointId and internal constant-time private equality, not printed private bytes.
- [ ] **Step 2:** Run `cargo +1.92.0 test --locked -p chat_core --test account_sql_migration --features test-hooks`; record RED.
- [ ] **Step 3:** Implement spec§5 sequence: lock/gate→prepare independent keys→single SQL import/Ready marker→close/reopen/decode compare→atomic `storage.mode=[1,dbId16,sourceHashOrNull]`→legacy cleanup. SQL completed DB and mode binding are authoritative; any SQL artifact with uncertainty prevents auto fresh/fallback. Keep legacy `store.id` guard; new SQL installs also create a16B guard before exposing account. Never-active discard clears account row/binding transactionally, not DB keys/mode. Owned cleanup retries only recorded legacy resources; no broad folder deletion.
- [ ] **Step 4:** Same command GREEN and account_storage regression GREEN. True process kill checkpoints before/after import/mode/key deletion must run in a child executable of this integration-test target; shared production fault hooks absent from Release. Include symlink aliases/canonical paths, partial key resource creation, unknown schema and changed legacy hash.
- [ ] **Step 5:** Commit `feat(storage): migrate protected account snapshots atomically`.

### Task 4: Route the existing account actor through SQL

**Files:** Modify `account/manager.rs`, `account/pair/transaction.rs` only where async persistence propagates, `account/storage/mod.rs`, `runtime.rs`, `app/rust/src/api/account.rs`, `puppet/account.rs`; Create `tests/account_sql_lifecycle.rs`. Update affected existing account tests/helpers only for the new persistence boundary.
**Interfaces:** T2 StorageHandle replaces Manager.store; `Manager::persist(&mut self,state:&AccountState,request:Request)->R<()>` becomes async and awaits a complete SQL commit before assigning new state/permit or sending private data. CoreSlot::open_account stays sync; FRB/puppet callers invoke it through `tokio::task::spawn_blocking`. Existing async public account functions and methods retain signatures.

- [ ] **Step 1:** Add `sql_commit_precedes_pair_disclosure`, `sql_pending_import_receipt_survives_restart`, `lifecycle_fence_wins_during_sql_wait`, `account_open_errors_still_reach_locked_ui`; assert same B5/B6 authorization bytes0 before durable commit and same M3 conflict/history rules. Assert legacy Store::open rejects SQL metadata instead of becoming a second writer.
- [ ] **Step 2:** Run `cargo +1.92.0 test --locked -p chat_core --test account_sql_lifecycle --features test-hooks` RED.
- [ ] **Step 3:** Complete async propagation without holding CoreSlot/account mutex across await. Reserve/obtain existing account slot in blocking open; network remains gated until migration/load validation. Initialize the SQL request fence from the exact ticket returned by existing network.block before exposing the handle. Immediate lifecycle request copies the same existing ticket to StorageHandle.invalidate before actor processing; no independent authorization counter. Actor applies account-state changes serially; post-await checks reject old-ticket publication/permissions. Keep native pending/completed/failed ABI meanings.
- [ ] **Step 4:** GREEN focused test, then `cargo +1.92.0 test --locked -p chat_core --features test-hooks` and `cargo +1.92.0 test --locked -p chat_ffi --features test-hooks`. Old legacy-storage tests remain fixtures; actual account startup now SQL. Preserve every failure and necessary correction; no test weakening to bypass commit checks.
- [ ] **Step 5:** Commit `refactor(account): persist account state through encrypted SQL worker`.

### Task 5: Transactional message repository and stable identity

**Files:** Create `message/{mod,types,store}.rs`, `tests/message_store.rs`, `tests/fixtures/m4-wire-vectors.json`; Modify `storage/{schema.sql,worker}.rs`, `lib.rs`, `error.rs`.
**Interfaces:** Types from§3. Internal `MessageRepo(StorageHandle)` async methods: `open_dm(local:PeerId,peer:PeerId)->R<ConversationView>`, `enqueue(peer:PeerId,id:MessageId,body:String,now_ms:u64)->R<MessageView>`, `load_messages(id:ConversationId,before:Option<MessageCursor>,limit:u32)->R<MessagePage>`, `list_conversations(before:Option<ConversationCursor>,limit:u32)->R<ConversationPage>`. `enqueue` obtains current actor-supplied bound local identity, never from caller-supplied sender. Worker adds typed MessageDbOp/Reply, no arbitrary SQL via FFI.

- [ ] **Step 1:** Tests `enqueue_commits_text_and_job_once`, `idempotent_retry_before_quota`, `full_queue_preserves_old_jobs`, `ambiguous_commit_reopens_without_duplicate`, `utf8_limits_and_lamport_are_exact`, `page_event_race_converges_without_duplicates`.
  ```rust
  assert_eq!(accepted_utf8_bytes, 16_384);
  assert_eq!(reject_utf8_bytes(16_385).code, ErrorCode::MessageTooLarge);
  assert_eq!((jobs_after_same_id_twice, messages_after_same_id_twice), (1, 1));
  assert_eq!(quota_count, 10_000); assert_eq!(quota_bytes, 128 * 1024 * 1024);
  ```
- [ ] **Step 2:** Run `cargo +1.92.0 test --locked -p chat_core --test message_store --features test-hooks` RED.
- [ ] **Step 3:** Implement required tables/UNIQUE/FK/indexes and§3 identity. Account, message and outbox share one DB; enqueue commit precedes result/event/network. Same key/body returns original row even when full or already delivered; same key/different body errors. Count quota by stored wire length; do not materialize all jobs. New DB IO uncertainty is gated until reopened and queried with the original ID.
- [ ] **Step 4:** GREEN focused tests. Cursor is strict tuple comparison and maximum page50 (invalid limit0/>50 returns InvalidMessage); rows ordered newest-first, next cursor is oldest returned tuple. Conversation recency uses change_revision/id; live changes invalidate old list cursor and trigger model resnapshot, not silent snapshot consistency claims.
- [ ] **Step 5:** Commit `feat(message): store conversations and durable outgoing jobs`.

### Task 6: Canonical message frames over M3 authenticated connections

**Files:** Create `message/wire.rs`, `account/manager/messages.rs`, `tests/message_auth.rs`, `tests/message_wire.rs`; Modify `account/{manager,connection}.rs`, message/store.rs, fixtures/m4-wire-vectors.json.
**Interfaces:** `wire::encode_text(&TextFrame)->Zeroizing<Vec<u8>>`, `decode(&[u8])->R<Frame>` with types/fields exactly§3. `Manager::receive_message(&mut self,conn:&Connection,inner:&[u8],request:Request)->R<Vec<u8>>` is async. Private `dispatch_once(job:Dispatch,lease:SessionLease)->AttemptResult` runs in a bounded network task and returns through the actor mailbox; it never mutates DB or waits inside the account actor. Test-only forcing uses the same path, not a new production RPC bypass.

- [ ] **Step 1:** Tests `before_auth_no_message_bytes`, `unknown_contact_gets_no_stored_text`, `sender_and_receipt_are_session_bound`, `expiry_after_db_commit_sends_no_ack`, plus wire tests exact golden bytes/size/noncanonical/trailing/unknown version. Invalid input must not insert processed_ct. M3 control tags retain original results.
- [ ] **Step 2:** Run `cargo +1.92.0 test --locked -p chat_core --test message_wire --test message_auth --features test-hooks` RED.
- [ ] **Step 3:** Route tag16 before revoke parsing, through ready+grant_authorized/contact checks; never from network probe/pair. Bind Text sender/conv to connection, atomically receive/de-duplicate/queue receipt then ACK only under a still-current ticket/grant. Before first outbound byte, persist the chosen allowed receiver/in-flight fact and recheck the lease/ticket. Transient StorageFull/StorageUncertain returns retryable no durable success; contact requirement is explicit retained Blocked at sender. Unknown frame versions never fallback to probe or MLS. Receipt RPC replies with matching ReceiptApplied only after its fact commit; no receipt-job deletion on mere stream write. Network IO stays outside SQL transaction/actor wait; same actor serializes account changes before accepting message operations.
- [ ] **Step 4:** GREEN focused plus `cargo +1.92.0 test --locked -p chat_core --test account_auth --test account_pair_channel --features test-hooks`. Duplicate valid frame returns the same stored ACK; conflicting logical key does not overwrite body. Test real two-endpoint account auth with local record services, not manufactured AuthReady bools.
- [ ] **Step 5:** Commit `feat(message): authorize direct text and durable receipts`.

### Task 7: Indefinite scheduler, receipts and cancellation

**Files:** Create `message/scheduler.rs`, `tests/message_outbox.rs`; Modify `message/store.rs`, account/manager/messages.rs, account/clock.rs only for wakeable controlled time support.
**Interfaces:** Scheduler async `poll_due(&mut self,now_ms:u64)->R<Vec<Dispatch>>` max32, `record_attempt(&mut self,result:AttemptResult,now_ms:u64)->R<()>`; MessageRepo async `cancel_send(key:MessageKey)->R<MessageView>`, `retry_send(key:MessageKey)->R<MessageView>`. Actor async `apply_message_receipt(&mut self,lease:SessionLease,receipt:ReceiptFrame)->R<MessageView>` rechecks registry connection/grant/ticket before SQL. Scheduler uses existing Arc<dyn Clock> plus Notify; default wall-ms helper may derive seconds for old fakes, SystemClock supplies real milliseconds. Existing security expiry still uses unix_seconds.

- [ ] **Step 1:** Tests `backoff_caps_without_exhaustion`, `ninety_days_preserves_jobs_but_not_auth`, `wall_clock_backward_restart_is_bounded`, `cancel_then_late_receipt_keeps_both_facts`, `read_before_delivered_never_regresses`, `same_wire_after_lost_ack_and_crash`, `receipt_retries_until_fact_commit_ack`, `offline_peer_does_not_starve_other_peer`.
  ```rust
  assert_eq!(retry_delays(), [1,2,4,8,16,32,64,128,256,300,300]);
  assert_eq!(remaining_jobs_after_90_days, 1); assert_eq!(stale_auth_bytes, 0);
  assert!(resume_delay_secs <= 300); assert_eq!(distinct_msg_ids, 1);
  ```
- [ ] **Step 2:** Run `cargo +1.92.0 test --locked -p chat_core --test message_outbox --features test-hooks` RED. Advance fake clocks; do not sleep minutes/days.
- [ ] **Step 3:** Retry only immutable committed jobs; counter saturates and never becomes exhaustion. Map transient offline/remote disk-full to due; authorization/storage/contact gates to retained Blocked; permanent malformed/unsupported to retained Failed. Manual Retry only releases resolvable Blocked and rechecks current authorization. Cancel gates new attempts atomically; no remote recall promise. Delivered/Read remove retry payload but retain receipt/cancel facts and attempted receivers on message. Late current valid receipts can prove delivery after cancel; no state regression. In-flight fences do not fabricate delivery or delete a job on timeout.
- [ ] **Step 4:** GREEN focused+message_auth. Restart recovery treats prior InFlight as unknown and resends identical bytes; saved remaining delay clamps0…300s despite wall jumps. Long fake time must expire auth; fresh normal owner publication and proof are required before completion. Enforce§3 fairness/concurrency.
- [ ] **Step 5:** Commit `feat(message): retry pending messages without time expiry`.

### Task 8: Core message API, read facts and revision events

**Files:** Create `message/service.rs`, `tests/message_service.rs`; Modify `message/mod.rs`, account/manager{.rs,/messages.rs}, event.rs, runtime.rs, lib.rs.
**Interfaces:** `AccountHandle::messages()->MessageHandle`; cloneable MessageHandle wraps the same AccountHandle, no new account actor. Public async methods are exactly T5 query/enqueue signatures with `send_text(peer,id,body)->R<MessageView>` (clock internal), plus `cancel_send(key)`, `retry_send(key)`, `mark_read(conversation,keys:Vec<MessageKey>)->R<()>` (≤50 visible incoming keys). Commands travel via private `AccountHandle::message(MessageRequest)->R<MessageReply>`. New CoreEvent variants `MessagesChanged {revision,conversation}` and `MessageStateChanged {revision,key,state,cancel_requested}` have no body.

- [ ] **Step 1:** Tests `queued_reply_precedes_network_but_follows_commit`, `event_gap_recovers_from_db`, `read_only_marks_visible_incoming_ids`, `late_query_cannot_restore_locked_view`, `page_event_race_converges_without_duplicates`. Assert no candidate/pending import can enqueue; a locally established account may enqueue offline but grant restrictions still gate dispatch.
- [ ] **Step 2:** Run `cargo +1.92.0 test --locked -p chat_core --test message_service --features test-hooks` RED.
- [ ] **Step 3:** Integrate MessageRequest into existing account actor and scheduler select loop. M3 lifecycle invalidates IO before waiting; message commit replies/events from old tickets cannot resume networking. Revision increments with each M4 state mutation; reads return a consistent revision. Events after commit reference rows; no unbounded replay on subscription. `mark_read` rejects own/out-of-conversation/unknown IDs, stores facts+receipt_job atomically and does not mark hidden/unloaded messages. Storage Locked invalidates body projection; transient pending jobs survive.
- [ ] **Step 4:** GREEN focused and complete core feature suite. Add EventBus unit assertion that late subscription does not replay message history while account/connection replay remains unchanged.
- [ ] **Step 5:** Commit `feat(core): expose transactional message operations and revisions`.

### Task 9: Typed native/Flutter bridge and generated correspondence

**Files:** Create `app/rust/src/api/message.rs`; Modify api/{mod,chat}.rs, convert.rs, app/lib/account/error_messages.dart only for exhaustive shared errors; regenerate `app/lib/src/rust/**`, `app/rust/src/frb_generated.rs`, Kotlin/Swift outputs through tools/gen_bindings.sh. Tests Create `app/test/message_bridge_test.dart`, extend affected account error tests.
**Interfaces:** FRB message module functions `open_dm(peer:Vec<u8>)`, `list_conversations(before:Option<ConversationCursor>,limit:u32)`, `load_messages(conversation:Vec<u8>,before:Option<MessageCursor>,limit:u32)`, `send_text(peer:Vec<u8>,id:Vec<u8>,body:String)`, `cancel_send(key)`, `retry_send(key)`, `mark_read(conversation,keys)`, mirroring core return views. Exact16/32B identifier and integer bounds at entry, no truncation. No raw SQL, DB keys or auth override exposed.

- [ ] **Step 1:** Add bridge tests for max/short/oversized IDs, unknown enums, UTF-8 boundaries, no missing CoreError mapping, round-trip cursor/state and same owning core instance. Test public read does not equate a transport event with a receipt.
- [ ] **Step 2:** Run `flutter test test/message_bridge_test.dart` from app; retain RED for real missing API before creating manual substitutes.
- [ ] **Step 3:** Implement thin conversion only. Run pinned regeneration through Git Bash on Windows (`C:/Program Files/Git/bin/bash.exe tools/gen_bindings.sh`); no WSL/bare bash ambiguity or hand-written generated bindings. Keep Kotlin/Swift native-instance API and iOS ABI behavior intact. All new error/event mappings mirrored in generated Dart and tools parser in T12.
- [ ] **Step 4:** GREEN `cargo +1.92.0 test --locked -p chat_ffi --features test-hooks`; app `flutter test test/message_bridge_test.dart test/account_errors_test.dart` and `flutter analyze --fatal-infos`. Regenerate a second time and verify zero generated drift from the first capture, preserving original dirty docs.
- [ ] **Step 5:** Commit exact handwritten/generated paths `feat(bridge): expose typed conversation and outbox APIs`.

### Task 10: Bounded Flutter message model

**Files:** Create `app/lib/message/{message_client,message_model,error_messages}.dart`, `app/test/{message_model_test,message_errors_test}.dart`, `app/test/support/fake_message_client.dart`; Modify core_client.dart only to expose FrbMessageClient.
**Interfaces:** MessageClient mirrors T9 Future APIs and uses existing CoreClient.events broadcast (one Rust subscription). MessageModel methods `openConversation(peer)`, `loadOlder()`, `send(body)`, `cancel(key)`, `retry(key)`, `markVisibleRead(keys)`, `reload()`, `dispose()`; owns current pages/revision/busy/error only, no independent DB/outbox truth. Random.secure16B request ID allocated once per send action, retained across response retry; generated bytes are not user ID/PeerId.

- [ ] **Step 1:** Tests `page_event_race_converges_without_duplicates`, `double_tap_and_lost_reply_reuse_request_id`, `failed_save_keeps_composer`, `old_query_after_lock_does_not_restore_text`, `burst_events_coalesce_one_refresh`. Assert page50, max3 displayed pages150; one outstanding refresh and a dirty flag instead of unbounded event futures.
- [ ] **Step 2:** App `flutter test test/message_model_test.dart test/message_errors_test.dart` RED.
- [ ] **Step 3:** Apply revision/ticket guards to every query result; subscribe before snapshot. Duplicate/lower revisions are harmless; gaps coalesce bounded resnapshot. Refresh the loaded window via at most3 pages and merge by MessageKey; conversation recency changes reset paging cursor and refresh/merge list keys, preserve scroll anchor where present. Do not claim a cross-request SQL snapshot during ongoing churn; after events settle pages must converge. Entering Locked clears displayed bodies and stops mark_read, not durable rows. Dispose all Dart listeners without closing the shared Rust bus.
- [ ] **Step 4:** GREEN focused tests + Flutter analyze. Core network/account models continue to share existing Rust events exactly once.
- [ ] **Step 5:** Commit `feat(ui): project message pages and durable send states`.

### Task 11: Conversation screens and accurate delivery UX

**Files:** Create `message/screens/{conversation_list_screen,conversation_screen}.dart`, `app/test/{conversation_list_test,conversation_screen_test}.dart`; Modify main.dart, home/home_screen.dart, account/account_routes.dart, bootstrap.dart for existing contact→DM routing/model lifetime. Use existing theme tokens; no unrelated redesign.
**Interfaces:** `ConversationListScreen(model:MessageModel,onOpen:...)`, `ConversationScreen(model:MessageModel,peer:...)`. Account contact route opens MessageModel conversation; existing account/device/network diagnostic routes remain reachable. Visibility is actual resumed+focused conversation with rendered incoming keys, not merely mounting the widget.

- [ ] **Step 1:** Tests saving→queued only after completion; sent≠delivered; Read/Cancel late receipt UX; quota input retained; mutual-contact Blocked reason/manual retry; foreground visible-message mark_read only; keyboard Enter/newline handling, 200% text/long body accessible scrolling and safe plain-text rendering.
- [ ] **Step 2:** App `flutter test test/conversation_list_test.dart test/conversation_screen_test.dart` RED.
- [ ] **Step 3:** Implement only spec screens9/11/12. Disable duplicate save actions while preserving request token and composer on error. Copy: `보내기 대기`, `전송됨`, `전달됨`, `읽음`, `전송 보류`, `전송 실패`, `전송 취소`; cancellation warning `이미 전송 중인 메시지는 상대에게 도착했을 수 있어요.` Capacity copy explains old messages retained. Ctrl/Cmd+Enter sends; Enter inserts newline; explicit send button remains. No attachment/group/search controls and no “항상 background 전송” promise. MLS absence belongs to development status/about text, not fake encrypted-message assurance.
- [ ] **Step 4:** GREEN focused+affected existing home/account/contact/bootstrap tests; app `flutter analyze --fatal-infos`. Add `integration_test/message_test.dart` native local enqueue/restart/page/typed-event probe for T13 execution. Measure input→render and durable commit separately against existing16ms target, preserve misses rather than early-success hacks.
- [ ] **Step 5:** Commit `feat(ui): add persistent direct conversation screens`.

### Task 12: Debug controls, privacy and owned restart scenario

**Files:** Create `core/chat_core/src/puppet/message.rs`, `tools/e2e/lib/src/message_scenario.dart`, `tools/e2e/test/message_scenario_test.dart`, `core/chat_core/tests/puppet_message.rs`; Modify puppet/{mod,protocol}.rs, tools/e2e/{bin/run.dart,lib/src/{controller,protocol}.dart}, relevant test parser files.
**Interfaces:** Add Debug-only `message_local_{open_dm,list_conversations,load_messages,send_text,cancel_send,retry_send,mark_read}` calling T8 owning handle; `clock_advance {seconds:u64}` configures existing FakeClock before account open and advances both clocks with checked arithmetic (≤31,536,000s per call), wakes scheduler/account expiry but grants no auth. `runMessageScenario(controller,apps,{required restart,evidence})`; CLI `--scenario m4` uses the existing two app participants; chat_node remains owned relay/diagnostic, not a fake OS-protected third account.

- [ ] **Step 1:** Tests every new command/result/event redacted in retained wire; malformed/overflow clock parameters rejected before mutation; no control in Profile/Release; `restart_same_store_keeps_queued_wire`, `contact_block_then_explicit_retry`, `lost_ack_yields_one_receiver_row`, bounded phase failures and exact cleanup. Check sentinel body/phrases do not occur in persisted artifacts.
- [ ] **Step 2:** `cargo +1.92.0 test --locked -p chat_core --test puppet_message --features test-hooks`; from tools/e2e `dart test test/message_scenario_test.dart test/controller_test.dart` RED.
- [ ] **Step 3:** Reuse account fixture setup/SecretJson disposal and immutable local-record configuration. Create distinct disposable accounts, exchange both identity invites/explicit contacts, normal proofs, send→offline→kill owned sender→restart same dir→online→durable ack→read. Force a duplicate and lost ack; compare public IDs/counts/hashes, not logged text. All message_local args/results are wholly redacted like account_local; public summary may expose revision/counts/hash only. An RPC timeout makes that run terminal: cleanup and preserve the failure, do not resume a controller poisoned by a late result. Register ownership before mutation and capture a partial owned Android launch PID before asserting am-start status; no stale-PID kill. Do not extend controller protocols with a privileged generic raw RPC/SQL endpoint.
- [ ] **Step 4:** GREEN focused tests and tools/e2e `dart analyze --fatal-infos`. Execute finite owned `dart run bin/run.dart --profile windows --scenario m4 --repo-root <R>` then `--profile android` and `--profile mixed` where supported/owned. Logs/ownership retained; no shared adb kill, unowned AVD/device takeover or hard-stop cleanup success inference. Existing M2/m3/m3-auth CLI behavior remains covered.
- [ ] **Step 5:** Commit `test(e2e): verify persistent outbox through native restarts`.

### Task 13: Platform and Release regression gates

**Files:** Modify `.github/workflows/{ci,e2e}.yml`, tools/ci/{check_workflows.py,test_check_workflows.py,check_ios_app.sh,check_release_hooks.py,test_check_release_hooks.py} only where SQL/build inputs require it; Extend `app/integration_test/{account_storage_test,release_contract_test,message_test}.dart`; Create `docs/superpowers/results/2026-10-21-m4-storage-outbox.md` evidence template/results.
**Interfaces:** Consumes actual built artifacts, T1 engine manifest, T12 scenario. No new product API. Extend existing native storage fixture only through the existing Debug test-hooks+debug_assertions boundary; Release continues to reject storage fixture injection.

- [ ] **Step 1:** Add failing checks for missing runtime cipher/provider, false lifecycle Completed with open SQL handle, generated drift, new Debug command marker in Release, key/DB/WAL backup exclusion and old record/snapshot preservation. Workflow tests require explicit Windows `cargo test -p chat_core`, SQL prerequisites only in jobs needing them and pinned Rust/Flutter. No success assertion from build-only Apple artifacts.
- [ ] **Step 2:** `python -m unittest discover -s tools/ci -p 'test_*.py' -v` and focused native test compile; record actual RED for each changed boundary without claiming unsupported host execution.
- [ ] **Step 3:** Wire Linux/Windows Rust/FRB builds, Android native worker Keystore/SQL/restart, Apple CommonCrypto compile/simulator and actual suspend-write/read/closed-lock probe. Known host policy must not be substituted for native iOS. Cross-target engine/provider evidence includes exact resolved source hashes. Extend existing Release marker scanner with `message_local_` in addition to prior markers and positive Debug controls. Add ephemeral public sentinel query to existing Release contract, verify close/reopen with OS protector, remove only owned fixture data; no release raw key/SQL debug command exposed.
- [ ] **Step 4:** Run final matrix below sequentially where resources require it. Review source/manifest against approved spec and completed tasks, preserve source/test/artifact attribution. Windows/Android owned local gates may pass while Apple/physical/public gates remain explicitly open; do not mark entire M4 accepted from narrower evidence.
- [ ] **Step 5:** Commit `test(platform): verify encrypted message storage and release boundaries` with exact owned files; results record real commands/counts/failures/cleanup. Stage none of the3 protected M2 docs.

## 5. Final matrix, coverage and handoff

Commands use R=worktree root, A=R/app, T=R/tools/e2e. Windows sets Flutter PATH, `CARGOKIT_TOOLCHAIN=1.92.0`, `CARGOKIT_VERBOSE=1` and the already pinned SDK/NDK. Build/copy ordinary and contract trees/APKs before the next build, with source hash manifests. Never call an old artifact a new-source result.

| Cwd | Final check | Required interpretation |
|---|---|---|
| R | `cargo +1.92.0 test --workspace --locked`; `cargo +1.92.0 test --locked -p chat_core --features test-hooks`; equivalent chat_ffi feature tests | Actual totals/failures, including all old account tests. |
| R | `cargo +1.92.0 clippy --workspace --all-targets -- -D warnings` and core/ffi test-hooks clippy separately | No inherited diagnostic quietly waived. |
| A/T | `flutter test`, `flutter analyze --fatal-infos`; `dart test`, `dart analyze --fatal-infos` | Full runs when source complete; covering reruns labeled composed coverage if initial full run failed. |
| R | Git Bash `tools/gen_bindings.sh`, repeat generation comparison, `python -m unittest discover -s tools/ci -p 'test_*.py' -v` | No manual generated edits; distinguish fixture artifacts and normal output. |
| A | `flutter test integration_test/message_test.dart -d windows` plus affected account storage/lifecycle native tests | OS-protected real SQL, not fake protector claims. |
| T | m4 windows/android/mixed scenario commands T12 + M3-auth/account scenario regressions | Owned two-app actual DB/restart/auth proof and cleanup. |
| A | Ordinary `flutter build windows --release --target lib/main.dart`; `flutter build apk --release --target lib/main.dart`; existing contract builds with unique public nonce | Pin all artifact native members, static crypto linkage/provider; build alone ≠ runtime. |
| R | Existing `check_release_hooks.py`, `release_smoke.dart` ordinary/contract flows + release_storage_fixture_is_unavailable Rust gate | No Debug fixture/control in actual Release; real SQL open/reopen contract success before nonce. |
| Apple runner | Existing simulator/unsigned iOS+NSE checks, SQL runtime/lifecycle lock test with source-pinned CommonCrypto | Record actual run or OPEN with missing environment, not “source present” PASS. |
| Selected physical release devices | Later isolated user-approved storage/restart/offline/send UX and platform key/privacy checks | Concrete build/device scope before install; never open/reset production account implicitly. |

Spec traceability: §1/7 scope/auth→T6/T11/T12; §2 retention/capacity→T5/T7; §3/5 migration authority→T3/T4; §4/6 crypto/worker/transactions→T1/T2/T5; §8 receipts/cancel/read→T6–T8/T11; §9 clocks→T7/T12; §10 UI/events/paging→T8–T11; §11 future boundaries→T5/T6 version tests; §12 acceptance→T12/T13. Review Focus1→T2/T5,2→T3/T4,3/4→T7,5→T8/T10.

- [ ] Each task has retained RED and focused GREEN; classify a first missing-module compile RED separately from a genuine behavioral RED. Never relabel compilation failure as reproduced runtime behavior. Existing behavioral changes/fixes need actual pre-fix behavioral failures; broader regression runs at dependency boundaries. Do not add tests that merely restate implementation for docs/config-only edits.
- [ ] Final handwritten source review addresses all original spec requirements, permission fences, atomicity/IO uncertainty, migrations, secret logging, generated correspondence and artifact provenance. If Subagent-driven selected: task-level fresh reviews plus final branch review. If Native selected: one fresh final branch reviewer, no claim of independent per-task reviews.
- [ ] Audit protected3 M2 hashes unchanged/unstaged, index scoped, actual owned-resource closure. Leave branch/worktree/artifacts and historical failed receipts intact; no push/merge/sign/upload implied.
- [ ] Record current source HEAD, completed tasks, uncovered acceptance gates, exact remaining limitations and next action. No whole-process erasure, full-disk rollback resistance, remote key-copy deletion or M5/6/7/8 features inferred.

## 6. Plan review state

This document is an implementation plan, not executed evidence. The author self-reviews spec coverage, task steps, type/interface consistency, five Review Focus tests and proportion before handoff. Planning fixes within the approved contract do not require repeated design permission. Changes to approved user-visible semantics or security guarantees return to the spec.

After user review, preserve the chosen execution method. **Recommended: Subagent-driven**, because migration and receipt/authorization races can silently lose data or grant authority, and task review can catch them before UI/native integration. Native execution is also possible and avoids repeated agent contexts, with one final independent review. Do not start product changes while this plan review/execution choice is unresolved.
