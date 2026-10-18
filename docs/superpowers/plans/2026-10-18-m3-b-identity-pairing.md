# M3 B — Identity and Pairing Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 서명한 기기 목록·일반 계정 인증·연락처와 양방향 기기 연결을 durable하게 구현한다.

**Architecture:** 순수 codec/policy를 외부 I/O와 분리하고 하나의 AccountHandle이 Store와 계정별 변경을 직렬화한다. 일반 account stream과 Noise pair stream은 M2 stream/session의 가정을 공유하지 않는다. DHT의 응답은 전역 합의가 아니라 관측 결과다.

**Tech Stack:** [A의 pin](2026-10-18-m3-a-keys-storage.md), pkarr 8.1.0(dht only), minicbor 0.26.5, snow 0.10.0, 기존 iroh 1.3.0/tokio.

**Spec:** [M3 승인 설계 §5–8](../specs/2026-10-18-m3-account-devices-design.md), [전체 계획](2026-10-18-m3-account-devices.md).

## Global Constraints

전체 계획의 Global Constraints와 A의 secret/Store/Clock 타입을 적용한다. 기기 **5**, packet **1000B**, record **7일/24시간**, 미래 **5분**, 초대 **4096B/64B/256B**, pair **5분/3회/60초** 및 승인 전 gated **0B**를 변경하지 않는다. 계정 auth-ready로 M2 연결 시각을 다시 정의하지 않는다.

## Review Focus

- 동일seq 다른packet/낮은seq/미래시계: durable floor 보존, 자동 병합·자동성공 금지 — B2.
- record 만료·자기 기기 제거·generation 변경: 기존 연결 권한도 즉시 제한/무효화 — B3.
- 이름의 다중바이트/제어/bidi·악성 URL: byte 한도 검사, network side effect 없는 parse — B4.
- QR 탈취 후보·역방향·다른연결 proof: E 로컬 승인과 pinned endpoint를 대체할 수 없음 — B5.
- 키 전송/receipt/publish 경계의 crash: 공개목록 미등재 N에 일반 권한을 주지 않고 같은 tx만 재개 — B6.

## Wire 계약 — 모든 task가 공유

CBOR는 definite 배열/shortest 정수/고정길이 bytes, kind desktop=0/mobile=1, optional은 null이다. map/float/tag/indefinite/trailing bytes/추가필드/중복기기/비정규 encoding은 거부한다. signed wire 배열의 마지막 sig를 제외한 나머지 배열을 다시 인코딩한 `body`에 `ASCII domain || u8(version) || body`를 서명한다. version은 1이며 body에도 포함한다. cert hash/record hash는 각각 정규 전체 wire/signed packet bytes의 SHA256이다. endpoint relay는 UTF-8 text URL이며 이름과 마찬가지로 CBOR text 예외다.

| 타입 / 소유 task | 정확한 배열 순서 |
|---|---|
| InstallationCert / B1 | `[1,peer32,installation32,transport32,kind,created_at_seconds,sig64]`; domain `ChatProject/cert/v1` |
| IdentityRecord / B1 | `[1,generation16,issued_at_seconds,expires_at_seconds,[[installation32,transport32,kind],...]]`; installation raw bytes로 정렬. IdentityKey의 pkarr 서명 하나를 사용하고 내부 중복서명 없음 |
| EndpointHint / B4 | `[transport32,relay_url_or_null]` |
| IdentityInvite / B4 | `[1,peer32,cert_array,hint_array,nonce16,name_or_null,issued_at_seconds,expires_at_seconds,sig64]`; domain `ChatProject/identity-invite/v1` |
| Safety QR / B4 | `[1,min_peer32,max_peer32,digest32]`; scheme `chatproject:safety/1/<base64url_no_padding>`, 두 raw PeerId를 정렬하고 digest를 다시 검산 |
| AuthHello / B3 | `[1,peer32,generation16,cert_array,nonce32]` |
| Auth context / B3 | `[1,[dialer_peer32,endpoint32,installation32,cert_hash32,generation16,nonce32],[acceptor_peer32,endpoint32,installation32,cert_hash32,generation16,nonce32]]` |
| Pair QR / B5 | `[1,direction,session16,owner_transport32,owner_installation32,existing_peer32_or_null,resume_tx16_or_null,relay_url_or_null,expires_at_seconds,psk32]`; direction 0=N 표시/E 연결, 1=E 표시/N 연결. scheme `chatproject:pair/1/<base64url_no_padding>` |
| Pair preface / B5 | `[1,session16,role,transport32,installation32,existing_peer32_or_null,resume_tx16_or_null]`; role E=0/N=1 |
| Noise prologue / B5 | `[1,direction,session16,E_peer32,E_transport32,E_installation32,N_transport32,N_installation32,resume_tx16_or_null]` |
| Pair proof / B5 | `[1,session16,role,handshake_hash32,prologue_hash32,sig64]`를 InstallationKey로 domain `ChatProject/pair-proof/v1`에 서명한다. E는 자신의 기존 cert도 전달한다 |
| PairPayload / B6 | `[1,tx16,identity_signing_seed32,N_cert_array,generation16,base_hash32,contacts_array]`. contacts는 `[peer32,alias,verified_0_or_1,verified_digest32_or_null]`의 배열(alias64UTF-8B, 1000개 이하, verification은 정수0/1). 일반 event로 전달 불가 |
| Receipt / B6 | `[1,tx16,payload_hash32,N_installation32,session16,handshake_hash32,sig64]`; N InstallationKey, domain `ChatProject/pair-receipt/v1`. 재개 시 durable receipt 내용에 새 session/hash의 새 서명을 추가해 현재 channel에 바인딩 |
| RevokeIntent / B2 | `[1,peer32,generation16,base_hash32,target_installation32,target_transport32,nonce16,issued_at_seconds,sig64]`; IdentityKey, domain `ChatProject/revoke/v1` |

pair frame은 u32 big-endian length + Noise ciphertext이며 개별 상한은 65535B, plaintext chunk는 16KiB 이하다. transfer envelope는 `[1,tx16,total_bytes,payload_hash32]`, chunk는 `[1,tx16,index,bytes]`이며 total 상한은 4MiB, index는 연속이어야 하고 마지막에 hash와 typed payload 전체를 검증한다. 중복/건너뛴 index/초과 시 종료한다. hello/preface는 인코딩 전체16KiB 이하이며 handshake message는65535B 이하, QR decoded는4096B 이하/relay256B 이하로 제한하고 긴 입력은 allocation 전에 거부한다. account control은 `[1,kind,payload]`로 감싸며 kind 0=AuthHello/1=proof sig64/2=public signed packet bytes/3=RevokeIntent/4=reauth request다. proof는 context/exporter에 묶인 서명이며 wire payload에 private key는 없다. Noise AEAD/nonce는 snow에 맡기고 자체 nonce를 만들지 않는다. account control frame도 u32 length이며 상한은 64KiB다. contacts 전송 등은 pair 이외에 추가하지 않는다.

### Task 1: B1 — cert·record codec과 정확한 pkarr adapter

**Files:** Create `core/chat_core/src/account/{wire,certificate,record,pkarr_backend}.rs`, `core/chat_core/tests/account_wire.rs`, `core/chat_core/tests/fixtures/m3-wire-vectors.json`; Modify `core/chat_core/Cargo.toml`, `Cargo.lock`, `core/chat_core/src/account/mod.rs`(통합 담당).

**Interfaces:** `InstallationCert`/`IdentityRecord`는 위 표의 typed fields다. `sign_cert(keys:&AccountKeys,device:&DeviceKeys,kind:DeviceKind,at:u64)->Result<InstallationCert,CoreError>`, `verify_cert(&InstallationCert,expected:PeerId)->Result<(),CoreError>`, `record_packet(keys:&AccountKeys,record:&IdentityRecord,seq_us:u64)->Result<pkarr::SignedPacket,CoreError>`, `parse_record(packet:&pkarr::SignedPacket,expected:PeerId,now:u64)->Result<IdentityRecord,CoreError>`. `RecordBackend: Send+Sync`는 boxed Send future로 `lookup(PeerId)->Result<Lookup,CoreError>`/`publish(&SignedPacket)->Result<(),CoreError>`를 제공한다. `Lookup::{Found(SignedPacket),CompletedAbsent}`. 실패는 Result::Err로 유지한다.

- [ ] golden cert 서명/record DNS/wrapper vector를 만들고 `five_devices_under_1000_wrapper_and_bep44`, `six_devices_and_duplicate_rejected`, `strict_cbor_and_wrong_domain_rejected`, `packet_has_no_ip_cert_name_relay`를 추가한다. `cargo test -p chat_core --test account_wire` → FAIL.
- [ ] minicbor로 위 배열의 엄격한 codec/strict Ed25519 검증을 구현한다. `_account`의 TXT 한 개에만 payload의 base64url을 저장한다. 긴 TXT는 DNS 표준의 255B 이하 character-string으로 분할하고 하나의 RR 안에서만 재연결한다. name은 pkarr 정규화 origin에 대한 상대 이름이다. TTL86400초와 앱 expiry604800초는 별도 계약이다. 수신 측도 wrapper1000B를 검사하고 알 수 없는 RR/복수의 _account를 거부한다.
- [ ] `pkarr=8.1.0`을 pin하고 `SignedPacket::builder().txt(...).timestamp(Timestamp::from(seq_us)).sign(...)`, `as_bytes().len()`(32pub+64sig+8timestamp+DNS)을 사용한다. BEP44value의 DNS bytes 길이도 별도로 기록한다. PeerId 표시는 RFC4648이며 pkarr의 z-base32 문자열을 PeerId로 해석하지 않는다. raw32B를 PublicKey/Keypair에 전달한다.
- [ ] adapter는 DHT only다(새 HTTP relay 의존성을 기본으로 활성화하지 않는다). `Client::resolve(&key,ResolvePolicy::NetworkOnly)`를 사용하고 `NotFound`만 CompletedAbsent로 취급하며 NoResponses/NoDhtNodesQueried/NoUsableResponses/InvalidSignedPacket 등은 대기/오류로 처리한다. NetworkOnly/오류 enum은 8.1.0 공식 배포 source에서 확인했다. compile로 pin을 검증하고 다른 policy로의 무단 fallback을 금지한다. 낮은 seq/expired packet을 cache freshness에 따라 제거하는 것과 앱의 7일 기한의 차이도 시험한다. fake backend에서는 모든 결과를 주입한다.
- [ ] unit PASS, `cargo +1.92.0 check --locked -p chat_core` PASS, Android/iOS target compile로 DHT 의존성을 확인한다. 네트워크 호출은 fake/loopback만 사용한다. `feat(identity): add signed account record codecs` commit.

### Task 2: B2 — durable publication·충돌·다른 기기 해제

**Files:** Create `core/chat_core/src/account/{publication,devices}.rs`, `core/chat_core/tests/account_publication.rs`; Modify `core/chat_core/src/account/{manager,record}.rs`, `core/chat_core/src/account/storage/schema.rs`.

**Interfaces:** `RecordState { accepted:Option<SignedRecord>,observed_seq:u64,allocated_seq:u64,tombstones:Vec<RevokeIntent>,pending:Vec<PublishIntent> }`. SignedRecord는 검증된 packet/record/hash, PublishIntent는 정확한 packet/base hash/작업/상태다. `AccountHandle::refresh_record(peer:PeerId)->Result<(),CoreError>`, `revoke_device(target:InstallationId)->Result<(),CoreError>`, `confirm_record_conflict(base_hash:[u8;32])->Result<(),CoreError>`. `PublicationState::{LocalBlocked,Pending,Observed,Conflict}`를 AccountView에 포함한다. 최초 확인/복구를 담당하는 A5는 이 publisher를 호출한다.

- [ ] `unavailable_is_not_absent`, `first_publish_after_confirmation`, `expired_seven_days_renew_same_generation_no_readd`, `recovery_replacement_one_new_device`, `same_seq_different_payload_conflict`, `durable_floor_survives_restart_and_clock_rollback`, `concurrent_add_revoke_recovery_no_silent_merge`, `tombstone_256_and_pending_32_preserve_old_state`를 fake backend/Clock으로 작성한다. `cargo test -p chat_core --test account_publication` → FAIL.
- [ ] 설계 §5.1 표의 각 분기를 구현한다. seq는 Unix microseconds로 `max(now_us,observed+1,allocated+1)`이며 now_us는 Clock의 unix_seconds를 checked_mul(1_000_000)한 값이다. seq는 i64::MAX 이내여야 하고 현재 시각+300초보다 미래이면 보류한다. 할당/packet/publish 의도를 먼저 commit한다. 같은 packet retry만 같은 seq를 사용한다. 관측 floor를 낮추지 않고 같은 seq의 다른 hash는 충돌이다. public completion은 publish ack가 아니라 재조회한 정확한 packet/연결된 내 기기 sync의 관측 상태로 표시한다.
- [ ] 다른 기기 revoke는 현재/마지막 기기에 대해 거부하고 local tombstone 저장→연결 중인 같은 PeerId의 인증된 기기로 signed intent 전송→public mutation 순서로 수행한다. 통신하지 않는 기기가 즉시 해제됐다고 표시하지 않는다. generation 변경/권한 상실 시 기존 pending을 중지한다. 충돌 확인은 화면에 표시한 최신 base hash의 명시적 확인만 허용하며 tombstone을 임의로 지우지 않는다.
- [ ] 같은 시험 PASS, 재발행은 foreground24시간 이내로 schedule하고 pause/resume에서 미완료 intent를 reconcile한다. high-water overflow/잘못된 시각은 구조화 error로 처리한다. `feat(identity): persist publication and revocation conflicts` commit.

### Task 3: B3 — exporter로 바인딩한 일반 계정 인증

**Files:** Create `core/chat_core/src/account/{auth,connection}.rs`, `core/chat_core/tests/account_auth.rs`; Modify `core/chat_core/src/transport/{endpoint,manager}.rs`.

**Interfaces:** `AccountHandle::connect_account(peer:PeerId)->Result<(),CoreError>`, `AuthGrant { peer,installation,record_hash,generation,expires_at }`는 비공개로 보관한다. `authenticate(conn:iroh::endpoint::Connection,expected:PeerId,local:&LocalIdentity,record:&SignedRecord,clock:&dyn Clock)->Result<AuthGrant,CoreError>`. LocalIdentity는 A의 keys+자기 cert+수용한 record다. `apply_record_change(&SignedRecord)`로 grant 제한/재인증을 수행한다. NetworkInfo의 EndpointId 필드와 AccountView의 PeerId 필드를 혼동하지 않는다.

- [ ] loopback에서 `valid_cert_actual_endpoint_installation_proof_required`, `replay_other_connection_role_nonce_rejected`, `cached_unexpired_allows_new_proof_but_not_mutation`, `expiry_restricts_then_new_proof`, `removal_generation_conflict_invalidates_existing_grants`, `own_removal_stops_account_activity`를 추가한다. `cargo test -p chat_core --features test-hooks --test account_auth` → FAIL.
- [ ] Endpoint ALPN dispatch를 추가하며 M2의 prepare_session/extra-stream guard를 account stream에 재사용하지 않는다. PeerId→account packet→EndpointId→기존 relay-only lookup으로 연결한다. Hello로 위 표의 context를 구성하고 iroh1.3 `export_keying_material(&mut [u8;32],b"ChatProject/account-exporter/v1",&context)`, 서명 입력 `b"ChatProject/account-auth/v1" || context || exporter`를 양쪽에서 검증한다. cert/record pair/remoteEndpoint/tombstone은 모두 필수다. 증명 전 account 작업/0-RTT 권한을 거부한다.
- [ ] record expiry 시 공개 record 교환/재인증만 허용한다. 같은 generation의 새 record도 새 proof가 필요하며 제거 시 close, generation/conflict 시 모든 grant를 무효화한다. cache 사용 상태와 account-auth-ready를 독립 event로 제공한다. M2connect_ms의 확정 지점을 바꾸지 않는다.
- [ ] loopback 시험과 기존 transport 시험 PASS. `feat(identity): authenticate account sessions with channel proofs` commit.

### Task 4: B4 — identity 초대·연락처·안전번호

**Files:** Create `core/chat_core/src/account/{invite,contacts,safety}.rs`, `core/chat_core/tests/account_invite.rs`; Modify `core/chat_core/src/account/{manager,types}.rs`, `core/chat_core/src/account/storage/schema.rs`.

**Interfaces:** `AccountHandle::create_invite(name:Option<String>)->Result<String,CoreError>`, `parse_invite(input:&str,now:u64)->Result<InvitePreview,CoreError>`, `accept_invite(input:String,alias:String)->Result<ContactView,CoreError>`, `confirm_safety(peer:PeerId,digest:[u8;32])->Result<(),CoreError>`. InvitePreview는 verified cert/hint/표시 이름/expiry/public peer, ContactView는 PeerId/alias/verified/new-device/unverified-device-state다. `safety_digest(a:PeerId,b:PeerId)->[u8;32]`, `safety_display(a:PeerId,b:PeerId)->SafetyDisplay { digest:[u8;32],groups:String,qr:String }`를 제공한다. QR와 표시 문자열은 이 한 digest에서 만든다.

- [ ] `name_64_bytes_accept_65_reject`, `unicode_controls_bidi_not_trusted_name`, `unknown_scheme_no_network_or_storage`, `expiry_future_and_all_fields_signed`, `duplicate_keeps_alias_and_verified`, `symmetric_full_digest_and_same_peer_recovery`를 작성하고 `cargo test -p chat_core --test account_invite` → FAIL을 확인한다.
- [ ] 위 표의 codec으로 scheme/4096B/relayHTTPS256B·noIP hint·시간을 검사하고 parser의 network/store 호출을 0으로 유지한다. accept는 재검증 후 local contact만 저장하며 offline 상태는 기기 미확인이다. 다른 PeerId의 검증 상태를 이어받거나 가짜 ConversationId를 반환하지 않는다. alias도 64UTF8B/제어문자 거부를 적용하고 bidi 표시 격리는 C2에서 처리한다. 오래된 hint로 auth에 실패하면 B3의 최신 record 재조회 이외의 경로로 우회하지 않는다.
- [ ] safety는 설계의 `SHA256("ChatProject/safety/v1"||min(rawIDs)||max(rawIDs))` 전체 32B, 64hex/8묶음이다. confirm 시 현재 digest 일치를 확인하고 local verified를 저장한다. 서명된 초대나 추가만으로 verified로 처리하지 않는다.
- [ ] 같은 시험 PASS, 저장 후 restart해도 alias/verification이 유지되는지 확인한다. `feat(contacts): add identity invites and safety verification` commit.

### Task 5: B5 — QR 방향·Noise 채널·로컬 승인 gate

**Files:** Create `core/chat_core/src/account/pair/{mod,wire,channel,session}.rs`, `core/chat_core/tests/account_pair_channel.rs`; Modify `core/chat_core/src/transport/{endpoint,manager}.rs`, `core/chat_core/Cargo.toml`, `Cargo.lock`, `core/chat_core/src/account/mod.rs`(통합 담당).

**Interfaces:** `PairDirection::{NewDisplays,ExistingDisplays}`, `PairDisplay { flow:FlowId,qr:String,expires_at:u64 }`, `PairView { flow,direction,phase,sas:Option<String>,expires_at,disclosure_possible }`. `AccountHandle::pair_begin(direction:PairDirection,resume:Option<TxId>)->Result<PairDisplay,CoreError>`, `pair_scan(qr:String)->Result<(),CoreError>`, `pair_confirm_code(flow:FlowId,matches:bool)->Result<(),CoreError>`, `pair_approve(flow:FlowId)->Result<(),CoreError>`, `pair_cancel(flow:FlowId)->Result<(),CoreError>`. pair_approve는 E의 로컬 UI에만 대응하며 wire 메시지에서 호출하지 않는다.

- [ ] 양방향의 `same_sas_both_local_confirmations`, `zero_gated_bytes_before_existing_local_approval`, `wrong_endpoint_role_qr_candidate_replay_denied`, `three_failures_and_five_min_expiry`, `remote_confirm_is_not_existing_local_approval`를 작성하고 `cargo test -p chat_core --features test-hooks --test account_pair_channel` → FAIL을 확인한다.
- [ ] candidate 저장/endpoint bind 이후에만 QR을 표시한다. QR 소유자는 responder, scan 측은 initiator다. bounded preface에서 양쪽 키/기존 PeerId를 받아 위 표의 prologue를 고정하고 snow PSK slot0을 설정한다. 공개 preface는 미인증 상태로 취급하며 QUIC actual endpoint/QR pin/handshake proof 일치 전에는 권한을 부여하지 않는다. E cert/record 활성 검증과 양쪽 InstallationKey fresh proof를 수행한다.
- [ ] `snow::Builder::new("Noise_NNpsk0_25519_ChaChaPoly_SHA256".parse()?).psk(0,&psk)?.prologue(&bytes)`로 양쪽 role을 만들고 handshake hash를 얻는다. SAS는 HKDF-SHA256 salt=hash/IKM=PSK/info=`pair-sas/v1`/L5이며 uppercase BASE32_NOPAD를 4-4로 나눈다. 일반 snapshot에는 PSK/QR을 넣지 않고 display 응답으로만 일시 반환한다.
- [ ] candidate 하나, failure3회/TTL5분/승인 후60초를 Clock으로 처리하고 cancel/reject/AEAD 실패 시 close/zeroize한다. E/N 양쪽의 local 확인 전에는 private seed/연락처/N 신규 cert에 접근하는 sender 경로 자체를 열지 않는다. E 공개 cert/proof는 gate 전에 허용한다. 재시작 시 PSK/flow를 복원하지 않는다.
- [ ] 같은 시험 PASS(실제 snow transport byte-count 관측), M2wrongALPN/magic 회귀 PASS. `feat(pairing): add Noise pairing and explicit approval gates` commit.

### Task 6: B6 — durable 키 전달·receipt·재개/폐기

**Files:** Create `core/chat_core/src/account/pair/{transaction,transfer,resume}.rs`, `core/chat_core/tests/account_pair_recovery.rs`; Modify `core/chat_core/src/account/pair/{session,wire}.rs`, `core/chat_core/src/account/{manager,publication}.rs`, `core/chat_core/src/account/storage/schema.rs`.

**Interfaces:** `PairTransaction`은 tx/E·N exact keys/cert/payload hash/base hash/generation/approval/disclosure_possible/verified receipt/publish intent를 보관한다. `AccountHandle::discard_pending_import()->Result<(),CoreError>`는 한 번도 active가 된 적 없는 N에만 허용한다. B5의 resume 인수는 저장된 tx와 완전히 일치할 때만 유효하다. N phase `Candidate→ReceiptReady→PublishPending→Active`, E phase `AwaitingApproval→DisclosurePossible→ReceiptVerified→PublishPending→Observed`.

- [ ] Ecommit 전후/firstbyte 전후/Ncommit 전후/ack 전후/Ereceiptcommit 전후/publish 전후의 crash를 표로 시험한다. `failed_disclosure_commit_sends_zero`, `ack_only_after_durable_import`, `resume_same_pins_fresh_approval_only`, `changed_payload_or_candidate_rejected`, `unpublished_resume_has_transaction_rights_only`, `discard_pending_then_phrase_recovery`, `already_published_no_duplicate_add`. `cargo test -p chat_core --features test-hooks --test account_pair_recovery` → FAIL.
- [ ] gated payload의 첫 byte 전에 E transaction을 durable commit한다. N은 PeerId 재계산/자기 key 일치/cert/크기/hash를 검증하고 pending import+receipt를 atomic commit한 뒤 ack한다. E는 current channel에 바인딩한 receipt를 저장한 뒤 B2add intent로 진행한다. N은 최신 수용 record에서 자기 pair를 확인한 뒤에만 active가 된다. RecoverySeed/DeviceSeed를 보내지 않는다.
- [ ] resume는 반드시 새 B5flow와 새 local approval을 사용한다. 정확한 tx/keys/payload를 고정한 채 receipt/status/같은 payload 재전송만 허용하고 권한/base/남은 기기 수를 재확인한다. 충돌/기존 E 상실 시 active나 성공으로 진행하지 않는다. pending 폐기는 OS blob/key/candidate/import를 모두 삭제했는지 확인하고 공개 목록의 revoke는 별도임을 표시한다. E disclosure 이력은 지우지 않는다.
- [ ] 같은 시험 PASS, `cargo test --locked -p chat_core --features test-hooks` PASS. posttransfer 취소/timeout의 “키가 전달됐을 가능성” warning을 view에 남긴다. `feat(pairing): persist transfer receipts and safe resume` commit.


공식 API 근거: [pkarr8.1 Client](https://docs.rs/pkarr/8.1.0/pkarr/struct.Client.html)는 publish 직렬화를 앱 책임으로 명시한다. [ResolveError](https://docs.rs/pkarr/8.1.0/pkarr/errors/enum.ResolveError.html)와 공식 crate source에서 NetworkOnly/NotFound 분기를 확인했다. 앱의 durable high-water와 전역 부재를 보장하지 않는다는 설명은 이 라이브러리의 응답보다 강한 합의를 주장하지 않기 위한 정책이다.

검증값: B1 `assert!(packet.as_bytes().len()<=1000)`/기기6대 `DeviceLimit`; B2 seq101 수용 뒤 seq100 응답에도 `assert_eq!(observed_seq,101)`, 같은seq 다른hash는 `RecordConflict`; B3 다른 연결의 proof `AuthFailed`, 만료 즉시 `AccountRestricted`; B4 `assert_eq!(name.as_bytes().len(),64)` 성공/65 실패 및 `assert_eq!(network_calls,0)`; B5 승인 전 `assert_eq!(gated_payload_bytes,0)`, SAS문자8개 일치/실패3회 종료; B6 Ncommit 실패 시 `assert_eq!(ack_bytes,0)`, restart 재개 뒤 `assert_eq!(device_count,old_count+1)`. 각 값은 해당 task의 시험에 직접 assertion으로 넣는다.
