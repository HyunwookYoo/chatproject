# P2P Chat 아키텍처 및 프로토콜 설계

- 작성일: 2026-08-23 (rev.2 — 서버 의존도 최소화 반영)
- 선행 문서: [`docs/research/2026-08-23-p2p-chat-feasibility.md`](../../research/2026-08-23-p2p-chat-feasibility.md) (실측 근거)
- 상태: **rev.3로 대체됨** → [`2026-10-04-p2p-chat-design.md`](2026-10-04-p2p-chat-design.md) (리뷰: [`2026-10-03-p2p-chat-design-review.md`](2026-10-03-p2p-chat-design-review.md))

---

## 1. 확정된 결정사항

| 항목 | 결정 | 근거 |
|---|---|---|
| 프레임워크 | **Flutter (UI) + Rust (코어)** via `flutter_rust_bridge` | 실측 RSS 83 MB vs Tauri 348 MB. 3플랫폼 1급 지원 |
| 전송 | **iroh (QUIC)** | 홀펀칭 + 릴레이 폴백 포함 ~90–95% 연결 성공 |
| 그룹 암호 | **MLS (RFC 9420) / OpenMLS** | 처음부터 그룹 포함. TreeKEM O(log N) |
| 오프라인 전달 | 필수 | 사용자 결정 |
| 푸시 알림 | 필요 | 사용자 결정 |
| iOS 배포 | TestFlight / 사이드로드 우선 | 사용자 결정 |
| **서버 의존도** | **구성 P를 기본으로, 구성 R로 승격 가능** | 사용자 결정 — 2절 |
| VPN 익스텐션 무서버 모드 | 채택 안 함 | 상시 소켓은 목적이 아니라 수단 |

### 1.1 요구사항 #2의 확정 해석

> "채팅 기록은 채팅 한 사람들만 가지고 있는다"
> → **대화 내용과 그 영구 기록을 보유하는 주체는 오직 참여 단말이다.**

구성 P에서는 이것이 **문자 그대로** 성립한다. 큐가 존재하지 않고 저장 주체가 오직 참여자 기기이기 때문이다. 구성 R로 승격하면 릴레이가 암호문을 짧은 TTL 동안 보관하지만 평문에는 접근할 수 없다.

---

## 2. 서버 의존도 — 설계의 중심 축

이 프로젝트는 서버 의존도를 **설정 가능한 축**으로 만든다. 같은 코드베이스이며 구성만 다르다.

### 2.1 두 구성

| | **구성 P — 자립 (기본)** | **구성 R — 릴레이 (승격)** |
|---|---|---|
| 우리가 운영하는 것 | **stateless 푸시 프록시 1개** (iOS 전용) | + 큐 릴레이 1대 (서울) |
| 오프라인 전달 | 다단말 + 발신자 outbox | + 릴레이 큐 (TTL 24–48h) |
| iOS 푸시 | 프록시 경유, 암호문을 페이로드에 동봉 | 동일 + 큐에서 당겨오기 |
| Android 푸시 | **UnifiedPush** (피어가 직접 POST) | UnifiedPush 또는 FCM |
| MLS 커밋 순서 | **지정 커미터** (propose/commit 분리) | 릴레이 시퀀서 |
| 홀펀칭 실패 폴백 | n0 공개 릴레이 (실측 146 ms) | 서울 자체 릴레이 (실측 9 ms) |
| 첨부 오프라인 전달 | ❌ (발신 기기가 보관) | ✅ blob TTL 저장 |
| 운영 부담 | 거의 0 (무료 티어) | VPS 1대 (월 5–10달러) |

### 2.2 승격 조건 — 언제 R로 가는가

구성 P가 부족해지는 지점은 명확하다. 아래 중 하나라도 실제 문제가 되면 R로 승격한다.

1. **양쪽 모두 상시 기기가 없다** — 폰만 쓰는 사용자끼리 접속 시간대가 안 겹치면 메시지가 며칠 지연된다
2. **4 KB 초과 메시지의 iOS 알림 내용 표시** — 구성 P는 푸시 페이로드 크기(4 KB)를 넘으면 "새 메시지"로만 표시
3. **첨부파일 오프라인 전달**
4. **릴레이 폴백 지연** — 실측 146 ms가 체감상 문제가 될 때
5. **그룹에서 커미터 부재가 잦다** — 멤버 변경이 자주 막힐 때

> 승격은 **릴레이 URL을 설정에 넣는 것**으로 끝난다. 프로토콜과 클라이언트 코드는 동일하다.

### 2.3 사용자 소유 릴레이 (BYO)

릴레이 URL은 사용자가 직접 지정할 수 있다. 파워유저는 자기 릴레이를 운영하고, 그 경우 "우리를 신뢰해야 하는가"라는 질문 자체가 사라진다. SimpleX 모델.

### 2.4 시스템 개요

```
┌────────────────────────────────────────────────────────────┐
│  Client (Windows / Android / iOS)                          │
│  ┌──────────────────────────────────────────────────────┐  │
│  │  Flutter UI (Dart) — 화면만. 상태를 소유하지 않음      │  │
│  └───────────────────────┬──────────────────────────────┘  │
│     Command ↓            │            ↑ Stream<ChatEvent>  │
│  ┌───────────────────────┴──────────────────────────────┐  │
│  │  Rust Core                                           │  │
│  │  ┌─────────┬─────────┬─────────┬──────────────────┐  │  │
│  │  │transport│ crypto  │  store  │  sync            │  │  │
│  │  │ (iroh)  │(OpenMLS)│(SQLCiph)│ (outbox/companion│  │  │
│  │  │         │         │         │  /queue/push)    │  │  │
│  │  └─────────┴─────────┴─────────┴──────────────────┘  │  │
│  └──────────────────────────────────────────────────────┘  │
└───┬──────────────┬────────────────┬──────────────┬─────────┘
    │ ① 직접 P2P   │ ② 동반기기      │ ③ 푸시 프록시 │ ④ 큐 릴레이
    │  최우선       │  (내 PC=내 큐)  │  stateless   │  [R에서만]
    ▼              ▼                ▼              ▼
 ┌──────┐    ┌───────────┐    ┌───────────┐  ┌──────────┐
 │ Peer │    │ 내 다른   │    │ APNs 프록시│  │ Relay    │
 │      │    │ 기기      │    │ (무상태)   │  │ (서울)   │
 └──────┘    └───────────┘    └───────────┘  └──────────┘
```

### 2.5 경로 선택 순서

```
send(conversation, msg):
  1. 직접 P2P 연결이 살아있다 → 즉시 전송              [서버 0]
  2. dial 시도 (캐시 → mDNS → DHT → 홀펀칭)           [서버 0]
  3. 실패 → 상대의 다른 기기에 dial 시도                [서버 0]
  4. 전부 실패 → outbox에 보관 + 푸시 신호 발송         [프록시만]
  5. [구성 R] 큐 릴레이에 투척                          [릴레이]
```

**1~3이 성공하면 서버를 전혀 거치지 않는다.** 활성 대화의 대부분이 여기 해당한다.

---

## 3. 동반기기(Companion Device) — 구성 P의 핵심

### 3.1 원리

MLS에서 다단말은 각 installation이 독립적인 그룹 멤버(leaf)다. 따라서 **하나의 애플리케이션 메시지가 모든 installation에게 복호 가능하다.**

```
B → [MLS 그룹] → A의 PC (온라인, 수신 O)
                └→ A의 폰 (오프라인, 수신 X)

나중에 A의 폰이 켜짐:
  A의 폰 → A의 PC 에 dial (LAN이면 mDNS로 즉시)
        → PC가 보관 중인 MLS 암호문을 그대로 재전송
        → 폰이 정상 복호 (같은 epoch 비밀을 가진 멤버이므로)
```

**양쪽 모두 상시 기기를 하나씩 가지면 오프라인 전달이 서버 0대로 완전히 해결된다.**

### 3.2 두 방향 모두 작동한다

| 상황 | 동작 |
|---|---|
| **수신**: 내가 오프라인일 때 온 메시지 | 내 PC가 받아둠 → 폰이 켜지면 PC에서 가져옴 |
| **송신**: 상대가 오프라인일 때 내가 보냄 | 내 PC의 outbox가 계속 재시도 → 상대가 켜지면 전달 |

### 3.3 동반기기 프로토콜

```
COMPANION_SYNC_REQ  { since: (epoch, generation) per conversation }
COMPANION_SYNC_RESP { envelopes: Vec<StoredEnvelope> }
COMPANION_OUTBOX_HANDOFF { entries: Vec<OutboxEntry> }   // 내 기기끼리 발신 큐 인계
```

- 내 기기끼리는 **같은 IdentityKey**로 상호 인증한다. 별도 페어링 불필요.
- 발견: mDNS 우선(같은 LAN), 없으면 DHT + 홀펀칭.
- 어느 기기가 "상시 기기"인지는 자동 판정한다 — 데스크톱 플랫폼 + 최근 7일 가동률 기준. 사용자가 수동 지정도 가능.
- 여러 기기가 상시면 전부 보관한다(중복은 무해하다).

### 3.4 한계 — 정직하게

- **상시 기기가 없는 사용자에게는 아무 도움이 안 된다.** 폰만 쓰는 사용자끼리는 온라인 윈도우가 겹쳐야 한다.
- 이것이 구성 R 승격의 첫 번째 조건이다(2.2절).

### 3.5 부수 효과

MLS의 forward secrecy 때문에 **새 단말은 과거 메시지를 복호할 수 없다.** 동반기기 채널이 있으면 기존 기기가 평문 히스토리를 직접 전송해 이 문제를 해결할 수 있다(사용자 확인 후, 별도 대칭키로 암호화).

---

## 4. 아이덴티티 및 키 아키텍처

### 4.1 키 계층

```
DeviceSeed (32B, 단말별, OS 키체인/Keystore/DPAPI)
  │
  ├─► IdentityKey     Ed25519   장기 아이덴티티. 이것이 곧 사용자다
  │     └─ PeerId = base32(pubkey)
  │
  ├─► TransportKey    Ed25519   iroh EndpointId
  │
  ├─► MLS Credential  Ed25519   IdentityKey로 서명된 basic credential
  │     └─ per-installation KeyPackage (단말마다 별개)
  │
  ├─► PushHandle      불투명     8절. 대화별로 발급
  │
  └─► [구성 R] QueueKeys  Ed25519  큐마다 새로 생성

[선택 · 나중] WalletKey  secp256k1  절대 위 계층에서 파생하지 않는다
```

### 4.2 원칙

1. **PeerId = 공개키.** 서버가 발급하는 계정 ID는 존재하지 않는다. 나중에 온체인 레지스트리를 얹을 수 있는 전제다.
2. **다단말은 MLS installation으로 표현한다.** 단말 추가 = MLS 그룹에 멤버 추가.
3. **지갑 키를 절대 파생하지 않는다.** 서명 바인딩만 (13.5절).

### 4.3 연락처 추가 — 서버 없이

```
Invite {
  version: 1
  peer_id:       [u8;32]        // IdentityKey 공개키
  endpoint_hint: EndpointAddr    // iroh 주소 힌트 (만료 가능)
  key_package:   MlsKeyPackage
  push_handle:   Option<PushHandle>
  expires_at:    u64
  sig:           [u8;64]
}
```

QR / 딥링크 / 복붙으로 전달한다. 이후 **안전번호**(양쪽 IdentityKey 해시) 대조로 MITM을 검증한다.

---

## 5. 전송 계층 (iroh)

### 5.1 dial 순서

```
1. 캐시된 EndpointAddr                     (0 RTT 추가)
2. mDNS                                    (LAN이면 <10 ms)
3. pkarr / Mainline DHT                    (무서버, 수백 ms)
4. [옵션] 자체 DNS 디스커버리                (배치 시 <10 ms)
→ QUIC 홀펀칭 → 실패 시 릴레이 폴백
```

n0의 공개 `dns.iroh.link`는 **쓰지 않는다.** 실측 258 ms(독일)로 콜드 다이얼을 망친다. 디스커버리는 DHT를 기본으로 한다.

### 5.2 릴레이 폴백

| 구성 | 릴레이 | 실측 왕복 |
|---|---|---|
| P | n0 공개 (뭄바이 최근접) | ~146 ms |
| R | 서울 자체 | ~9 ms |

구성 P에서도 대부분(~95%)은 직접 경로로 흐르므로 146 ms는 소수 케이스에만 적용된다. 릴레이 맵은 설정으로 교체 가능하다.

---

## 6. MLS 그룹 계층

### 6.1 커밋 순서 문제

MLS는 Commit 메시지의 **전역 순서**를 요구한다. 두 멤버가 같은 epoch에서 동시에 커밋하면 그룹이 갈라진다. **이것은 "서버"가 아니라 "순서"에 대한 요구다.**

#### 구성 P — propose/commit 분리 + 지정 커미터

MLS는 **Proposal과 Commit을 분리**한다. 이 성질을 그대로 활용한다.

```
- Proposal (add/remove/update)  → 누구나 발행 가능. 순서 무관
- Commit                        → 지정 커미터만 발행. 단일 작성자이므로 순서가 자명
```

| 대화 | 커미터 지정 |
|---|---|
| 1:1 | PeerId가 작은 쪽 (결정적, 협상 불필요) |
| 그룹 | 그룹 생성자. 부재 시 온라인 멤버 중 PeerId 최소로 인수인계 |

- **애플리케이션 메시지는 커밋이 아니다.** 누구나 언제든 보낼 수 있고 순서 문제가 없다. 커미터가 오프라인이어도 대화는 정상 동작한다.
- 커미터 부재의 영향은 **멤버 변경과 키 갱신이 지연되는 것뿐**이다. 둘 다 드문 이벤트다.
- 인수인계 충돌 시: 두 커밋이 같은 epoch에 도달하면 `hash(commit)`이 작은 쪽을 채택하고 패자는 재시도한다(결정적 해소).

#### 구성 R — 릴레이 시퀀서

```
COMMIT_APPEND { group_queue_id, commit_ciphertext } → { seq }
    - 릴레이가 seq를 원자적으로 부여
    - 릴레이는 커밋 내용을 볼 수 없다 (오직 순서만)
```

#### 확장 훅

이 자리는 나중에 **온체인 컨트랙트로 교체 가능하다**. 순서 합의는 블록체인이 잘하는 일이고 커밋은 작고 드물다. XMTP의 `GroupMessages` 컨트랙트가 정확히 이 방식이다. 13.3절.

### 6.2 그룹 라이프사이클

| 동작 | MLS 연산 | 구성 P에서 |
|---|---|---|
| 그룹 생성 | `create_group` | 생성자가 커미터 |
| 멤버 추가 | Add Proposal → Commit + Welcome | 누구나 제안, 커미터가 확정 |
| 멤버 제거 | Remove Proposal → Commit | 동일 |
| 단말 추가 | 자기 installation 추가 | 동일 |
| 키 갱신 (PCS) | Update Proposal → Commit | 멤버가 제안, 커미터가 일괄 확정 |
| 메시지 | `create_message` | **커미터 무관, 항상 가능** |

### 6.3 사이퍼수트

`MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (필수 구현 사이퍼수트).

포스트퀀텀(X-Wing 등)은 채택하지 않는다 — IANA 표준화 전이고 상호운용 보장이 없다. `ciphersuite` 필드만 두어 나중에 협상 가능하게 한다.

---

## 7. 메시지 포맷

### 7.1 전송 봉투

```rust
struct Envelope {
    version:   u8,          // 1
    target:    Target,      // Peer(PeerId) | Queue([u8;32])
    payload:   Vec<u8>,     // MLS PrivateMessage
    padded_to: u16,         // 크기 버킷: 1K / 4K / 16K / 64K
}
```

**패딩 필수**: 메시지 크기 자체가 메타데이터다.

### 7.2 애플리케이션 페이로드 (MLS 안, 참여자만 복호)

```rust
struct ChatMessage {
    msg_id:    [u8; 16],
    prev_hash: [u8; 32],    // 로컬 해시체인 — 13.4절 앵커링 대비
    sent_at:   u64,         // 송신자 로컬 ms. 신뢰 불가, 정렬 힌트
    content:   Content,
}

enum Content {
    Text        { body: String, mentions: Vec<PeerId> },
    Attachment  { blob_id, key, mime, size, thumbnail },
    Reaction    { target, emoji },
    Edit        { target, body },
    Delete      { target },
    Receipt     { upto, kind: Delivered | Read },
    Typing      { active },              // 직접 연결에서만 전송
    System      { event: SystemEvent },
    Custom      { kind: String, data: Vec<u8> },   // ← 확장 지점
}
```

**확장 규칙**: variant 추가 시 구버전은 `Custom`으로 폴백해 "지원하지 않는 메시지"로 표시한다. 파싱 실패로 대화가 깨지지 않는다.

### 7.3 순서와 시각

- `sent_at`은 송신자가 조작할 수 있다. **정렬 근거로 삼지 않는다.**
- 정렬: `(mls_epoch, mls_generation)` → `sent_at` → `msg_id` 사전순.
- `prev_hash`는 송신자별 로컬 체인이다. 수신자가 갭을 감지할 수 있다.

### 7.4 첨부파일

- 랜덤 키로 AES-256-GCM 암호화. 키는 `Content::Attachment.key`로 MLS 안에서 전달.
- **구성 P**: 양쪽 온라인이면 iroh 직접 스트림. 아니면 발신 기기가 보관하고 재시도 (동반기기가 인계 가능).
- **구성 R**: 릴레이 blob 저장소에 TTL 업로드. 릴레이는 키를 모른다.

---

## 8. 푸시 알림

### 8.1 Android — 서버 불필요

**UnifiedPush**를 사용한다. 사용자가 distributor(ntfy 등)를 고르면 **엔드포인트 URL**을 받고, 그 URL을 아는 사람은 누구나 POST할 수 있다. 즉 **상대 피어가 직접 푸시를 쏜다.**

- 엔드포인트 URL은 `PushHandle`로서 MLS 채널을 통해 상대에게 전달된다
- FCM 불필요, 게이트웨이 불필요
- 대가: 사용자가 distributor 앱을 설치해야 한다. 우리 앱에 ntfy 클라이언트를 내장해 마찰을 줄일 수 있다
- 폴백: distributor가 없으면 FCM (구성 R)

### 8.2 iOS — stateless 프록시

APNs는 우리 앱의 인증키로만 발송 가능하므로 프록시가 필요하다. 다만 **완전히 무상태로 만들 수 있다.**

```
PushHandle = base64( AEAD_encrypt(proxy_key, {
    device_token,        // APNs 토큰
    conversation_tag,    // 대화별 구분 (차단용)
    issued_at, expires_at
}) )
```

- 수신자가 자기 handle을 발급하고 **MLS 채널로** 송신자에게 전달한다
- 송신자는 `POST /push { handle, payload }` 만 호출한다
- 프록시는 handle을 복호해 토큰을 얻고 APNs로 전달한 뒤 **아무것도 저장하지 않는다**
- 프록시 키만 갖고 있으면 되므로 Cloudflare Workers 무료 티어로 충분하다

**스팸 방지**: handle은 대화별로 발급되므로 남용하는 상대를 특정해 차단할 수 있다(수신자가 해당 handle을 폐기). handle에 만료 시각이 들어 있어 무기한 재사용도 불가능하다. 프록시는 handle 단위 rate limit만 짧은 윈도우로 적용한다(메모리 카운터).

**프록시가 아는 것**: "이 토큰에 푸시가 갔다"뿐이다. 송신자가 누구인지, 내용이 무엇인지, 어떤 대화인지 모른다.

### 8.3 알림에 내용 표시 — 구성 P에서도 가능

**푸시 페이로드에 MLS 암호문을 동봉한다.**

```json
{ "aps": { "mutable-content": 1, "alert": { "loc-key": "NEW_MSG" } },
  "e": "<base64 MLS ciphertext>" }
```

- APNs 페이로드 상한은 4 KB다. 짧은 텍스트는 충분히 들어간다
- NSE가 페이로드의 암호문만 복호하면 되므로 **네트워크 스택이 불필요하다** — iroh를 NSE에서 돌릴 필요가 없다
- 4 KB를 넘으면 암호문을 빼고 "새 메시지"로만 표시한다. 본문은 앱 실행 시 수신
- 구성 R에서는 초과분도 큐에서 당겨올 수 있다 (HTTP만 필요하므로 여전히 가볍다)

> APNs(Apple)는 암호문의 크기와 빈도를 관찰할 수 있다. 내용은 볼 수 없다.

### 8.4 완화 조치

- 푸시 페이로드에 발신자 정보를 넣지 않는다
- 발송에 0–3초 지터를 넣어 타이밍 상관을 약화한다
- 사용자가 **푸시를 완전히 끌 수 있다**. 이 경우 프록시조차 사용하지 않는다

---

## 9. 큐 릴레이 프로토콜 — 구성 R에서만

SimpleX의 SMP를 참고하되 단순화한다. **릴레이가 사용자 식별자도, 소셜 그래프도, 평문도 갖지 못하게 한다.**

### 9.1 큐 모델

- 큐는 **단방향**이다. A↔B 대화에는 큐 2개가 존재한다
- 큐 ID는 32바이트 랜덤. 아이덴티티에서 파생하지 **않는다**
- 큐마다 두 개의 Ed25519 키쌍:
  - `senderKey` — 큐에 넣을 권한. **송신자만** 개인키를 안다
  - `recipientKey` — 큐에서 뺄 권한. **수신자만** 개인키를 안다
- 릴레이는 두 **공개**키와 큐 ID만 안다

### 9.2 명령

```
CREATE  { recipient_pub, sender_pub, ttl_secs }   → { queue_id }   sig: recipient
SEND    { queue_id, ciphertext, ts }              → { ok }         sig: sender
RECV    { queue_id, since_seq, max }              → [ {seq, ct, ts} ]  sig: recipient
ACK     { queue_id, upto_seq }                    → { ok }         sig: recipient → 즉시 삭제
SUB     { queue_id }                              → 스트림          sig: recipient
DELETE  { queue_id }                              → { ok }         sig: recipient
COMMIT_APPEND { group_queue_id, commit_ct }       → { seq }        (6.1절)
```

제약: `ciphertext ≤ 64 KiB`, 큐당 미수신 ≤ 256개.

### 9.3 저장 정책 — 짧게 유지한다

- 기본 TTL **24시간** (최대 48시간). ACK 수신 즉시 삭제
- **저장 기간이 곧 서버의 역할 크기다.** 동반기기가 대부분을 흡수하므로 길게 잡을 이유가 없다
- 릴레이는 디스크에 평문 로그를 남기지 않는다
- 큐가 30일간 미사용이면 자동 삭제

### 9.4 큐 로테이션

대화당 큐를 주기적으로 교체한다. 새 `QueueRef`는 **기존 MLS 채널 안에서** 전달하므로 릴레이는 교체 사실을 알 수 없다.

---

## 10. 프라이버시 한계 — 정직한 기록

| 관찰자 | 구성 P에서 아는 것 | 구성 R에서 추가로 아는 것 |
|---|---|---|
| 푸시 프록시 | "이 토큰에 푸시가 갔다" (송신자·내용·대화 불명) | — |
| APNs / UnifiedPush | 암호문 크기·빈도 | — |
| 큐 릴레이 | (존재하지 않음) | 큐별 트래픽 타이밍·크기, 큐↔디바이스 매핑(푸시 등록 시), 클라이언트 IP |
| 네트워크 관찰자 | 트래픽 존재·타이밍 | 동일 |

**구성 P의 프라이버시가 구성 R보다 실질적으로 강하다.** 상시 관찰 지점이 없기 때문이다.

**명시적으로 범위 밖**: 글로벌 수동 관찰자에 대한 익명성, 트래픽 분석 저항, 발신자 익명성(sealed sender).

---

## 11. 로컬 저장

### 11.1 엔진

SQLite + **SQLCipher** (`rusqlite`, bundled-sqlcipher). DB 키는 OS 보안 저장소에 보관:

| 플랫폼 | 저장소 |
|---|---|
| Windows | DPAPI |
| Android | Keystore (하드웨어 지원 시 StrongBox) |
| iOS | Keychain (`kSecAttrAccessibleAfterFirstUnlock`, 공유 그룹) |

> iOS: NSE가 접근해야 하므로 **App Group 컨테이너**에 DB를 둔다.

### 11.2 스키마 (요약)

```sql
identity(id, seed_ref, peer_id, created_at)
installation(id PK, peer_id, kind[desktop|mobile], is_companion, last_seen)
contact(peer_id PK, display_name, verified_at, safety_number, added_at)
conversation(id PK, kind[dm|group], mls_group_id, title, committer_peer_id, created_at)
member(conversation_id, peer_id, installation_id, role, joined_epoch)
message(id PK, conversation_id, sender_peer_id, msg_id, prev_hash,
        sent_at, recv_at, epoch, generation, content_json, state)
  INDEX (conversation_id, epoch, generation, sent_at)
outbox(id PK, conversation_id, target_peer, payload, attempts, next_retry_at, created_at)
companion_store(id PK, conversation_id, envelope, for_installation, received_at)
push_handle(peer_id, conversation_id, handle, platform, expires_at)
queue(...)         -- 구성 R에서만
attachment(blob_id PK, message_id, key, mime, size, local_path, state)
mls_state(group_id PK, blob)
```

### 11.3 메시지 상태 기계

```
composing → queued → sent_direct | sent_companion | sent_relay → delivered → read
                  └→ failed (재시도 소진)
```

`outbox`는 지수 백오프(1s, 2s, 4s … 최대 5분)로 재시도한다. 앱이 죽어도 재시작 시 이어서 처리한다. **상시 기기는 백오프 상한을 30초로 짧게 유지한다** — 배터리 제약이 없으므로.

---

## 12. Rust ↔ Dart 경계

### 12.1 원칙: 상태는 전부 Rust가 소유한다

Dart는 UI만 그린다. 상태를 두 곳에 두면 반드시 갈라진다.

```rust
// Dart → Rust : 명령 (모두 async)
async fn send_text(conversation_id: String, body: String) -> Result<MessageId>;
async fn create_group(title: String, members: Vec<PeerId>) -> Result<ConversationId>;
async fn propose_add_member(conversation_id: String, peer: PeerId) -> Result<()>;
async fn create_invite() -> Result<InviteTicket>;
async fn accept_invite(ticket: String) -> Result<ConversationId>;
async fn load_messages(conversation_id, before: Option<MessageId>, limit: u32)
        -> Result<Vec<MessageView>>;
async fn set_push_config(cfg: PushConfig) -> Result<()>;
async fn set_relay(url: Option<String>) -> Result<()>;   // None = 구성 P
async fn set_companion_role(enabled: bool) -> Result<()>;

// Rust → Dart : 단일 이벤트 스트림
fn events() -> Stream<ChatEvent>;

enum ChatEvent {
    MessageReceived     { conversation_id, message: MessageView },
    MessageStateChanged { message_id, state: MessageState },
    ConversationUpdated { conversation_id },
    PeerPresence        { peer_id, online: bool, path: Direct | Companion | Relay },
    ConnectionState     { direct_peers: u32, companion_ok: bool, relay_ok: Option<bool> },
    GroupEvent          { conversation_id, event: SystemEvent },
    Error               { code: ErrorCode, detail: String },
}
```

### 12.2 규칙

1. **UI는 `events()` 한 스트림만 구독한다.** 여러 스트림은 순서 보장을 깬다
2. **긴 목록은 Rust가 페이지 단위로 준다.** FFI 직렬화 비용이 그대로 프레임 드랍이다
3. **에러는 `ErrorCode` enum으로 구조화한다.** 문자열 매칭 금지
4. **블로킹 호출 금지.** tokio 런타임에서 처리하고 즉시 반환
5. **로컬 에코 먼저.** `send_text`는 DB에 `queued`로 즉시 쓰고 이벤트를 먼저 흘린다

### 12.3 iOS NSE와의 코어 공유

NSE는 별도 프로세스이며 메모리 예산이 본체보다 훨씬 빡빡하다.

- Rust 코어를 **`full` / `nse` 두 feature로 분리 빌드**한다
  - `nse`: **MLS 복호 + 알림 문구 생성만.** iroh·DHT·UI·첨부 전부 제외
  - 8.3절 설계 덕분에 NSE에 **네트워크 스택이 불필요하다** (암호문이 푸시 페이로드에 동봉되므로)
- App Group DB를 공유하되 WAL 모드 + 짧은 트랜잭션으로 잠금 경합을 피한다
- **⚠️ 구현 전 실측 필요**: NSE 메모리 상한과 OpenMLS 그룹 상태 로드가 그 안에 들어가는지. 큰 그룹에서 문제가 될 수 있다. 실패 시 폴백은 "새 메시지"만 표시

---

## 13. 확장 지점 (요구사항 4 + 블록체인 대비)

전부 **trait로 추상화**하고 기본 구현을 꽂아둔다.

### 13.1 `IdentityResolver` — PeerId → 주소
```rust
trait IdentityResolver {
    async fn resolve(&self, peer: &PeerId) -> Result<EndpointAddr>;
    async fn publish(&self, addr: &EndpointAddr) -> Result<()>;
}
```
지금: `PkarrDhtResolver`, `MdnsResolver`, `CachedResolver` → 나중: `OnChainResolver`

### 13.2 `NameRegistry` — 이름 → PeerId
```rust
trait NameRegistry {
    async fn lookup(&self, name: &str) -> Result<PeerId>;
    async fn reverse(&self, peer: &PeerId) -> Result<Option<String>>;
}
```
지금: `LocalContactRegistry` → 나중: `EnsRegistry`

### 13.3 `CommitOrderer` — MLS 커밋 순서 ★
```rust
trait CommitOrderer {
    async fn append(&self, group: &GroupId, commit: &[u8]) -> Result<u64>;
    async fn fetch_since(&self, group: &GroupId, seq: u64) -> Result<Vec<(u64, Vec<u8>)>>;
}
```
- 구성 P: `DesignatedCommitterOrderer` (6.1절)
- 구성 R: `RelayOrderer`
- 나중: `OnChainOrderer` — **블록체인이 이 설계에서 진짜로 유용해지는 유일한 지점.** XMTP 방식

### 13.4 `Anchor` — 로컬 해시체인 앵커링
```rust
trait Anchor {
    async fn anchor(&self, root: [u8; 32]) -> Result<AnchorProof>;
    async fn verify(&self, root: [u8;32], proof: &AnchorProof) -> Result<bool>;
}
```
지금: `NoopAnchor` → 나중: `OnChainAnchor` (머클 루트만. **메시지는 절대 올리지 않는다**)

### 13.5 지갑 바인딩
```rust
struct IdentityBinding {
    chat_pubkey:     [u8; 32],   // Ed25519
    wallet_address:  [u8; 20],   // EVM
    sig_by_wallet:   Vec<u8>,    // 지갑이 chat_pubkey를 서명
    sig_by_identity: [u8; 64],   // chat key가 wallet_address를 서명
}
```
양방향 서명이라 한쪽만으로는 사칭할 수 없다.

### 13.6 블록체인 확정 원칙

1. **메시지는 절대 온체인에 올리지 않는다.** 요구사항 #2와 충돌하고 forward secrecy를 파괴한다
2. **온체인 작업은 메시지 hot path에 넣지 않는다.** 초~분 단위다
3. **App Store**: 지갑 기능은 Apple이 **Organization 계정**을 요구한다. TestFlight 단계에서는 무관하나 정식 배포 시 전환 필요
4. 법률 판단(특금법/MiCA)은 이 문서 범위 밖이며 기능 확정 시 별도 자문 필요

---

## 14. 성능 예산 (실측 기반)

| 구간 | 목표 | 근거 |
|---|---|---|
| 입력 → 화면 표시 (로컬 에코) | **< 16 ms** | 네트워크를 기다리지 않음 |
| MLS 암·복호 | < 1 ms | 실측 스택 오버헤드 p50 0.14 ms |
| 직접 P2P 편도 (국내) | < 20 ms | 실측 서울 RTT 4.4 ms |
| 동반기기 경유 (LAN) | < 5 ms | mDNS 직결 |
| 릴레이 폴백 — 구성 P (뭄바이) | ~73 ms 편도 | **실측** |
| 릴레이 폴백 — 구성 R (서울) | ~5 ms 편도 | **실측** |
| 콜드 연결 (캐시 히트) | < 300 ms | ICE 47 ms + QUIC 1-RTT |
| 콜드 연결 (DHT 조회) | < 1.5 s | DHT 지연 지배 |
| 앱 콜드스타트 → 대화 목록 | < 600 ms | 실측 Flutter 328 ms + DB |
| 유휴 RSS (Windows) | < 150 MB | 실측 Hello World 83 MB + 코어/DB |

### 14.1 반응성 전략

1. **로컬 에코 우선** — 전송은 낙관적으로 표시하고 상태만 갱신
2. **연결 예열** — 포그라운드 진입 시 최근 대화 상대와 동반기기에 미리 dial
3. **주소 캐시 적극 활용** — DHT 조회는 캐시 미스일 때만
4. **DB 페이지네이션** — 대화당 최근 50개만 로드

---

## 15. 플랫폼별 구현 노트

| | Windows | Android | iOS |
|---|---|---|---|
| 역할 | **동반기기 기본값** (상시 실행, 트레이 상주) | 조건부 동반기기 (충전 중 + Wi-Fi) | 동반기기 불가 |
| 백그라운드 | 제한 없음 | UnifiedPush 우선. FGS는 앱 활성 중에만 | APNs + NSE만 |
| 제약 | 없음 | `dataSync` FGS 24h 중 6h 제한, Doze, OEM 킬러 | NSE 메모리, App Group 필요 |
| 저장 키 | DPAPI | Keystore/StrongBox | Keychain (공유 그룹) |

**Android**: FGS를 상시 켜두지 않는다. 6시간 제한에 걸리고 배터리 평판도 나빠진다. 충전 중 + Wi-Fi일 때만 동반기기 역할을 수행한다.

---

## 16. 위협 모델

| 공격자 | 할 수 있는 것 | 할 수 없는 것 |
|---|---|---|
| 푸시 프록시 | 푸시 발송 관찰, 발송 거부 | 평문·송신자·대화 식별 |
| 악의적 릴레이 (구성 R) | 드롭, 순서 조작 시도, 큐↔디바이스 매핑, IP 관찰 | 평문 열람, 위조, 순서 조작을 들키지 않기 |
| 네트워크 관찰자 | 트래픽 존재·타이밍·대략 크기 | 내용 (QUIC+TLS+MLS 이중) |
| 단말 탈취 (잠금 해제) | 전부 | — |
| 단말 탈취 (잠금) | — | DB (SQLCipher + OS 키저장소) |
| 과거 그룹 멤버 | 재직 중 epoch | 제거 이후 epoch (MLS PCS) |
| 악의적 커미터 (구성 P) | 멤버 변경 거부/지연, 원치 않는 멤버 추가 시도 | 추가를 은폐 — 모든 멤버가 Commit을 검증한다 |

---

## 17. 미결정 사항 및 리스크

| # | 항목 | 영향 | 대응 |
|---|---|---|---|
| 1 | **NSE 메모리 예산 안에 OpenMLS 그룹 상태가 들어가는가** | 큰 그룹 iOS 알림 실패 | **초기 실측 필요.** 실패 시 "새 메시지"만 표시 |
| 2 | **동반기기 없는 사용자 비율** | 구성 P의 실효성 전체 | 초기 사용자에게 측정. 높으면 즉시 R로 승격 |
| 3 | UnifiedPush 사용자 마찰 (distributor 별도 설치) | Android 푸시 도달률 | ntfy 클라이언트 내장 검토. 폴백 FCM |
| 4 | 커미터 부재 시 인수인계 충돌 빈도 | 그룹 멤버 변경 지연 | 결정적 해소로 처리. 빈발하면 R 승격 |
| 5 | `openmls` / `iroh_flutter` Dart 바인딩 미성숙 | 의존 리스크 | 서드파티에 의존하지 않고 직접 `flutter_rust_bridge`로 래핑 |
| 6 | 신규 단말의 과거 메시지 | MLS forward secrecy로 복호 불가 | 동반기기 채널로 평문 히스토리 전송 (3.5절) |
| 7 | 그룹 규모 상한 | 미정 | 초기 50명. 푸시 팬아웃 비용 확인 필요 |
| 8 | 첨부 크기 상한 | 미정 | 초기 25 MB |
| 9 | 안전번호 검증 UX | MITM 방어 실효성 | 별도 UX 설계 필요 |

---

## 18. 다음 단계

승인되면 구현 계획(마일스톤 + 검증 기준)을 별도 작성한다. 순서만 적어두면:

1. Rust 코어 스켈레톤 + FFI 경계 + Flutter 셸 (3플랫폼 빌드 통과)
2. **iroh 직접 연결 1:1** — 저장도 MLS도 없이, 두 기기가 실제로 뚫리는지만
3. SQLCipher 저장 + 로컬 에코 + outbox 재시도
4. MLS 1:1 (지정 커미터)
5. **동반기기 동기화** — 구성 P의 핵심
6. MLS 그룹 (propose/commit 분리)
7. 푸시 — Android UnifiedPush → iOS 프록시 + NSE
8. 첨부파일
9. [필요 시] 구성 R — 큐 릴레이

**2번을 먼저 하는 이유**: 이 프로젝트의 가장 큰 미검증 가정이 "실제로 홀펀칭이 되는가"이고, 실측상 30%는 실패한다. 여기서 막히면 나머지 설계가 무의미하다.

**5번이 구성 P의 성패를 가른다.** 동반기기가 동작하지 않으면 구성 P는 A(서버 0대)와 다를 바 없는 UX가 된다.
