# M3 Account and Devices Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 승인된 M3 계정·연락처·기기 연결 기능을 Windows·Android·iOS에 구현하고, 보안 상태와 실제 플랫폼 증거를 구분해 검증한다.

**Architecture:** Rust가 키·암호화 snapshot·record·인증·pair transaction을 소유한다. native adapter는 OS 보호만 제공하고 Flutter는 일시적 비밀 표시와 상태 UI를 맡는다. 아래 A→B→C 세 계획을 각각 독립적인 시험·검토 단위로 실행한다.

**Tech Stack:** Rust 1.92.0, Flutter 3.44.6/Dart 3.12.2, FRB 2.11.1, UniFFI 0.32.2, iroh 1.3.0. 신규 직접 의존성은 A의 고정 표를 따른다.

**Spec:** [승인된 M3 상세 설계](../specs/2026-10-18-m3-account-devices-design.md), [상위 설계 rev.3](../specs/2026-10-04-p2p-chat-design.md), [자동화 설계](../specs/2026-10-05-test-automation-design.md).

## Global Constraints

- 상태: **APPROVED — 구현 진행 중**. 2026-10-18 사용자는 전체 상세 설계 승인 후 구현 계획에도 “진행”으로 동의했다. 작업별 에이전트 방식은 이미 선택되었다. 이 문서 작성은 제품 구현·빌드·기기 실행이 아니다.
- RecoverySeed **32B entropy → BIP-39 24 English words**. PBKDF2 64B seed/passphrase 없음. HKDF-SHA256 labels는 설계 §3 그대로다.
- Identity signing seed는 모든 trusted device에 공유한다. 기기마다 DeviceSeed **32B**는 독립 생성한다. 삭제/같은 PeerId 복구가 이전 IdentityKey 사본을 취소한다고 설명하지 않는다.
- AES-256-GCM, snapshot마다 random **96비트 nonce**, AAD format version/installation. 파일 **4MiB**, 연락처 **1000**, pending **32**, tombstone **256**, 활성 기기 **5** 상한을 적용한다.
- account signed DNS/wrapper 전체 **1000B 이하** 및 BEP44 value 상한을 각각 검사한다. 공개 IP·이름·cert·relay URL을 account packet에 넣지 않는다. Endpoint 주소 게시의 기존 relay-only 정책 유지.
- record **7일**, 전경 재게시 **24시간 이내**, 미래 발행 허용 **5분**. cache는 unexpired + 알려진 충돌/제거 없음일 때 새 proof로 접속만 허용한다. 목록 변경 권한으로 쓰지 않는다.
- IdentityInvite v1은 별도 scheme, decoded **4096B**, 이름 UTF-8 **64B**, HTTPS relay **256B**, 기본 **24시간**/최대 **7일**. M5 Invite v2/KeyPackage/ConversationId를 만들지 않는다.
- Noise_NNpsk0_25519_ChaChaPoly_SHA256, QR PSK **32B**, session/tx **16B**, SAS **40bit/8자**, QR 세션 **5분**, 실패 **3회**, 승인 뒤 전달 확인 **60초**. 양방향 모두 E의 로컬 승인과 N의 코드 확인 필요.
- E의 durable disclosure_possible 이전 gated 비밀 **0B**, N의 durable receipt 이전 ack **0B**. 재개는 새 QR/PSK/session/Noise/SAS/E 승인으로 같은 pinned tx만 허용한다.
- 일반 접속은 `chatproject/account/1`, pair는 `chatproject/pair/1`, M2는 기존 `chatproject/m2/1`. transport-connected와 account-auth-ready를 구분한다.
- 제품 임시 표시 이름 **ChatProject**, application/bundle ID 불변. 한국어 오류·키보드·스크린리더·200% 글자. 문구 자동 clipboard/log/일반 event 금지.
- iOS는 foreground 유한 저장 구간만 lock, background 전 새 쓰기 차단/진행 저장 정리/lock 해제. NSE에 account/pair/DHT를 넣지 않는다. Windows·Android는 secret read 전 canonical folder 독점 소유권을 확보한다.
- M4 DB/outbox, M5 MLS, M6 이후 전송 기능, 활성 계정 삭제/덮어쓰기/범용 Core reset은 범위 밖이다. M2 500ms 미달·유예 LAN·통합 미실행은 별도 추적한다.

## Review Focus

1. 저장 도중 강제 종료/OS secret 유실: 기존 정상 snapshot 보존, 새 계정 자동 생성 금지 — A2/A3.
2. 늦게 도착한 이벤트·중복 승인·새 화면 재진입: 이전 세션이 현재 권한/승인을 바꾸지 않음 — A4/B5/C1.
3. 오래 오프라인/시계 역행/동시 기기 변경: high-water를 내리지 않고 충돌/게시 대기를 표시 — B2/B3.
4. 다른 QUIC 연결·다른 역할·QR 탈취 후보의 proof: 인증/비밀 전달을 거부하고 자동 승인하지 않음 — B3/B5.
5. QR/복구 문구가 시험 도구·화면 전환에 노출: 기록 redaction과 민감 화면 가림, Release hook 차단 — C2/C4.

## 계획 구성과 작업 의존성

| 순서 | 계획 | 독립 산출물 / 작업 |
|---|---|---|
| A | [키와 보호 저장](2026-10-18-m3-a-keys-storage.md) | A1 파생·타입, A2 snapshot, A3 OS adapter, A4 endpoint 전환, A5 생성·복구 상태 |
| B | [목록·인증·기기 연결](2026-10-18-m3-b-identity-pairing.md) | B1 wire/packet, B2 게시·충돌·해제, B3 account auth, B4 초대·연락처, B5 pair channel, B6 durable 전달·재개 |
| C | [앱 통합과 검증](2026-10-18-m3-c-app-validation.md) | C1 FRB/상태, C2 계정·연락처 화면, C3 QR/native UX, C4 자동 시나리오/Release/실기기 증거 |

A1 → A2 → A3 → A4가 기본 순서다. A1 뒤 B1의 순수 codec 작업은 A2/A3와 병렬 가능하다. A5는 A4+B1, B2는 A5+B1, B3는 A4+B2, B4는 A5+B1, B5는 A4+B1, B6는 A5+B2+B5 뒤다. C1은 B의 명령/타입 고정 뒤, C2는 C1, C3는 C1/C2, C4는 전체 통합 뒤다. 각 task의 TDD·검토 gate가 끝난 뒤 의존 작업을 시작한다. native 플랫폼별 A3 구현은 동일 인터페이스 아래 별도 worker로 나눌 수 있다.

## 소유권과 실행 준비

- 현재 기준은 linked Orca worktree의 `HyunwookYoo/m2-iroh-transport`, HEAD `aadd2db`다. 기존 M2 문서 3개 수정과 승인된 M3 spec/본 계획을 보존한다. 원본 `C:/ChatProject`와 과거 provider pane/transcript는 읽기 전용이다.
- 구현 시작 시 먼저 Orca 상태/현재 격리를 확인한다. 별도 M3 checkout을 만들 경우 Orca CLI를 사용하며, M2 코드 `aadd2db`를 명시적으로 기준으로 잡고 M3 문서만 전달한다. 이 기준은 M2에 의존하는 작업이며 main에 M2가 이미 통합됐다고 가정하지 않는다. 기존 worktree를 중첩 생성하거나 미commit M2 문서를 자동 이동하지 않는다.
- 통합 담당은 manifests/lockfiles, `core/chat_core/src/{lib,event,error,runtime}.rs`, `app/rust/src/{lib,convert}.rs`, `app/rust/src/api/**`, generated bindings, 공통 CI를 독점한다. 다른 worker는 자기 새 모듈과 해당 테스트만 편집하고 공통 파일 변경 요청을 보고한다.
- UI worker: `app/lib/account/**`, 화면 통합과 widget tests. 플랫폼 worker: A3의 OS별 파일. 자동화 worker: `puppet/**`, `tools/e2e/**`, `tools/chat_node/**`. 공유 경로를 동시에 수정하지 않는다.
- task마다 fresh implementer → spec 검토 → code 검토 → 해당 시험 → 범위 지정 commit. 마지막 whole-branch review에서 primitive/권한/복구 의미를 재확인한다. 검토 수정은 해당 task 소유자에게 돌린다.
- 계획 승인 뒤 로컬 단위/플랫폼 빌드·로컬 relay/emulator 자동화는 구현 검증에 포함한다. 실제 휴대전화 연결, 공개 DHT/relay 시험, signed 배포/업로드, 원격 push/PR/merge는 준비물·정확한 대상·시나리오를 제시한 뒤 별도 실행 범위를 확인한다. 기존 기기 승인/연결 상태를 현재 준비 완료로 가정하지 않는다.

## 완료 판단과 인계

- A/B의 unit·loopback 증거, C의 widget·native·Release·시나리오 증거를 각각 기록한다. 계획에 적힌 command는 **향후 실행 명령**이며 이번 계획 작성 중 통과한 시험이 아니다.
- `docs/superpowers/results/2026-10-18-m3-account-devices.md`에 기준 commit/toolchain, 각 task 결과, 플랫폼별 실제/가짜 저장 경계, signed iPhone 여부, 미실행/실패/사용자 유예를 구분한다. 비밀·원문 QR·실제 계정 키는 넣지 않는다.
- M3 완료는 설계 §10 전체 증거가 확보된 뒤 판단한다. M2 완료나 500ms 목표 수용은 M3 결과로 대신하지 않는다. 원격 통합·배포는 완료 판단과 별개다.

## 작성 자체 검토 — 2026-10-18

설계 §3→A1/A5, §4→A2/A3/A4, §5→B1/B2/B3, §6→B4, §7→B5/B6, §8→A5/B2/C1, §9→C1/C2, §10→전 task/C4로 대응한다. 공통 타입은 A1/A2와 B1의 표를 사용하며 FFI는 C1에서 한 번 통합한다. Review Focus 5개를 소유 task의 시험에 배치했다. 의존성은 공식 metadata/API를 확인했으며 실제 target compile은 A1/A3/B1/B5 gate다. 제품 코드·manifest·lockfile·generated 파일은 이 계획 작성에서 변경하지 않는다.
