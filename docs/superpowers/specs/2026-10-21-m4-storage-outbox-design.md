# M4 저장·보내기 큐 상세 설계

- 작성일: 2026-10-21 (Asia/Seoul).
- 상태: **APPROVED — 2026-10-21 사용자가 전체 설계안에 “진행해줘”라고 승인했다.** 다음 단계는 구현 계획 작성·검토다. 이 문서만으로 기기 실행·서명 배포·원격 통합을 승인하지 않는다.
- 기준: 검토된 M3 소스 `9d1f3945b4b2f475cf757688c14e968e87319d49`, 결과 문서 HEAD `e47d5c4c175e610ef5244d63f55c844d8ec99ca0`.
- 상위 계약: [로드맵 M4](../plans/2026-10-04-p2p-chat-roadmap.md), [전체 설계 rev.3 §7·11·12](2026-10-04-p2p-chat-design.md), [M3 §4·5](2026-10-18-m3-account-devices-design.md), [M1a M4 이월](../plans/2026-10-05-m1a-carry-forward.md), [자동 시험 설계](2026-10-05-test-automation-design.md).
- 사용자 결정: M5보다 **M4를 먼저 설계**한다. 상대가 오래 오프라인이어도 보내기 대기 메시지는 **기간 제한 없이 보관**한다. `제안`은 작성 당시 설계자의 권고를 표시하며, 전체 설계 승인에 포함됐다.

## 1. 목적과 완료 후 가능한 일

연락처와의 1:1 텍스트 대화를 이 기기의 암호화 DB에 보관한다. 사용자가 보내기를 누르면 네트워크를 기다리지 않고 메시지와 전송 작업을 함께 저장한다. 오프라인·앱 종료·재시작을 거쳐도 같은 메시지를 이어서 보내며, 같은 메시지 재전송은 수신 대화에 한 번만 나타난다. 대화 목록, 대화방, 전송 상태와 실패/취소 안내를 한국어로 제공한다.

M4는 개발·검증 단계의 메시지 기능이다. 인증된 기기 사이의 iroh/QUIC 암호화 전송과 SQLCipher 저장을 사용하며, **MLS 메시지 암호화·키 갱신 보장은 M5**다. M4 전용 메시지를 M5 MLS 암호문이나 완성된 Invite v2로 표시하지 않는다.

범위는 텍스트, 지속 저장, 전송 대기·재시도·중복 제거, 전달/읽음 표시, 페이지 조회와 계정 저장 이관이다. 그룹·첨부·편집·반응·상대 메시지 삭제·입력 중 표시·검색·푸시·메일박스·서버 큐·대화 백업/복원·기기 간 전체 대화 동기화는 만들지 않는다. M3 복구 문구는 계정 ID를 복구하며 이 기기의 과거 대화나 DB 암호키를 재생성하지 않는다.

M2 성능/iOS와 M3 Release·Apple·공개 네트워크 등 기존 열린 수용 항목은 유지한다. M4 설계 승인으로 이전 마일스톤을 완료 처리하지 않는다.

## 2. 사용자 보관 결정과 용량

1. **대기 메시지에 시간 TTL이나 재시도 횟수 만료를 두지 않는다.** 상대가 며칠 접속하지 않아도 버리거나 시간 경과만으로 영구 실패시키지 않는다.
2. 상대의 유효한 기기 하나가 저장 완료를 확인하면 사용자 관점에서 `전달됨`이다. 전송 작업을 끝내도 대화의 메시지는 로컬 기록에 남는다.
3. 사용자 취소는 앞으로의 자동 전송을 멈춘다. 이미 상대에게 도착한 사본은 회수하지 않는다. 취소 후에도 본문과 취소 결과는 로컬에 남는다.
4. 일반 대화 기록에도 자동 기간 만료를 두지 않는다. 보관 기간 무제한은 디스크 용량 무제한이나 OS 백그라운드 상시 실행을 뜻하지 않는다.
5. **용량 제안:** 이 설치의 사용자 텍스트 outbox는 대기/보류 작업 합계 10,000건 또는 저장된 전송 payload 합계 128MiB 중 먼저 도달한 한도다. 이 수치는 설계자의 초기 자원 한도이며 사용자 결정으로 단정하지 않는다. 수신/이미 저장된 대화의 시간 만료와는 별개다.
6. 한도 초과·디스크 부족·DB 쓰기 실패는 새 enqueue를 원자적으로 거부한다. 입력창 본문을 유지하고 저장 공간 오류를 보인다. 이미 받아 둔 대기 메시지를 밀어내거나 성공을 먼저 표시하지 않는다. 수신 저장 실패 시 저장 확인을 보내지 않아 송신자가 보관·재시도하도록 한다.

오프라인 재시도와 별도로 인증 실패/해제/충돌/저장 잠김은 **보류**한다. 본문을 보존하고 이유를 보이며, 요구되는 새 record 확인·재인증·키 잠금 해제가 끝나야 전송을 재개한다. 재시도를 위해 M3의 권한 검사를 우회하지 않는다.

## 3. M3와의 관계 및 저장 구조 선택

현재 M3는 OS 보호 키로 암호화한 `account.bin` 스냅샷을 사용한다. 계정 actor가 인증서·record floor·충돌·연락처·pair transaction을 소유한다. 계정 인증 세션의 ready/cache 표시는 메시지 저장이나 전달 확인이 아니다. 진단 `ProbeTextReceived`와 메시지 기능은 계속 구분한다.

저장 구조는 다음을 비교했다.

| 방식 | 판단 |
|---|---|
| account.bin을 계속 쓰고 메시지 DB만 추가 | 계정과 DB의 영구적인 이중 권한 저장소가 되며 M3의 SQLCipher 이관 계약과 어긋난다. 채택하지 않는다. |
| M3 전체 상태를 즉시 세분화된 SQL 표로 재작성 | M4 이전에 검토한 계정 상태 기계까지 크게 바뀐다. 이번 단계의 필수 작업으로 만들지 않는다. |
| **M3의 버전 있는 계정 상태를 SQLCipher의 한 행으로 이관하고, 대화·메시지 표를 추가** | **설계자 권고.** 기존 상태 codec/한도/계정 규칙을 보존하면서 권한 있는 영속 상태를 DB 하나로 합친다. |

`account_state`의 직렬화 상태가 계정의 영속 원본이다. 메모리 AccountView 및 연락처 표시 자료는 여기서 만든 파생 결과다. 같은 연락처·high-water·pair 상태를 독립 SQL 표와 기존 파일 양쪽에서 갱신하지 않는다. 조회 성능상 필요한 식별 참조는 원본 상태와 같은 transaction에서 검증하며 별도 계정 원본으로 삼지 않는다.

기존 `Store` API의 단일 writer·suspend·fail-closed 동작을 SQL 저장으로 옮긴다. M3의 public AccountHandle API/키 파생/wire를 임의로 바꾸지 않는다. 최소 내부 adapter/async 호출 변경은 필요하지만 범용 저장 플러그인·journal framework를 만들지 않는다.

## 4. DB 보호와 플랫폼 계약

SQLCipher를 bundled rusqlite로 사용한다. DB 키는 복구 문구·IdentityKey·TransportKey에서 파생하지 않는 별도 random 32B다. 키와 iOS 헤더용 16B salt는 OS 보호 자원으로 관리하고 FFI/Dart/로그/일반 설정 문자열로 내보내지 않는다. 플랫폼별로 재사용 가능한 기존 보호 경로를 우선 사용하되, DB용 자원 식별자는 legacy 계정 wrapping 자원과 분리한다. 보호 material의 bounded encoding과 native adapter는 구현 계획에서 고정한다.

- Windows: 기존 LocalAppData 정책과 DPAPI. 기존 계정 폴더를 임의로 이동하지 않는다. canonical 경로/한글/긴 경로에서 실제 SQLCipher open·재시작을 검증한다.
- Android: 기존 no-backup 폴더·Keystore, DB/WAL/SHM/키 metadata 전부 백업 제외. 소프트웨어·TEE·StrongBox 표시를 실제 provider 결과와 일치시킨다.
- iOS: 기존 App Group 및 공유 Keychain 계약을 유지한다. SQLCipher plaintext header32와 외부 salt를 매 open에 올바르게 제공한다. 헤더 때문에 DB 종류·일부 형식 정보가 보이는 한계를 기록한다. NSE는 M4에서 DB writer가 되지 않는다.

SQLCipher 키 적용·설정 후 실제 읽기와 metadata binding 검증이 성공해야 저장소를 연 것으로 본다. `PRAGMA cipher_version`과 실제 엔진 정보를 기록해 일반 SQLite가 암호화 PRAGMA를 무시한 경우를 통과시키지 않는다. SQL 값은 바인딩하고 secret SQL·본문·키를 추적 로그에 넣지 않는다. WAL·임시 자료에도 평문 본문이 남지 않도록 `temp_store=MEMORY`, 암호화 WAL, 진단 로그 제외를 확인한다. 키 분실·다른 사용자·변조·미지원 버전은 오류로 닫고 새 DB/계정을 조용히 만들지 않는다.

WAL, foreign keys, 충분한 동기화 설정을 사용하고 commit 뒤에만 durable 성공을 알린다. 초기 권고는 `synchronous=FULL`이며 성능을 위해 durability를 조용히 낮추지 않는다. 정확한 rusqlite/libsqlite3-sys/SQLCipher/crypto provider 버전·features는 구현 계획에서 lockfile과 각 플랫폼 빌드로 고정한다. SQLite WAL-reset 수정이 포함된 실제 기반 엔진/백포트인지 확인하며 crate 버전만으로 추정하지 않는다.

## 5. 이관·최초 생성·충돌하는 저장소

M3에 승인된 “성공적인 SQLCipher transaction 뒤에만 이전 상태 폐기”를 다음과 같이 구체화한다.

1. 현재 canonical root의 writer 소유권을 먼저 잡고 네트워크/계정 변경·메시지 명령을 막는다. legacy 비밀은 기존 codec으로 검증한 뒤에만 읽는다.
2. 새 DB ID, OS 보호 DB 키/salt, 준비 metadata를 중단 시 식별 가능한 순서로 저장한다. 공개 metadata에는 키 원문을 담지 않는다. 키 자원을 만들기 전에 그 자원의 소유 식별자를 남긴다.
3. 하나의 SQL transaction에 전체 계정 상태, 스키마 버전, DB ID/PeerId/installation binding, legacy 원본 hash 및 이관 완료 표식을 저장한다. Candidate·Pinned·PendingImport도 그대로 보존하며 Active로 승격하지 않는다.
4. DB를 닫고 보호 키로 다시 열어 전체 상태를 decode·비교한다. M3의 revision, 비밀키/cert, 기기 키/EndpointId, 연락처·안전번호, record floor/conflict/tombstone, pending publish, pair pin/receipt/disclosure/retained history가 보존돼야 한다. 앱 내 비밀 비교 결과만 기록하고 값을 출력하지 않는다.
5. 원자 교체·플랫폼 durability 절차로 root의 `storage.mode`를 SQL authority로 전환한다. DB 내부 binding과 mode가 일치해야 새 명령을 허용한다.
6. 전환이 완료된 뒤 legacy `account.bin`/임시 사본/기존 wrapping blob와 해당 legacy 보호 자원을 순서대로 폐기한다. **기존 M3 앱이 빈 계정으로 오인하지 못하도록 M3가 인식하는 `store.id` guard는 남긴다.** guard는 계정 원본이나 복호 키가 아니다. 새 DB의 키 자원을 삭제하지 않는다.

새 M4 설치도 SQL 저장만 생성하고, 구버전이 새 계정을 덮어 만들지 못하도록 같은 downgrade guard를 둔다. 빈 계정이 아닌 상태로 자동 복구/초기화하지 않는다. M3 이하로의 정상 다운그레이드는 지원하지 않는다.

DB는 계정 생성 전에도 `Unconfigured` binding으로 존재할 수 있다. 정상적인 candidate 준비/생성/never-active pending 폐기에서는 account_state와 `Unconfigured → Candidate(installation) → Active(PeerId,installation)` binding을 같은 transaction으로 갱신한다. DB ID/키/mode와 downgrade guard는 그대로 유지한다. 미활성 폐기로 SQL authority 전체를 지우거나 실제 대화가 있는 Active 저장소를 비우지 않는다. Active 계정 갈아끼우기·대화 DB 초기화 API를 추가하지 않는다.

중단 복구는 다음 규칙을 따른다.

| 관찰한 상태 | 다음 실행 |
|---|---|
| legacy만 있음, 새 DB 흔적 없음 | 검증 후 이관 시작. |
| 준비 metadata/DB가 있으나 SQL authority 전환 전 | 완전한 legacy와 DB 완료 표식을 조사한다. commit이 확인되면 검증 후 전환을 마친다. commit 전이며 원본 hash가 같은 경우에만 소유한 준비 작업을 재개한다. |
| SQL mode 또는 완료된 DB가 있음 | SQL 저장이 유일한 후보. legacy로 자동 fallback하지 않는다. |
| SQL 전환 후 legacy 파일/키 정리가 중단됨 | SQL만 사용하고 정리를 멱등 재개. 새 계정 상태를 legacy에 다시 쓰지 않는다. |
| 키 없음·DB 손상·서로 다른 binding/legacy 변경·진행 상태 불명 | 기존 파일을 보존하고 오류/명시적 재시도 안내. 자동 삭제·새 계정 생성·stale fallback 금지. |

로컬 관리자가 전체 저장소·OS 보호 자원·mode를 함께 과거로 복원하는 공격을 완전히 막는다고 주장하지 않는다. OS wrapping 자원 제거가 원격 키 사본 소거를 뜻하지 않는다.

## 6. 직렬화와 DB transaction

한 설치에서 **DB connection 하나를 소유한 직렬 저장 worker**를 둔다. SQL·fsync는 Flutter UI와 tokio 네트워크 실행기를 막지 않는 전용 실행 경로에서 수행한다. 기존 계정 actor가 권한 판단·메시지 작업을 직렬화하고 worker의 명시적인 완료를 기다린다. DB connection/transaction을 Dart로 넘기거나 async network await 동안 열어 둔 transaction으로 만들지 않는다.

같은 root의 프로세스 간 writer 규칙은 M3를 이어받는다. Windows/Android는 중복 writer를 거부한다. iOS는 짧은 SQL 작업 동안만 소유권을 잡고, background 진입 때 새 작업·네트워크를 먼저 막아 작업을 종료/rollback한 뒤 connection 및 모든 transaction/file lock 해제를 확인한다. timeout을 “해제 완료”로 보고하지 않는다. 실패 시 계속 차단하며 native 종료·재개 증거를 별도 게이트로 남긴다. 재개는 키·mode·DB binding·소유권을 다시 확인한다.

최소 스키마의 역할은 다음과 같다. 정확한 DDL/codec 배열은 구현 계획에서 고정한다.

| 표 | 원본 역할/제약 |
|---|---|
| `storage_meta` | 스키마·DB/설치 binding·이관 완료·다음 revision. |
| `account_state` | 기존 versioned M3 상태 한 행. 기존 decoder/한도 유지. |
| `conversation` | 내 계정과 상대 PeerId에 대한 1:1 대화. 같은 쌍 중복 생성 금지, Lamport 최댓값·최근 메시지 정보. |
| `message` | msg_id, 대화, 인증된 송신 PeerId/installation, Lamport·표시 시각, 본문·본문 hash, 수신/송신 방향, 상태/receipt 사실. 논리 메시지 키에 UNIQUE. |
| `outbox` | 메시지의 고정 wire payload/codec/hash, 목적 PeerId, 시도 횟수·다음 시각·보류 이유. 같은 논리 메시지 작업 중복 생성 금지. |
| `processed_ct` | codec·인증된 송신자 범위·payload hash. M4 frame과 M5 암호문 namespace를 분리. 이름만으로 M4 payload를 MLS 암호문이라고 주장하지 않음. |
| `receipt_job` | 수신 저장/읽음 확인의 지속 전송. 메시지와 함께 원자 저장하고 같은 확인을 병합. 사용자 본문 작업을 밀어내지 않음. |

원자 경계는 (a) 송신 메시지+Lamport+고정 payload+outbox, (b) 수신 메시지+중복 표식+대화 갱신+receipt job, (c) 검증한 receipt+단조로운 메시지 상태+outbox 완료, (d) 읽음 사실+read receipt job, (e) 사용자 취소+향후 dispatch 중단이다. 실패 시 각 경계 전체가 rollback된다. DB commit 후 이벤트 발행 전에 죽어도 다음 snapshot/page 조회에서 복구된다.

같은 두 PeerId의 대화 식별자는 양쪽에서 동일해야 하며 installation·접속 주소·표시 이름에 따라 바뀌지 않는다. 구현 계획에서 고정 domain과 정렬한 두 PeerId로 결정적인 식별자 및 golden vector를 정한다. 본인과의 대화는 M4에서 지원하지 않는다. enqueue idempotency는 용량 검사보다 먼저 확인해, 이미 성공한 동일 요청의 응답 재시도가 대기열 한도 때문에 실패하거나 두 번째 메시지를 만들지 않게 한다.

## 7. 텍스트 전송과 인증 경계

M4는 **M3가 인증한 account connection**에서만 별도 versioned message frame을 처리한다. 기존 auth/record/revoke control stream과 discriminator를 명시적으로 구분한다. unauthenticated M2 probe·pair 채널·초대 parser를 메시지 경로로 사용하지 않는다. ready라는 UI bool만 믿거나 raw EndpointId로 계정 인증을 대체하지 않는다.

초기 제안은 `m4-direct-text/v1` codec이다. fixed canonical CBOR 배열, frame 상한60KiB, 본문 UTF-8 16KiB 이하, 빈 본문 거부, 고정 길이 random msg_id16·installation32·PeerId32·hash32, 유효한 정수/버전/정확한 필드 수를 요구한다. 표시 이름이나 본문을 잘라 성공 처리하지 않는다. 본문은 일반 텍스트로 그리며 코드/HTML로 실행하지 않는다. 첫 메시지 전에 상대 PeerId가 로컬 연락처인지 확인하고, 모르는 인증 계정이 대화/본문을 조용히 삽입하지 못하게 한다. 수신 허용되지 않은 상대는 내용을 저장하지 않고 구조화된 거부를 돌려준다.

이는 M4 초기판에서 **양쪽이 서로 연락처로 추가한 뒤 대화**하도록 하는 설계자의 범위 제안이다. 한쪽이 초대를 추가한 사실만으로 상대의 연락처 목록에 자동 추가하지 않는다. `상대가 아직 연락처로 허용하지 않음`은 본문을 보존하는 전송 보류이며 시간 만료/자동 폐기가 아니다. 모르는 발신자의 대화 요청함·스팸 승인 화면은 이 제안을 바꾸는 별도 제품 범위이므로 이번 설계 승인 때 확인한다.

송신은 목적 **PeerId**에 묶고, 매 시도에 M3가 허용한 현재 기기를 조회·선택한다. 유효 cache 재접속은 기존 M3 규칙 안에서만 허용한다. expired record/새 generation/제거/known conflict/failed fresh proof이면 전송을 보류한다. 송신자 PeerId/installation은 인증 세션으로 결정하고 payload의 주장을 대조한다. worker를 기다리는 동안 lifecycle/권한 revision이 바뀌면 이전 결과가 새 전송 권한을 만들 수 없다. 계정 상태 적용과 메시지 승인 순서는 같은 actor에서 직렬화하고 실제 first byte/receipt 반영 직전에도 세션과 ticket을 확인한다.

**M4 전달 범위 제안:** 상대의 유효한 기기 하나에 저장되면 전달 완료다. 상대의 모든 기기 및 내 다른 기기에 동일 기록을 자동 배포한다는 약속은 하지 않는다. 새 기기 연결 시 과거 메시지 DB·outbox·DB 키를 M3 pairing payload에 추가하지 않는다. 기기별 MLS 멤버십/다중 기기 전달 확장은 M5, 메일박스 재동기화는 M6 설계에서 정한다.

한 대화의 정렬은 기존 계약대로 `(lamport, sender_installation, msg_id)`다. 송신시 `max+1`, 수신시 최댓값을 원자 갱신한다. 표시용 `sent_at`으로 순서를 바꾸지 않는다. overflow·과도한 Lamport 점프는 거부하며 정확한 허용폭을 구현 계획의 wire/test 상수로 고정한다. 인증된 sender별 prev_hash 사슬을 저장하되, 갭을 이유로 기록을 지우거나 M4에 메일박스 요청 프로토콜을 새로 만들지 않는다.

동일 payload 재전송은 검증 후 이미 저장한 receipt를 재발급한다. 논리 키가 같은데 payload hash/본문이 다르면 충돌 오류이며 덮어쓰지 않는다. 같은 msg_id를 다른 PeerId/installation의 메시지와 혼동하지 않는다. payload hash는 invalid/권한 미확인 입력에 대해 먼저 영속 확정하지 않는다.

## 8. 상태·ack·취소

사용자에게 보이는 상태는 저장 중 → 보내기 대기 → 전송됨 → 전달됨 → 읽음이며, 보류·실패·취소를 별도로 표시한다. 내부 dispatch/receipt 사실은 분리해 늦게 온 결과가 `읽음`을 `전송됨`으로 되돌리지 못하게 한다.

- `저장 중`: 아직 enqueue commit 전. 입력을 잃지 않으며 보내기 성공으로 표시하지 않는다.
- `보내기 대기`: 메시지와 outbox가 commit됨. `send_text`는 이 상태와 안정된 msg_id를 반환한 뒤 네트워크를 진행한다.
- `전송됨` (`sent_direct`): frame 전송 완료를 관찰했지만 상대 저장 확인은 없을 수 있다. outbox는 계속 보존/재시도한다.
- `전달됨`: 현재 인증된 목적 PeerId의 허용 기기가 해당 msg_id/payload hash를 지속 저장했다는 일치하는 receipt를 수신·commit했다. socket write/QUIC ACK는 이 상태가 아니다.
- `읽음`: 상대가 자기 앱의 foreground 대화 화면에 실제 표시한 해당 메시지에 대한 read receipt를 보냈다. 사람이 이해했다는 증거는 아니다. 표시하지 않은 전체 대화를 읽음 처리하지 않는다.
- `보류`: 인증·충돌·키/저장 잠김 등 선행 해소 필요. 연결 실패 횟수로 이 상태를 자동 해제하지 않는다.
- `실패`: 지원하지 않는/잘못된 고정 메시지, 영구적인 수신 거부 등 자동 재시도로 고칠 수 없는 오류. 본문과 실패 이유를 보존한다. 일시적인 수신 DB/디스크 부족은 영구 거부가 아니라 재시도 가능 오류다.
- `취소`: 향후 이 outbox 메시지의 재전송을 중단했다. 이미 in-flight였으면 상대 도착 가능성을 안내하며 후속 진짜 receipt도 보존·표시한다. 사용자가 재전송을 누르면 별도 새 msg_id의 새 메시지로 보낸다.

receipt는 codec/메시지·payload hash·목적/송신 계정 및 installation 문맥에 묶는다. 임의 UI 호출로 delivered/read를 설정할 수 없다. 확인되지 않은 endpoint, 다른 hash, 다른 대화, 이미 폐기된 권한에서 온 receipt로 완료시키지 않는다. Read는 Delivered를 포함하며 순서가 바뀐 receipt도 단조롭게 합친다.

취소와 dispatch는 같은 원본 상태로 직렬화한다. 취소 이후 시작되는 새 시도는 없고, 이전 in-flight 결과는 사실대로만 반영한다. 수신 저장 후 응답 전 crash/응답 유실/송신 완료 저장 전 crash에서도 재전송은 한 메시지로 합쳐져야 한다.

## 9. 무기한 재시도·시간·재시작

일시 실패 간격은 **1초, 2초, 4초 … 최대5분**이며 이후5분 간격이다. 성공 횟수·대기 기간으로 자동 삭제하지 않는다. 시도 카운터 overflow를 막고 최대 간격에서 포화시키며 카운터 상한을 “소진”으로 오해하지 않는다. 네트워크 복귀/새 인증 세션에서 대기 메시지를 조기에 다시 시도할 수 있으나 하나의 메시지를 동시에 여러 작업이 보내지 못하게 한다.

M3의 기존 Clock을 공유·확장한다. 새로운 독립 보안 시계를 만들지 않는다. 실행 중 due/sleep는 monotonic 기준, 표시/영속 복귀용 시각은 wall clock과 마지막 delay를 사용한다. 재시작 때 wall clock이 뒤로/크게 앞으로 이동해도 메시지가 만료되거나 영구 대기하지 않게, 재개 delay를0…5분 안으로 clamp한다. 권한 record의 만료 검증을 이 clamp로 약화하지 않는다. 테스트 clock 전진은 scheduler를 깨우며 실제 분/시간을 기다리지 않는다. production에서 clock 조종을 노출하지 않는다.

프로세스 내 한 scheduler가 한 메시지당 하나의 in-flight 작업을 만든다. 프로세스 사망 후 dispatch 상태는 미확인으로 복귀하며 **동일 wire payload·동일 msg_id**로 다시 시도한다. 재시도마다 새 메시지나 새 Lamport/prev_hash를 만들지 않는다. 대기 전체를 메모리에 올리지 않고 due index에서 bounded batch를 읽는다. 다른 대화를 한 offline peer가 영구 독점하지 않도록 공정하게 처리하며 backpressure를 둔다.

OS가 앱 실행을 중단한 동안 실제 송신을 보장하지 않는다. M4가 Android 상시 서비스·푸시·PC 절전 방지를 만들지 않는다. 앱 재실행/재개 후 저장된 대기열을 이어간다.

## 10. 화면과 코어 API

기존 로드맵 화면9·11·12의 대화 목록·대화방·메시지 상태를 사용한다. 기존 계정·연락처·안전번호·기기 화면을 유지하고 연락처에서 1:1 대화를 연다. 새 시각 브랜드/그룹·파일 메뉴를 이번에 만들지 않는다. exact 레이아웃은 기존 목업과 현재 theme tokens를 따르고 구현 계획의 위젯 검증에서 고정한다.

목록은 마지막 메시지·시각·미확인 개수/전송 상태를 표시한다. 대화는 최신50개를 읽고 과거로 페이지 조회하며 offset 대신 안정된 정렬 cursor를 쓴다. 중간 수신·중복 이벤트·재접속 때문에 페이지가 빠지거나 같은 버블이 늘어나지 않게 Rust의 원본 key로 합친다. UI가 임의로 queued/delivered/read 원본을 저장하지 않는다.

최소 명령은 대화 열기/목록 조회, 메시지 page 조회, `send_text`, `cancel_send`, 보류 해소 후 명시적 retry, 실제 표시된 메시지의 읽음 반영이다. send는 호출자 생성 idempotency token을 받을 수 있게 하여 응답 유실·더블 탭이 중복 enqueue를 만들지 않게 한다. 동일 token/다른 본문은 충돌 오류다. token/대화/본문 경계와 만료 없는 token 보관 범위는 message UNIQUE key로 구현 계획에서 구체화한다.

기존 단일 CoreEvent 스트림에 메시지·상태·대화 변경과 revision을 추가한다. 늦은 구독자는 DB snapshot/page에서 읽고 이후 이벤트와 revision으로 이어간다. event 누락/프로세스 사망을 전체 본문 무제한 replay로 해결하지 않는다. 과거 transcript와 본문·receipt 전체를 디버그 제어 로그로 내보내지 않는다. FRB/UniFFI는 typed public view와 구조화된 오류만 전달하며 secret material은 native/Rust 내부에 남는다.

접근성·키보드·큰 글자·한국어 오류를 검증한다. 기존 입력→화면16ms 목표는 네트워크 대기와 분리해 실제 저장/이벤트/render 시간도 함께 측정한다. 느린 durable commit을 숨기려고 commit 전 queued 성공을 알리거나 수용 기준을 조용히 바꾸지 않는다. 목표 미달은 수치와 원인을 남긴다.

## 11. M5 이후 경계

M4 canonical text/receipt 의미·stable message identity는 M5에서 재사용할 수 있게 고정하되 MLS KeyPackage/Welcome/Commit/StorageProvider는 구현하지 않는다. reserved `mls_kv`는 M5가 실제 필요와 원자 경계를 정해 추가하며 빈 범용 crypto framework를 선행하지 않는다.

M5에서는 MLS 상태 변화·암호문 생성/수신 처리·message/outbox/processed_ct가 같은 SQL transaction에 들어가야 한다. 현재의 고정 재시도 payload 규칙을 유지하며 매 재시도에 재암호화/새 MLS generation을 만들지 않는다. M4 미전달 메시지의 M5 승격은 명시적 버전 이관 대상이며, MLS 사용을 선택한 대화에서 실패했다고 M4 codec으로 자동 downgrade하지 않는다. 정확한 혼합 버전/승격 정책은 M5 설계에서 승인받는다.

M4의 계정·메시지 스키마 버전은 명시적으로 올리고 미래 버전 DB는 구버전 앱에서 쓰지 않는다. M6/M8의 server TTL은 사용자가 정한 로컬 outbox 무기한 보관을 자동 변경하지 않는다.

## 12. 완료를 판단할 증거

| 층 | 필수 검증 |
|---|---|
| 이관/키 보호 | legacy 모든 상태의 실제 이관·재open·동일 계정/Endpoint; 각 경계 crash, marker/DB binding 불일치, key 유실/손상, 준비 자원 정리, 구버전 guard, SQL mode 후 stale fallback 금지. 기존 사용자 저장소가 아닌 격리 fixture에서 수행. |
| SQL 원자성 | enqueue/receive/receipt/cancel 각 transaction 전·중·후 실패, disk full/IO/잘못된 키, duplicate/hash 충돌, page cursor/revision, 이미 저장된 대기열 비퇴거. 일반 SQLite 대체 방지 및 DB/WAL/임시 파일 평문 유출 검사. |
| 상태/인증 | auth-ready 전 payload0B; 다른 sender/endpoint/hash/receipt 거부; record expiry/removal/generation/conflict·suspend 중 차단; 보류 뒤 새 proof; 진단/pair stream 메시지 우회 금지. |
| 재시도/시간 | FakeClock과 clock_advance로1s…5min·며칠/수개월 전진·무기한 보관, saturation/시계 역행/forward jump/앱 재시작; 동일 payload·msg_id; offline peer 공정성·bounded batch·취소/in-flight/late receipt 경쟁. |
| Flutter | 저장 전 성공 표시 없음, 입력 유지/한도 오류, 50개 paging·이벤트 합치기, 전달/읽음의 실제 의미, 취소 경고, 보류 이유/복구, 키보드/200%/semantics와 실제 성능 계측. |
| 여러 native 참가자 | 기존 local relay/record fixture+Windows/Android2/chat_node에서 송신→offline→강제 종료→재실행→원래 대기 재개→수신 저장 ack, 중복1개. 실제 OS 보호/DB를 쓰며 메모리 fake와 구분. |
| 플랫폼/회귀 | Windows 한글/긴 경로·두 번째 writer, Android no-backup/Keystore, iOS DB open/WAL·suspend 후 실제 lock/handle 해제·재개, M3 pairing/pending/crash/auth 회귀, binding 재생성·native Release 스캔·시험 조종 차단. |
| 실물/외부 | 선택한 Windows↔Android Release에서 로컬 저장·재시작·오프라인 대기·복귀 전달과 실패 UX; Apple runner/실물은 별도 실제 결과. 공개 DHT/릴레이·기기 설치·서명·원격 작업은 기존의 명시적 실행 범위를 따른다. |

자동 시험은 로컬 서비스로 재현하며 공개 인터넷 성공을 CI 조건으로 삼지 않는다. native fixture/테스트 조종은 Debug에만 남기고 Release 부재/runtime 차단을 다시 확인한다. M3의 비밀 로그 금지·안전한 자원 소유권·모든 실패 기록 보존을 유지한다. 본 설계 작성은 시험 실행/성공 증거가 아니다.

## 13. 구현자가 정할 세부와 승인 요청 범위

구현자는 이 계약 안에서 파일 배치·정확한 bounded wire 배열·DDL/인덱스·오류 enum 이름·작업 분할·pin/빌드 flags·테스트 fixture를 정할 수 있다. 대기 TTL 도입, 자동 기록 퇴거, 권한 우회, 다중 기기 전체 전달 약속, 복구 문구의 메시지 복원 의미, migration fallback/SQL authority 변경, MLS 선행 도입, durability 하향은 상세 선택으로 처리하지 않는다.

주요 제안은 (1) SQL account_state 한 행 이관, (2) 시간 무제한과 별개의 outbox10,000건/128MiB 용량 한도, (3) 인증된 상대 기기 하나의 저장 ack를 전달 완료로 보는 M4 범위, (4) 텍스트16KiB와60KiB frame, (5) foreground 표시 기반 읽음과 취소 후 도착 가능성 안내, (6) 초기 M4의 상호 연락처 추가 조건이다. 이 항목들은 2026-10-21 전체 설계 승인에 포함됐다. 최초 사용자 요구와 설계자가 제안해 승인받은 세부의 출처는 구분해 보존한다.

전체 설계 승인 후 별도의 구현 계획을 작성·검토한다. 설계 승인만으로 제품 코드 착수·의존성 변경·공개 통신·서명 배포·원격 통합이 실행되지는 않는다.

## 14. 확인한 외부 근거

- [SQLCipher API](https://www.zetetic.net/sqlcipher/sqlcipher-api/): 키 적용, plaintext header와 매 open 외부 salt, 실제 cipher version/provider 확인.
- [SQLCipher design](https://www.zetetic.net/sqlcipher/design/): 암호화 페이지/WAL 및 보호 범위.
- [SQLite WAL](https://sqlite.org/wal.html): side files·checkpoint·read-only 제약, WAL-reset 수정3.51.3 또는 명시된 backport. 실제 bundled 엔진 적용 여부는 빌드 증거가 필요하다.
- [SQLite transaction](https://sqlite.org/lang_transaction.html): 단일 writer와 transaction 경계.
- [rusqlite Connection](https://docs.rs/rusqlite/latest/rusqlite/struct.Connection.html), [crate README](https://github.com/rusqlite/rusqlite/blob/master/README.md): connection/transaction 및 SQLCipher 빌드 옵션. 조회 당시 docs.rs는0.40.2이며 latest URL을 구현 pin으로 삼지 않는다.

근거 확인일2026-10-21. 버전 pin·플랫폼 호환성은 구현 계획에서 다시 확인한다.
