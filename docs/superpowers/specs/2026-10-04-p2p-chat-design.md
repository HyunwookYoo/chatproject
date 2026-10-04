# P2P Chat 아키텍처 및 프로토콜 설계

- 작성일: 2026-10-04 (rev.3 — 설계 리뷰 반영)
- 이전 판: [`2026-08-23-p2p-chat-design.md`](2026-08-23-p2p-chat-design.md) (rev.2)
- 리뷰: [`2026-10-03-p2p-chat-design-review.md`](2026-10-03-p2p-chat-design-review.md). 이 판은 리뷰의 차단 5건(B1–B5), 중요 9건(M1–M9), 정정 11건(C1–C11)과 사용자 결정 4건(D1–D4)을 반영한다.
- 선행 문서: [`docs/research/2026-08-23-p2p-chat-feasibility.md`](../../research/2026-08-23-p2p-chat-feasibility.md) (실측 근거)
- 상태: **승인 (2026-10-04)** — 다음 단계는 18절의 스파이크 A·B·C
- 승인 후 수정 (2026-10-04): 푸시 프록시 위치를 Cloudflare Workers에서 큐 서버 VPS로 바꿈, iOS 빌드는 클라우드 CI + TestFlight (둘 다 사용자 결정). 스파이크 B의 Windows 근사치 결과 반영(6.3, 17, 18절).
- UI 목업 반영 (2026-10-04, 사용자 승인): 초대장 표시 이름(4.4절), 기기 연결 양방향(4.5절), 알림 권한을 묻는 시점(8.6절). 목업은 Design 캔버스 "사용자 흐름 (시나리오 순)" 페이지에 있다(비공개 아티팩트 https://claude.ai/artifact/NbgPT2dyzGTGoZs4s6rG2Y).
- 구현 계획: [`docs/superpowers/plans/2026-10-04-p2p-chat-roadmap.md`](../plans/2026-10-04-p2p-chat-roadmap.md)

---

## 0. rev.3에서 바뀐 것

| 항목 | rev.2 | rev.3 | 리뷰 |
|---|---|---|---|
| 오프라인 전달 | 앱 전체가 구성 P(기본) 또는 구성 R(승격) | 사람마다 자동. 메일박스 기기가 있으면 서버 없이, 없으면 서버 큐 | B1, D1 |
| 메일박스 기기 | "동반기기". 데스크톱이면 상시 기기로 간주 | Windows PC(사용자 동의) 또는 상시 연결 모드 Android. 실제 가동률로 판정. iPhone은 불가 | B1, M7, M8 |
| Android 푸시 | UnifiedPush (FCM은 구성 R에서) | FCM 기본(푸시 프록시 경유), UnifiedPush 선택 | M1, D2 |
| 릴레이 | 구성 P: n0 공개(뭄바이로 오기), 구성 R: 서울 | 베타: n0 공개(싱가포르). 공개 출시 전 서울 자체 릴레이로 교체 | M2, C2, D3 |
| MLS 멤버 변경 | proposal 브로드캐스트 + 지정 커미터 | 변경 요청은 애플리케이션 메시지, 커미터가 by-value로 커밋. 커미터는 메일박스 기기 우선, 없으면 큐 서버 시퀀서 | B2 |
| 지연·중복 수신 | 다루지 않음 | 암호문 해시로 중복 제거, 과거 epoch·순서 허용 범위 확대, 그룹 상태는 앱 프로세스만 수정 | B3 |
| 키 계층 | 기기별 시드에서 IdentityKey 파생(모순) | 복구 문구 → IdentityKey(사용자), 기기별 시드 → 전송·installation 키. 기기 연결·해제·복구 절차 추가 | B4, D4 |
| 정렬·동기화 | (epoch, generation) | 대화별 Lamport 시계, 메일박스 로컬 일련번호 | B5 |
| 패딩 | 봉투에서, 1K/4K/16K/64K | MLS 암호화 전 평문에서. 푸시 버킷은 MLS 암호문 2.9 KB 이하 | M4 |
| iOS | NSE 메모리 "실측 필요" | 설계 기준 15 MB, NSE는 그룹 상태를 바꾸지 않음, SQLCipher 평문 헤더, entitlement 2건 | M5, M6, M9 |

---

## 1. 확정된 결정사항

| 항목 | 결정 | 근거 |
|---|---|---|
| 프레임워크 | **Flutter (UI) + Rust (코어)** via `flutter_rust_bridge` | 실측 RSS 83 MB vs Tauri 348 MB. 3플랫폼 1급 지원 |
| 전송 | **iroh 1.x (QUIC)** | 릴레이를 포함하면 연결은 사실상 항상 성립한다. 직접 경로 비율은 제3자 실측 70% ± 7.1%(릴레이 없음), iroh 주장 ~90%(방법론 비공개). 한국 모바일망 값은 스파이크 A에서 잰다 |
| 그룹 암호 | **MLS (RFC 9420) / OpenMLS** | 처음부터 그룹 포함. TreeKEM O(log N). 운용 규칙은 6절 |
| 오프라인 전달 | 필수 | 사용자 결정 |
| **오프라인 전달 방식** | **메일박스 기기가 있으면 서버 없이, 없으면 서버 큐 자동 사용** | 사용자 결정 2026-10-04 (리뷰 D1) — 2절 |
| 푸시 알림 | 필요. **iOS는 APNs, Android는 FCM, 둘 다 푸시 프록시 경유.** UnifiedPush는 선택 | 사용자 결정 2026-10-04 (리뷰 D2) — 8절 |
| 릴레이 | **베타는 n0 공개 릴레이, 공개 출시 전 서울 자체 릴레이로 교체** | 사용자 결정 2026-10-04 (리뷰 D3) — 5.2절 |
| 계정 복구 | **복구 문구** | 사용자 결정 2026-10-04 (리뷰 D4) — 4절 |
| iOS 배포 | TestFlight / 사이드로드 우선. 둘 다 유료 Apple Developer Program 서명이 필요하다(무료 계정 서명은 푸시 불가) | 사용자 결정 + 리뷰 M9 |
| iOS 빌드 | **클라우드 CI(macOS) + TestFlight** | 사용자 결정 2026-10-04 — Mac 없음 |
| 푸시 프록시 위치 | **큐 서버와 같은 서울 VPS** | 사용자 결정 2026-10-04 — 큐 서버 때문에 VPS가 어차피 필요하고, Workers의 문서화되지 않은 HTTP/2 동작을 피한다 |
| VPN 익스텐션 무서버 모드 | 채택 안 함 | 상시 소켓은 목적이 아니라 수단 |

### 1.1 요구사항 #2의 확정 해석

> "채팅 기록은 채팅 한 사람들만 가지고 있는다"
> → **대화 내용과 그 영구 기록을 보유하는 주체는 오직 참여 단말이다.**

- 영구 기록은 언제나 참여 단말에만 있다.
- 메일박스 기기가 있는 사용자에게는 우리 서버가 메시지를 저장하지 않는다.
- 메일박스 기기가 없는 사용자에게는 우리 큐 서버가 암호문을 24–48시간 보관한다. 평문에는 접근할 수 없다.
- 기기가 꺼져 있는 동안에는 푸시 서비스가 암호문을 잠시 보관한다. APNs는 앱당 1개를 최대 30일, FCM은 기기당 100개를 최대 28일 보관한다. 우리 서버가 아니며 평문에 접근할 수 없다.

---

## 2. 서버 의존도 — 설계의 중심 축

rev.2는 서버 의존도를 앱 전체의 설정(구성 P/R)으로 두었다. rev.3는 **받는 사람마다** 자동으로 정한다. 코드베이스와 프로토콜은 하나다.

### 2.1 우리가 운영하는 것

| 서버 | 하는 일 | 저장 | 누가 쓰나 |
|---|---|---|---|
| 푸시 프록시 | APNs·FCM 발송 대행 (인증키 보관) | 없음 | 푸시를 받는 모든 모바일 기기 |
| 릴레이 | 첫 연결 중개, 직접 연결이 안 될 때 중계 | 없음 | 모든 기기. 베타 동안은 n0 공개 릴레이를 쓴다 |
| 큐 서버 | 오프라인 메시지 보관, 그룹 커밋 순서 부여 | 암호문 24–48시간 | 메일박스 기기가 없는 사용자 |

푸시 프록시와 큐 서버는 베타부터 같은 서울 VPS 1대에 둔다. 공개 출시 전 릴레이를 교체할 때 같은 VPS에 올릴 수 있다.

### 2.2 메일박스 판정과 큐 자동 선택

- **메일박스 기기**: 내 기기 가운데 늘 켜져 있으면서, 내 다른 기기 대신 메시지를 받아 두고 내가 보낸 메시지를 계속 재시도하는 기기다(3절).
- **판정**: 최근 7일 실제 가동률이 기준(초기값 90%) 이상인 메일박스 기기가 있으면 "메일박스 있음"이다. 플랫폼만으로 판정하지 않는다.
- **메일박스 없음**: 클라이언트가 큐 서버에 수신 큐를 자동으로 만들고, QueueRef를 MLS 채널로 연락처에 알린다(9절).
- **메일박스 회복**: 가동률이 다시 기준을 넘으면 큐를 해제하고 QueueRef 철회를 알린다.
- **수동 설정**: 사용자는 자동 판정을 "항상 큐 사용" 또는 "큐 사용 안 함(지연 감수)"으로 덮어쓸 수 있다.
- 각 사용자는 자기 메일박스 installation을 `System { MailboxAnnounce }` 이벤트로 대화 상대에게 알린다. 커미터 선택(6.1)에 쓰인다.

### 2.3 사용자 소유 릴레이·큐 (BYO)

릴레이 URL과 큐 서버 URL은 사용자가 직접 지정할 수 있다. 파워유저는 자기 서버를 운영하고, 그 경우 "우리를 신뢰해야 하는가"라는 질문 자체가 사라진다. SimpleX 모델.

### 2.4 시스템 개요

```
Client (Windows / Android / iOS)
├─ Flutter UI (Dart) ── 화면만. 상태를 소유하지 않음
│     Command ↓   ↑ Stream<ChatEvent>
├─ Rust Core ── transport(iroh) · crypto(OpenMLS) · store(SQLCipher)
│               · sync(outbox / mailbox / queue / push)
└─ 네이티브 진입점 ── iOS NSE(Swift) · Android 푸시 수신기(Kotlin)
                     → Flutter 없이 Rust 코어 직접 호출 (12.3절)

메시지가 나가는 길
 ① 직접 P2P ─────── 상대 기기              [서버 0. 직접 연결이 안 되면 릴레이 경유]
 ② 메일박스 기기 ─── 상대(또는 내) PC·상시 Android   [서버 0]
 ③ 푸시 프록시 ───── APNs · FCM            [무상태]
 ④ 큐 서버 ──────── 서울                   [메일박스 없는 사용자만. 암호문 24–48h]

릴레이: 베타 n0 공개(싱가포르) → 공개 출시 전 서울 자체. 무상태.
```

### 2.5 경로 선택 순서

```
send(conversation, msg):
  1. 상대 기기와 직접 연결이 살아 있으면 즉시 전송                [서버 0]
  2. 상대 기기에 dial — 조회는 동시에, 릴레이로 먼저 연결한 뒤        [릴레이: 무상태]
     홀펀칭에 성공하면 직접 경로로 옮긴다 (5.1절)
  3. 실패하면 상대의 메일박스 기기에 dial                          [서버 0]
  4. 상대가 QueueRef를 알려 왔으면 큐 서버에 넣는다                [큐 서버]
  5. 그래도 못 넣었으면 내 outbox에 보관하고 재시도                [서버 0]
     (내 메일박스 기기가 있으면 outbox를 그쪽에 인계한다)
  · 1에서 직접 받지 못한 상대 기기마다 푸시를 보낸다              [푸시 프록시]
    (MLS 암호문이 푸시 버킷 안이면 푸시에 동봉한다, 7.2절)
```

**1–3에서 끝나면 우리 서버에 아무것도 남지 않는다.** 활성 대화와, 메일박스가 있는 사용자 사이의 대화가 여기에 해당한다.

---

## 3. 메일박스 기기 — 서버 없이 오프라인 전달

### 3.1 원리

MLS에서 다단말은 각 installation이 독립적인 그룹 멤버(leaf)다. 따라서 **하나의 애플리케이션 메시지를 모든 installation이 복호할 수 있다.**

```
B → [MLS 그룹] → A의 PC (온라인, 수신 O)
                └→ A의 폰 (오프라인, 수신 X)

나중에 A의 폰이 켜짐:
  A의 폰 → A의 PC 에 dial (LAN이면 즉시)
        → PC가 보관 중인 MLS 암호문을 그대로 재전송
        → 폰이 복호 (과거 epoch 허용 범위 안이면, 6.3절)
```

**받는 사람에게 메일박스 기기가 있으면, 그 사람에게 오는 오프라인 메시지는 서버 없이 해결된다.**

### 3.2 두 방향 모두 작동한다

| 상황 | 동작 |
|---|---|
| **수신**: 내가 오프라인일 때 온 메시지 | 내 메일박스가 받아 둠 → 폰이 켜지면 메일박스에서 가져옴 |
| **송신**: 상대가 오프라인일 때 내가 보냄 | 내 메일박스의 outbox가 계속 재시도 → 상대가 켜지면 전달. 상대가 큐를 쓰면 큐가 먼저다(2.5절) |

### 3.3 어떤 기기가 메일박스가 되나

| 플랫폼 | 가능 여부 | 조건 |
|---|---|---|
| Windows | 가능 | 사용자가 "PC를 켜 둠"에 동의하면, 전원 연결 중에 전원 요청(`PowerSetRequest`)을 잡아 유휴 절전을 막는다. 사용자가 직접 재우면(덮개 닫기, 전원 버튼, 시작 메뉴 → 절전) 멈춘다. Modern Standby 중에는 앱이 정지된다 |
| Android | 상시 연결 모드에서만 | 앱이 화면에 보일 때 사용자가 켠다. `remoteMessaging` 포그라운드 서비스(시간 제한 없음) + 고정 알림 + 배터리 최적화 예외. 백그라운드에서는 스스로 시작할 수 없다(충전 시작 이벤트로도 불가). 충전기에 꽂아 둔 남는 폰에 맞다 |
| iOS | 불가 | 백그라운드에서 연결을 유지할 수 없다 |

어느 플랫폼이든 판정은 실제 가동률로 한다(2.2절).

### 3.4 메일박스 프로토콜

```
MAILBOX_SYNC_REQ       { since_seq: u64 }                         // 메일박스 로컬 일련번호
MAILBOX_SYNC_RESP      { entries: Vec<(u64, StoredEnvelope)>, next_seq: u64 }
MAILBOX_OUTBOX_HANDOFF { entries: Vec<OutboxEntry> }              // 내 기기끼리 발신 큐 인계
```

- 기준점은 메일박스가 받은 순서대로 붙인 로컬 일련번호(`mailbox_store.id`)다. MLS generation은 발신자마다 따로 세므로 기준점으로 쓸 수 없다.
- 내 기기끼리는 각자의 InstallationCert(4.1절)로 상호 인증한다.
- 발견은 주소 캐시·mDNS·DHT 동시 조회다. iOS는 mDNS 대신 Bonjour를 쓴다(5.1절).
- 여러 기기가 메일박스면 전부 보관한다. 받는 쪽은 암호문 해시로 중복을 거른다(6.3절).

### 3.5 한계 — 정직하게

- **받는 사람에게 메일박스가 없으면 그 사람의 수신에는 도움이 안 된다.** 이 경우는 큐 서버가 맡는다(2.2절).
- 메일박스 기기가 꺼져 있는 동안은 도움이 안 된다. 가동률이 기준 아래로 떨어지면 자동으로 큐로 바뀐다.
- 메일박스는 내 메시지 암호문을 보관하므로, 그 기기를 잃으면 4.5절의 해제 절차를 밟는다.

### 3.6 부수 효과

MLS의 forward secrecy 때문에 **새 기기는 과거 메시지를 복호할 수 없다.** 기기를 연결할 때(4.5절) 기존 기기가 평문 기록을 직접 넘겨 이 문제를 해결한다(사용자 확인 후, 별도 대칭키로 암호화).

---

## 4. 아이덴티티 및 키 아키텍처

### 4.1 키 계층

```
RecoverySeed   256비트. 복구 문구(BIP-39 24단어)로 한 번 보여 주고 저장하지 않는다
  └─► IdentityKey   Ed25519   사용자. 이것이 곧 사용자다
        │   └─ PeerId = base32(pubkey)
        └─ 서명 → InstallationCert { peer_id, installation_pub, transport_pub, kind, created_at }

DeviceSeed     32B, 기기별, OS 키저장소 (Keychain / Keystore / DPAPI)
  ├─► TransportKey     Ed25519   iroh EndpointId (기기별)
  └─► InstallationKey  Ed25519   MLS leaf 서명키 (기기별)

QueueKeys      Ed25519   큐마다 새로 생성 (파생하지 않음, 9절)
PushHandle     불투명    기기·대화별로 발급 (8절)

[선택 · 나중] WalletKey  secp256k1  절대 위 계층에서 파생하지 않는다
```

- IdentityKey 개인키는 사용자의 모든 기기가 OS 키저장소에 갖는다(Signal과 같은 모델). 새 기기를 연결하고 기기 목록에 서명하기 위해서다. 대가로, 잠금 해제된 기기를 빼앗기면 아이덴티티도 빼앗긴 것으로 본다(16절).
- MLS credential은 BasicCredential에 InstallationCert를 담는다. 모든 멤버는 leaf 서명키가 cert의 `installation_pub`과 같은지, cert 서명이 PeerId로 검증되는지 확인한다.
- rev.2는 기기별 `DeviceSeed`에서 IdentityKey를 파생했다. 그러면 기기마다 사용자 키가 달라지고, 시드를 복사해 맞추면 TransportKey까지 같아진다. rev.3는 사용자 키와 기기 키의 출처를 나눈다.

### 4.2 원칙

1. **PeerId = IdentityKey 공개키.** 서버가 발급하는 계정 ID는 존재하지 않는다. 나중에 온체인 레지스트리를 얹을 수 있는 전제다.
2. **기기는 InstallationCert로 사용자에 묶인다.** 기기 추가 = MLS 그룹에 installation 추가.
3. **지갑 키를 절대 파생하지 않는다.** 서명 바인딩만 한다(13.5절).

### 4.3 기기 목록 게시 (IdentityRecord)

- 연락처가 PeerId만으로 내 현재 기기를 찾을 수 있도록, IdentityKey로 서명한 pkarr 레코드에 현재 기기의 EndpointId 목록을 게시한다(Mainline DHT).
- pkarr 레코드 크기 상한(1,000 B) 때문에 기기 수를 제한한다(초기 5대).
- 기기를 해제하면 목록에서 뺀다.
- 레코드는 공개다. PeerId를 아는 사람은 내 기기 수와 각 기기의 홈 릴레이를 볼 수 있다. IP는 게시하지 않는다(10절).

### 4.4 연락처 추가 — 서버 없이

```
Invite {
  version: 2
  peer_id:       [u8;32]        // IdentityKey 공개키
  endpoint_hint: EndpointAddr    // iroh 주소 힌트 (만료 가능)
  key_package:   MlsKeyPackage   // last-resort로 표시
  push_handle:   Option<PushHandle>
  display_name:  Option<String>  // 선택. UTF-8 64바이트 이하
  expires_at:    u64
  sig:           [u8;64]
}
```

- QR / 딥링크 / 복붙으로 전달한다. 이후 **안전번호**(양쪽 IdentityKey 해시) 대조로 MITM을 검증한다.
- `display_name`은 보내는 사람이 정한 표시 이름이다. 받는 쪽은 연락처를 추가할 때 이 값을 기본값으로 보여 주고 바꿀 수 있게 한다. 이름은 받는 쪽의 `contact.display_name`에만 저장한다(2026-10-04 UI 목업 결정).
- KeyPackage는 원칙적으로 1회용인데 초대장은 여러 사람이 쓸 수 있다. 그래서 초대장의 KeyPackage는 last-resort로 표시하고, 첫 연결 때 상대의 기기마다 새 KeyPackage를 받는다.

### 4.5 기기 연결·해제·복구

**연결 (기존 기기가 있을 때)**

1. 새 기기가 DeviceSeed를 만들고 QR에 `{transport_pub, installation_pub, 1회용 비밀}`을 띄운다.
2. 기존 기기가 QR을 찍고 iroh로 연결해 1회용 비밀로 상호 확인한다.
3. 기존 기기가 IdentityKey 개인키, InstallationCert(서명), 연락처·대화 목록을 넘긴다. 사용자가 원하면 대화 기록도 넘긴다(3.6절).
4. 기존 기기가 IdentityRecord를 갱신하고, 각 대화의 커미터에게 새 installation 추가를 요청한다(6.1절).

**연결 방향 — 카메라가 있는 쪽이 찍는다** (2026-10-04 UI 목업 결정)

- 기본 방향은 위 1–2단계다. 새 기기가 QR을 띄우고 기존 기기가 찍는다.
- 남은 기존 기기에 카메라가 없으면(카메라 없는 PC) 반대로 한다. 기존 기기가 `{transport_pub, 1회용 비밀}`을 QR로 띄우고 새 기기가 찍어서 연결한다.
- 두 방향 모두 두 화면에 확인 코드를 보여 준다. 확인 코드는 양쪽 공개키와 1회용 비밀에서 파생한다.
- 반대 방향에서는 QR 화면을 누가 몰래 찍으면 그 사람이 먼저 연결할 수 있다. 그래서 확인 코드 대조와 기존 기기의 승인을 거쳐야만 3단계(키 전달)로 넘어간다.

**해제 (기기를 잃었을 때)**

1. 남은 기기에서 해제한다. IdentityRecord에서 빼고, 각 대화의 커미터에게 제거를 요청한다.
2. 제거 커밋이 처리될 때까지 잃은 기기는 새 메시지를 복호할 수 있다. 커미터가 메일박스 기기이거나 큐 서버 시퀀서이면 대개 바로 처리된다(6.1절).
3. 잃은 기기가 잠금 해제된 채 넘어갔을 수 있으면 아이덴티티 교체를 권한다. 새 복구 문구를 만들면 새 PeerId가 되고, 연락처에게는 안전번호 변경으로 보인다.

**복구 (모든 기기를 잃었을 때)**

1. 새 기기에 복구 문구를 입력해 IdentityKey를 되살린다. PeerId가 같으므로 연락처는 같은 사람으로 인식한다.
2. 새 기기를 IdentityRecord에 게시한다. 연락처의 클라이언트는 새 기기를 발견하면 "새 기기" 안내를 띄우고, 해당 대화의 커미터에게 추가를 요청한다.
3. **복구되지 않는 것**: 대화 기록과 내 연락처 목록. 연락처 목록은 상대가 다시 연락해 올 때 채워진다.

---

## 5. 전송 계층 (iroh 1.x)

### 5.1 주소 조회와 연결

- iroh는 등록된 조회 수단을 **동시에** 실행하고 결과를 합친다. 순서대로가 아니다.
  - 주소 캐시
  - mDNS (Windows·Android. iOS는 아래 참고)
  - Mainline DHT (`iroh-mainline-address-lookup`)
  - [옵션] 자체 DNS 조회 (배치 시 <10 ms)
- n0 preset은 `dns.iroh.link` 조회까지 켠다. 최소 구성(Minimal)에서 직접 조립하고 `dns.iroh.link`는 쓰지 않는다(실측 258 ms, 독일).
- pkarr 레코드에는 기본값대로 **홈 릴레이 URL만** 게시한다(`AddrFilter::relay_only`). IP를 공개 DHT에 올리지 않는다.
- 연결은 먼저 릴레이를 거쳐 성립하고, 홀펀칭에 성공하면 직접 경로로 올라간다. 그래서 릴레이 거리는 모든 새 연결의 성립 시간에 붙는다. 스파이크 A 도구로 같은 PC에서 잰 값은 싱가포르 릴레이 경유 연결 162–169 ms, 직접 경로 전환까지 344–356 ms였다(릴레이 URL만으로 dial, 실제 조회 방식과 같음).

**iOS 주의**

- iroh의 mDNS는 raw multicast UDP라 `com.apple.developer.networking.multicast` entitlement(Apple 승인)가 필요하다. iOS에서는 iroh mDNS를 끄고, 같은 LAN의 메일박스 발견은 Bonjour(NWBrowser) 네이티브 구현이나 캐시·DHT로 한다. entitlement는 병행해서 신청한다.
- mDNS를 꺼도 Local Network 권한 팝업이 뜬다(iroh #3474). `NSLocalNetworkUsageDescription` 문구를 준비한다.
- iroh 기본 feature `fast-apple-datapath`는 Apple 비공개 API를 쓴다. 끈 빌드와 성능을 비교한 뒤 정한다.

**Android 주의**

- mDNS는 `WifiManager.MulticastLock`과 `CHANGE_WIFI_MULTICAST_STATE`가 필요하다. Android 16부터 백그라운드 앱의 multicast lock은 동작하지 않으므로 포그라운드 서비스 안에서 잡는다.
- Android 17(API 37)을 타깃으로 하면 `ACCESS_LOCAL_NETWORK` 런타임 권한이 LAN 연결 전체에 필요하다.
- iroh는 Android에서 시스템 DNS 설정을 JNI로 읽는다. 앱은 `JNI_OnLoad`에서 `iroh::dns::install_android_jni_context`를 호출해야 한다(스파이크 A 도구 작성 중 확인).

### 5.2 릴레이

| 단계 | 릴레이 | 릴레이를 거친 A↔B 왕복 |
|---|---|---|
| 개발·베타 | n0 공개 릴레이 (아시아 최근접: 싱가포르 `aps1`) | ~146 ms |
| 공개 출시 전 교체 | 서울 자체 iroh-relay | ~9 ms |

- n0 공개 릴레이는 공식적으로 개발·취미용이고 SLA가 없다. **공개 출시 전에 반드시 교체한다.** 베타 중 rate limit에 걸리면 앞당긴다.
- n0 릴레이는 최소 구성에 `RelayMode::Default`만 켜서 쓴다(5.1절의 `dns.iroh.link` 제외 조건 유지).
- 교체는 릴레이 맵 설정만 바꾸면 된다. 프로토콜과 앱 코드는 그대로다. 릴레이 맵을 앱에 넣어 두면 교체 시 앱 업데이트가 필요하고, 원격 설정으로 받으면 업데이트 없이 바뀐다.
- 자체 릴레이는 무상태다. 애플리케이션 데이터를 저장하지 않고, IP·시각·바이트 수만 본다.
- 자체 iroh-relay는 기본값이 `enable_quic_addr_discovery = false`다. 이 기능을 켜고(TLS 필요) UDP 7842를 열어야 직접 연결 비율이 떨어지지 않는다.

---

## 6. MLS 그룹 계층

### 6.1 멤버 변경 — 변경 요청과 커미터

MLS는 Commit의 전역 순서를 요구한다. 또 RFC 9420 §12.4는 유효한 proposal을 관찰한 멤버가 애플리케이션 데이터를 보내기 전에 Commit을 보내도록 요구한다(MUST). OpenMLS `create_message`도 대기 중인 proposal이 있으면 `PendingProposal`로 실패한다. 그래서 **proposal을 그룹에 보내지 않는다.**

```
- 변경 요청 (멤버·기기 추가/제거, 그룹 나가기)
    → 애플리케이션 메시지 System { ChangeRequest } 로 그룹에 보낸다. 누구나 보낼 수 있다
    → 다른 멤버의 proposal 저장소는 늘 비어 있으므로 대화가 막히지 않는다
- Commit
    → 커미터만 발행한다. 요청 내용을 by-value proposal로 담는다
    → 참조형(by-reference) proposal은 쓰지 않는다 (받는 쪽에서 StageCommitError::MissingProposal)
```

**커미터**

| 그룹 상태 | 커미터 |
|---|---|
| 멤버 중 메일박스 기기가 있다 | 알려진 메일박스 installation 중 EndpointId가 가장 작은 것 (결정적) |
| 메일박스 기기가 하나도 없다 | 큐 서버 시퀀서. 누구나 커밋하고 서버가 순서를 매긴다(`COMMIT_APPEND`) |

- 메일박스가 없는 멤버는 모두 큐 서버를 쓰므로(2.2절) 두 번째 경우에도 시퀀서가 항상 있다. 시퀀서 서버는 그룹을 만들 때 정해 그룹 컨텍스트에 기록한다.
- 커미터가 24시간 넘게 응답하지 않으면 다음 순위 메일박스 installation이나 시퀀서로 넘긴다.
- 같은 epoch에 커밋이 둘 생기면 `hash(commit)`이 작은 쪽을 채택한다. 진 쪽을 적용한 멤버가 되돌릴 수 있도록 커밋 직전 상태를 다음 커밋까지 보관한다. 상세 절차는 구현 계획에서 정한다.
- **키 갱신(Update)은 예외다.** 본인 leaf의 새 키가 필요하므로 커미터가 대신 만들 수 없고, 본인이 Update proposal을 보내야 한다. 그 사이 온라인 멤버의 송신이 잠시 막히므로, 커미터와 직접 연결된 상태에서만 보내고 커미터가 즉시 커밋한다. 시퀀서 그룹에서는 본인이 직접 커밋한다.
- **애플리케이션 메시지는 커미터와 무관하게 언제나 보낼 수 있다.**

### 6.2 그룹 라이프사이클

| 동작 | MLS 연산 | rev.3 |
|---|---|---|
| 그룹 생성 | `create_group` | 커미터 규칙과 시퀀서 서버를 그룹 컨텍스트에 기록 |
| 멤버 추가 | Add(by value) → Commit + Welcome | 누구나 요청, 커미터가 확정 |
| 멤버 제거·그룹 나가기 | Remove(by value) → Commit | 누구나 요청(본인 나가기 포함), 커미터가 확정 |
| 기기 추가·제거 | 위와 같음 | 4.5절 절차에서 요청 |
| 키 갱신 (PCS) | Update proposal → Commit | 본인이 proposal을 보내고 커미터가 즉시 확정 (6.1절) |
| 메시지 | `create_message` | **커미터 무관, 항상 가능** |

### 6.3 지연·중복·순서 뒤바뀜 수신

같은 암호문이 직접 연결, 메일박스, 큐, 푸시 같은 여러 경로로, 늦게, 순서 없이 올 수 있다. OpenMLS 기본값(`max_past_epochs = 0`, `out_of_order_tolerance = 5`)으로는 이것을 받아내지 못한다.

1. **복호 전에 중복을 거른다.** MLS 암호문 바이트의 SHA-256을 `processed_ct`에 기록하고, 이미 있으면 버린다. `msg_id`는 암호문 안에 있어 복호 전에는 쓸 수 없다. 보낼 때도 자기 암호문의 해시를 기록한다. OpenMLS는 자기 메시지를 복호할 수 없으므로(`OwnPrivateMessage`) 되돌아온 자기 메시지는 여기서 걸러진다.
2. **허용 범위를 키운다.** 초기값은 `max_past_epochs = 5`, `out_of_order_tolerance = 200`, `maximum_forward_distance = 1000`(기본값)이다. 실측 후 조정한다. 대가는 forward secrecy 약화다(16절).
3. **그룹 상태는 앱 프로세스만 바꾼다.** iOS NSE는 복호 결과를 저장하지 않는다(12.3절).
4. OpenMLS StorageProvider에는 트랜잭션이 없다. 그룹 하나의 메시지 처리 단위로 SQLite 트랜잭션 경계를 우리가 둔다.
5. StorageProvider의 값은 JSON이 아니라 바이너리로 직렬화한다. 스파이크 B에서 OpenMLS 기본 메모리 저장소의 JSON은 바이트를 10진수 배열로 적어 원래 크기의 6–7배였다.
6. OpenMLS는 메시지를 보내고 받을 때마다 그룹의 MessageSecrets 전체를 다시 저장한다. 과거 epoch마다 멤버 목록이 통째로 복사되므로 `max_past_epochs`가 클수록 쓰기량이 늘어난다(스파이크 B, 17절 #16). `max_past_epochs`는 이 쓰기량과 지연 수신 허용 범위를 함께 보고 정한다.

### 6.4 사이퍼수트

`MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519` (필수 구현 사이퍼수트).

포스트퀀텀(X-Wing 등)은 채택하지 않는다. OpenMLS에서 임시 코드 포인트를 쓰는 실험 기능이다. 기존 그룹의 사이퍼수트는 바꿀 수 없으므로, 나중에 바꿀 때는 새 그룹부터 적용하고 기존 그룹은 ReInit(그룹 재생성)으로 옮긴다.

### 6.5 확장 훅

커밋 순서는 나중에 **온체인 컨트랙트로 교체할 수 있다.** 순서 합의는 블록체인이 잘하는 일이고 커밋은 작고 드물다. XMTP의 `GroupMessages` 컨트랙트가 정확히 이 방식이다(13.3절).

---

## 7. 메시지 포맷

### 7.1 전송 봉투

```rust
struct Envelope {
    version: u8,        // 2
    target:  Target,    // Installation(EndpointId) | Queue([u8;32])
    payload: Vec<u8>,   // MLS PrivateMessage. 패딩은 이미 안쪽에 들어 있다
}
```

- 패딩은 봉투가 아니라 MLS 암호화 **전** 평문에서 한다(7.2절). 봉투나 푸시 페이로드의 길이가 원래 크기를 드러내지 않게 하기 위해서다.
- 중복 판정 키는 `SHA-256(payload)`다(6.3절).

### 7.2 애플리케이션 페이로드 (MLS 안, 참여자만 복호)

```rust
struct ChatMessage {
    msg_id:    [u8; 16],
    lamport:   u64,          // 대화별 논리 시계. 정렬 기준 (7.3절)
    prev_hash: [u8; 32],     // 보낸 installation별 로컬 해시체인 — 13.4절 앵커링 대비
    sent_at:   u64,          // 송신자 로컬 ms. 표시용. 정렬에 쓰지 않음
    content:   Content,
    pad:       Vec<u8>,      // 크기 버킷까지 채움 (AEAD 안쪽)
}

enum Content {
    Text        { body: String, mentions: Vec<PeerId> },
    Attachment  { blob_id, key, mime, size, thumbnail },
    Reaction    { target, emoji },
    Edit        { target, body },
    Delete      { target },
    Receipt     { upto, kind: Delivered | Read },
    Typing      { active },              // 직접 연결에서만 전송
    System      { event: SystemEvent },  // ChangeRequest, MailboxAnnounce, QueueRef 등
    Custom      { kind: String, data: Vec<u8> },   // ← 확장 지점
}
```

**크기 버킷**: 1 KiB / 푸시 버킷 / 16 KiB / 60 KiB.

- 푸시 버킷은 완성된 MLS 암호문이 2,900 B 이하가 되는 크기다. base64로 바꾸고 `aps` 필드를 더해도 APNs·FCM의 4,096 B 안에 들어간다. 정확한 값은 스파이크 C에서 확정한다.
- 60 KiB는 큐 서버 상한(64 KiB) 안에 들어가게 잡았다.

**확장 규칙**: variant가 추가되면 구버전은 `Custom`으로 폴백해 "지원하지 않는 메시지"로 표시한다. 파싱 실패로 대화가 깨지지 않는다.

**수정·삭제 권한**: `Edit`·`Delete`는 대상 메시지를 보낸 사람(PeerId)의 기기에서 온 것만 반영한다.

### 7.3 순서와 시각

- 정렬 기준: `(lamport, 보낸 installation, msg_id)`.
- Lamport 규칙: 보낼 때 `lamport = 지금까지 본 최댓값 + 1`, 받으면 최댓값을 갱신한다. 비정상적으로 큰 점프는 거부한다.
- `sent_at`은 송신자가 조작할 수 있으므로 표시용으로만 쓴다.
- `(epoch, generation)`은 정렬에 쓰지 않는다. generation은 보낸 사람마다 따로 세고 epoch마다 0부터 다시 센다(RFC 9420 §9.1).
- `prev_hash`는 installation별 체인이다. 수신자가 갭을 감지하면 메일박스·큐·상대에게 다시 요청한다.

### 7.4 첨부파일

- 랜덤 키로 AES-256-GCM 암호화한다. 키는 `Content::Attachment.key`로 MLS 안에서 전달한다.
- 양쪽이 온라인이면 iroh 직접 스트림으로 보낸다.
- 받는 사람에게 메일박스가 있으면 그 메일박스로 보낸다.
- 받는 사람이 큐를 쓰면 큐 서버 blob 저장소에 TTL로 올린다. 서버는 키를 모른다.
- 그 밖에는 보낸 기기가 보관하고 재시도한다(내 메일박스로 인계 가능).

---

## 8. 푸시 알림

### 8.1 공통 — 무상태 푸시 프록시

```
PushHandle = base64( AEAD_encrypt(proxy_key, {
    platform,            // apns | fcm
    token,               // APNs 디바이스 토큰 또는 FCM 등록 토큰
    conversation_tag,    // 대화별 구분 (차단용)
    issued_at, expires_at
}) )
```

- 받는 기기가 대화마다 handle을 발급해 MLS 채널로 보낸다. 기기마다 따로 발급하므로 푸시 팬아웃은 받는 기기 수다.
- 보내는 쪽은 `POST /push { handles: [...], payload }` 한 요청에 여러 handle을 묶는다.
- 프록시는 handle을 복호해 APNs·FCM으로 전달하고 **아무것도 저장하지 않는다.**
- handle 만료는 7일이다. 받는 기기는 만료 전에, 그리고 토큰이 바뀔 때 새 handle을 보낸다. APNs·FCM이 토큰 무효를 알리면 프록시가 보낸 쪽에 알려 그 handle을 버리게 한다.
- 남용 방지: 무상태라 개별 handle을 즉시 폐기할 수 없다. 짧은 만료와 handle별 rate limit으로 제한한다. 프록시 인스턴스가 하나이므로 프로세스 메모리 카운터로 정확히 셀 수 있다. 카운터는 재시작하면 사라져도 되는 값이라 "무상태" 원칙에 어긋나지 않는다.
- **프록시가 아는 것**: 보낸 사람 IP, 받는 기기 토큰, 대화 태그(같은 대화끼리 묶을 수 있음), 시각. 내용과 보낸 사람의 PeerId는 모른다.

**호스팅**: 큐 서버와 같은 서울 VPS에서 돈다. APNs는 HTTP/2 클라이언트로 직접 호출한다. rev.3 초안의 Cloudflare Workers 안은 APNs용 HTTP/2 발송이 문서화되지 않은 동작이라 버렸다(2026-10-04 사용자 결정).

**비용**: VPS 고정비에 포함된다. 요청 수 과금이 없다. 메시지 1통의 푸시 수는 받는 기기 수다.

### 8.2 iOS — APNs

```json
{ "aps": { "mutable-content": 1, "alert": { "loc-key": "NEW_MSG" } },
  "e": "<base64 MLS ciphertext>" }
```

- APNs 페이로드 상한은 4,096 B다. MLS 암호문이 푸시 버킷 안이면 `e`에 담고, 아니면 빼고 "새 메시지"로만 표시한다.
- 기기가 꺼져 있거나 망 밖이면 APNs는 **앱당 1개만** 보관한다(최대 30일). 그래서 iOS에서 푸시는 깨우기·미리보기용이고, 메시지 자체는 메일박스나 큐에서 받는다.
- NSE는 앱이 포그라운드여도 실행된다. 처리 순서는 12.3절이다.
- 차단한 대화의 푸시를 아예 숨기려면 `com.apple.developer.usernotifications.filtering` entitlement(Apple 승인)가 필요하다. 승인 전에는 "새 메시지"로 표시된다.

### 8.3 Android — FCM (기본)

- 같은 푸시 프록시가 FCM HTTP v1(서비스 계정)으로 보낸다.
- 높은 우선순위 데이터 메시지로 보낸다. 받은 메시지는 반드시 보이는 알림으로 이어져야 한다. 그렇지 않으면 일반 우선순위로 강등된다. 그래서 읽음·타이핑 같은 조용한 신호는 푸시로 보내지 않는다.
- 페이로드 상한은 4,096 B다. iOS와 같은 푸시 버킷을 쓴다.
- 기기가 꺼져 있으면 FCM이 **기기당 100개까지, 최대 28일** 보관한다. 넘치면 보관분을 전부 버리고 `onDeletedMessages()`가 호출된다. 이때 앱은 메일박스·큐·상대에게 다시 요청한다. 스파이크 C에서 확인했다: 100통은 전부 도착했고, 101통부터는 한 통도 오지 않고 `onDeletedMessages()`만 왔다.
- 앱이 강제 종료 상태면 Play services가 메시지를 버린다(스파이크 C: 5통 중 4통이 사라짐). 그래서 앱은 실행될 때마다 메일박스·큐에서 밀린 메시지를 받는다. FCM은 놓칠 수 있는 깨우기 신호로 다룬다.
- 수신기(`FirebaseMessagingService`, Kotlin)는 Flutter를 띄우지 않고 Rust 코어를 직접 부른다. 앱 프로세스 안에서 돌므로 바로 복호하고 저장한다. 주어지는 처리 시간은 몇 초다.

### 8.4 Android — UnifiedPush (선택)

- Google 서비스가 없는 기기용이다. 사용자가 distributor(ntfy 등)를 설치하면 켠다.
- 엔드포인트는 distributor의 서버에 있고, 그 서버가 메시지를 보관했다가 전달한다(ntfy 기본 12시간). 공개 ntfy.sh는 보내는 쪽 기준 하루 250건 한도가 있다.
- 메시지는 RFC 8291(WebPush)로 암호화해야 한다. handle에 엔드포인트 URL과 수신자의 p256dh·auth 키를 담는다. 피어가 직접 보내므로 프록시는 필요 없다. 평문 한도는 3,993 B다.

### 8.5 완화 조치

- 푸시 페이로드에 발신자 정보를 넣지 않는다.
- 발송에 0–3초 지터를 넣어 타이밍 상관을 약화한다.
- 사용자가 **푸시를 완전히 끌 수 있다.** 이 경우 프록시조차 사용하지 않는다.

### 8.6 알림 권한을 묻는 시점

- 온보딩에서 복구 문구 확인 직후에 묻는다(2026-10-04 UI 목업 결정).
- 거절하면 푸시를 끈 상태와 같다(8.5절). 앱을 열 때만 메시지를 받는다.
- 설정에서 언제든 다시 켤 수 있다.

---

## 9. 큐 서버 프로토콜 — 메일박스가 없는 사용자

SimpleX의 SMP를 참고하되 단순화한다. **큐 서버가 사용자 식별자도, 소셜 그래프도, 평문도 갖지 못하게 한다.** 큐는 받는 사람이 만들고 QueueRef를 연락처에 알린다(2.2절).

### 9.1 큐 모델

- 큐는 **단방향**이다. A↔B 대화에는 큐 2개가 존재한다(양쪽 다 큐를 쓰는 경우).
- 큐 ID는 32바이트 랜덤이다. 아이덴티티에서 파생하지 **않는다.**
- 큐마다 두 개의 Ed25519 키쌍을 둔다.
  - `senderKey` — 큐에 넣을 권한. **송신자만** 개인키를 안다.
  - `recipientKey` — 큐에서 뺄 권한. **수신자만** 개인키를 안다.
- 큐 서버는 두 **공개**키와 큐 ID만 안다.

### 9.2 명령

```
CREATE  { recipient_pub, sender_pub, ttl_secs }   → { queue_id }   sig: recipient
SEND    { queue_id, ciphertext, ts }              → { ok }         sig: sender
RECV    { queue_id, since_seq, max }              → [ {seq, ct, ts} ]  sig: recipient
ACK     { queue_id, upto_seq }                    → { ok }         sig: recipient → 즉시 삭제
SUB     { queue_id }                              → 스트림          sig: recipient
DELETE  { queue_id }                              → { ok }         sig: recipient
COMMIT_APPEND { group_queue_id, commit_ct }       → { seq }        (6.1절 시퀀서)
```

제약: `ciphertext ≤ 64 KiB`, 큐당 미수신 ≤ 256개.

### 9.3 저장 정책 — 짧게 유지한다

- 기본 TTL **24시간** (최대 48시간). ACK 수신 즉시 삭제한다.
- **저장 기간이 곧 서버의 역할 크기다.** 메일박스가 있는 사용자는 큐를 쓰지 않으므로 길게 잡을 이유가 없다.
- 큐 서버는 디스크에 평문 로그를 남기지 않는다.
- 큐가 30일간 미사용이면 자동 삭제한다.

### 9.4 큐 로테이션

대화마다 큐를 주기적으로 교체한다. 새 `QueueRef`는 **기존 MLS 채널 안에서** 전달하므로 큐 서버는 교체 사실을 알 수 없다.

---

## 10. 프라이버시 한계 — 정직한 기록

| 관찰자 | 아는 것 |
|---|---|
| 푸시 프록시 | 보낸 사람 IP, 받는 기기 토큰, 대화 태그(같은 대화끼리 묶기 가능), 시각 |
| APNs · FCM · UnifiedPush 서버 | 암호문 크기(버킷)·빈도. 기기가 꺼져 있는 동안 암호문을 잠시 보관 (APNs 앱당 1개·30일, FCM 기기당 100개·28일, ntfy 12시간) |
| 릴레이 | 양쪽 IP, 시각, 바이트 수 (저장 없음). 베타 동안은 n0가 운영 |
| 큐 서버 (메일박스 없는 사용자만) | 큐별 트래픽 타이밍·크기, 큐↔기기 매핑(푸시 등록 시), 클라이언트 IP |
| DHT 관찰자 | EndpointId별 홈 릴레이 URL, IdentityRecord의 기기 수 (IP는 게시하지 않음) |
| 연결한 상대 | 서로의 IP (P2P에 내재) |
| 네트워크 관찰자 | 트래픽 존재·타이밍 |

- **메일박스가 있는 사용자의 프라이버시가 더 강하다.** 큐 서버라는 상시 관찰 지점을 거치지 않기 때문이다.
- 메시지 크기는 MLS 안쪽 패딩 덕분에 버킷 단위까지만 드러난다.
- **명시적으로 범위 밖**: 글로벌 수동 관찰자에 대한 익명성, 트래픽 분석 저항, 발신자 익명성(sealed sender). 푸시 프록시가 보는 보낸 사람 IP를 숨기는 일도 지금은 범위 밖이다.

---

## 11. 로컬 저장

### 11.1 엔진

SQLite + **SQLCipher** (`rusqlite`, bundled-sqlcipher). DB 키는 OS 보안 저장소에 보관한다.

| 플랫폼 | 저장소 |
|---|---|
| Windows | DPAPI |
| Android | Keystore (하드웨어 지원 시 StrongBox) |
| iOS | Keychain (`kSecAttrAccessibleAfterFirstUnlock`, 공유 그룹) |

**iOS App Group**

- NSE가 읽을 수 있도록 메인 DB를 App Group 컨테이너에 둔다.
- 앱이 suspend 중에 파일 잠금을 쥐고 있으면 iOS가 종료시킨다(0xdead10cc). iOS는 평문 헤더를 보고 WAL 모드 SQLite를 알아보고 예외로 처리하는데, SQLCipher는 헤더까지 암호화하므로 이 예외를 받지 못한다. 그래서 `PRAGMA cipher_plaintext_header_size = 32`로 헤더를 평문으로 두고 salt는 Keychain에 따로 보관한다.
- 메인 DB에 쓰는 것은 앱뿐이다. NSE는 메인 DB를 읽기만 하고, 받은 암호문은 inbox 디렉터리에 메시지마다 파일 하나로 쓴다(잠금 불필요).

### 11.2 스키마 (요약)

```sql
identity(peer_id PK, identity_key_ref, created_at)       -- IdentityKey는 OS 키저장소에
installation(id PK, peer_id, endpoint_id, cert, kind[desktop|mobile],
             is_mailbox, uptime_7d, last_seen, revoked_at)
contact(peer_id PK, display_name, verified_at, safety_number, added_at)
conversation(id PK, kind[dm|group], mls_group_id, title,
             committer_installation, sequencer_url, lamport_max, created_at)
member(conversation_id, peer_id, installation_id, role, joined_epoch)
message(id PK, conversation_id, sender_peer_id, sender_installation, msg_id,
        lamport, prev_hash, sent_at, recv_at, content_json, state)
  INDEX (conversation_id, lamport, sender_installation, msg_id)
processed_ct(hash PK, seen_at)                           -- 복호 전 중복 제거 (6.3절)
outbox(id PK, conversation_id, target, payload, attempts, next_retry_at, created_at)
mailbox_store(id PK AUTOINCREMENT, conversation_id, envelope, for_installation, received_at)
push_handle(installation_id, conversation_id, handle,
            platform[apns|fcm|unifiedpush], expires_at)
queue(...)                                               -- 메일박스가 없을 때
attachment(blob_id PK, message_id, key, mime, size, local_path, state)
mls_kv(group_id, kind, key, value, PRIMARY KEY (group_id, kind, key))
                                                         -- OpenMLS StorageProvider 구현
```

### 11.3 메시지 상태 기계

```
composing → queued → sent_direct | sent_mailbox | sent_queue → delivered → read
                  └→ failed (재시도 소진)
```

`outbox`는 지수 백오프(1s, 2s, 4s … 최대 5분)로 재시도한다. 앱이 죽어도 재시작 시 이어서 처리한다. **메일박스 기기는 백오프 상한을 30초로 짧게 유지한다.**

---

## 12. Rust ↔ Dart 경계와 네이티브 진입점

### 12.1 원칙: 상태는 전부 Rust가 소유한다

Dart는 UI만 그린다. 상태를 두 곳에 두면 반드시 갈라진다.

```rust
// Dart → Rust : 명령 (모두 async)
async fn create_identity() -> Result<RecoveryPhrase>;          // 한 번만 보여 준다
async fn recover_identity(phrase: String) -> Result<PeerId>;
async fn create_link_qr() -> Result<LinkQr>;                  // 새 기기 쪽
async fn accept_link(qr: String) -> Result<()>;               // 기존 기기 쪽
async fn revoke_installation(installation_id: String) -> Result<()>;
async fn send_text(conversation_id: String, body: String) -> Result<MessageId>;
async fn create_group(title: String, members: Vec<PeerId>) -> Result<ConversationId>;
async fn request_add_member(conversation_id: String, peer: PeerId) -> Result<()>;
async fn create_invite() -> Result<InviteTicket>;
async fn accept_invite(ticket: String) -> Result<ConversationId>;
async fn load_messages(conversation_id, before: Option<MessageId>, limit: u32)
        -> Result<Vec<MessageView>>;
async fn set_push_config(cfg: PushConfig) -> Result<()>;
async fn set_relay_map(urls: Vec<String>) -> Result<()>;       // 베타 n0 → 출시 서울
async fn set_queue_mode(mode: QueueMode) -> Result<()>;        // Auto | Always | Never
async fn set_mailbox_mode(enabled: bool) -> Result<()>;

// Rust → Dart : 단일 이벤트 스트림
fn events() -> Stream<ChatEvent>;

enum ChatEvent {
    MessageReceived     { conversation_id, message: MessageView },
    MessageStateChanged { message_id, state: MessageState },
    ConversationUpdated { conversation_id },
    PeerPresence        { peer_id, online: bool, path: Direct | Mailbox | Queue },
    ConnectionState     { direct_peers: u32, mailbox_ok: bool, queue_ok: Option<bool> },
    GroupEvent          { conversation_id, event: SystemEvent },
    Error               { code: ErrorCode, detail: String },
}
```

### 12.2 규칙

1. **UI는 `events()` 한 스트림만 구독한다.** 여러 스트림은 순서 보장을 깬다.
2. **긴 목록은 Rust가 페이지 단위로 준다.** FFI 직렬화 비용이 그대로 프레임 드랍이다.
3. **에러는 `ErrorCode` enum으로 구조화한다.** 문자열 매칭 금지.
4. **블로킹 호출 금지.** tokio 런타임에서 처리하고 즉시 반환한다.
5. **로컬 에코 먼저.** `send_text`는 DB에 `queued`로 즉시 쓰고 이벤트를 먼저 흘린다.

### 12.3 네이티브 진입점 — iOS NSE, Android 푸시 수신기

iOS NSE(Swift)와 Android 푸시 수신기(Kotlin)는 Flutter 없이 Rust 코어를 직접 부른다. 이를 위해 `flutter_rust_bridge`와 별도로 작은 C ABI(또는 UniFFI) 경계를 둔다.

- Rust 코어를 `full` / `nse` 두 feature로 나눠 빌드한다. `nse`는 **MLS 복호 + 알림 문구 생성만** 담는다(iroh·DHT·UI·첨부 제외).
- NSE 메모리 설계 기준은 **15 MB**다. Apple은 한도를 문서화하지 않았고, 관측값은 15–24 MB다(Apple Intelligence 지원 기기는 더 크다). 처리 시간은 약 30초다.

**iOS NSE 처리 순서**

1. 푸시의 암호문 해시가 `processed_ct`에 있으면 알림만 띄우고 끝낸다.
2. NSE용 StorageProvider는 메인 DB에서 읽기만 하고, 쓰기는 메모리에만 한 뒤 버린다. 즉 복호는 하되 **그룹 상태를 바꾸지 않는다.**
3. 암호문을 inbox 파일로 쓰고 알림 문구를 만든다.
4. 앱은 실행될 때 inbox를 순서대로 처리한다(6.3절). 이때 비로소 그룹 상태가 바뀐다.

**Android 수신기**는 앱 프로세스 안에서 돌므로 코어를 그대로 쓴다. 복호와 저장까지 바로 한다.

---

## 13. 확장 지점 (요구사항 4 + 블록체인 대비)

전부 **trait로 추상화**하고 기본 구현을 꽂아둔다.

### 13.1 `IdentityResolver` — PeerId → 기기 → 주소

```rust
trait IdentityResolver {
    async fn devices(&self, peer: &PeerId) -> Result<Vec<EndpointId>>;   // IdentityRecord
    async fn resolve(&self, endpoint: &EndpointId) -> Result<EndpointAddr>;
    async fn publish(&self, record: &IdentityRecord) -> Result<()>;
}
```

지금: `PkarrDhtResolver`(IdentityRecord + EndpointId 레코드), `MdnsResolver`, `BonjourResolver`(iOS), `CachedResolver` → 나중: `OnChainResolver`

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

- 메일박스가 있는 그룹: `MailboxCommitterOrderer` (6.1절)
- 메일박스가 없는 그룹: `QueueSequencerOrderer`
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
    chat_pubkey:     [u8; 32],   // Ed25519 IdentityKey
    wallet_address:  [u8; 20],   // EVM
    sig_by_wallet:   Vec<u8>,    // 지갑이 chat_pubkey를 서명
    sig_by_identity: [u8; 64],   // chat key가 wallet_address를 서명
}
```

양방향 서명이라 한쪽만으로는 사칭할 수 없다.

### 13.6 블록체인 확정 원칙

1. **메시지는 절대 온체인에 올리지 않는다.** 요구사항 #2와 충돌하고 forward secrecy를 파괴한다.
2. **온체인 작업은 메시지 hot path에 넣지 않는다.** 초~분 단위다.
3. **블록체인은 서버를 없애지 않는다.** 서버를 여러 운영자에게 나누고 순서·보상을 합의할 뿐이다. iOS 푸시에 인증키를 가진 서버가 필요하다는 점도 그대로다.
4. **App Store**: 지갑 기능은 Apple이 **Organization 계정**을 요구한다. TestFlight 단계에서는 무관하나 정식 배포 시 전환이 필요하다.
5. 법률 판단(특금법/MiCA)은 이 문서 범위 밖이며 기능 확정 시 별도 자문이 필요하다.

---

## 14. 성능 예산

목표값이다. 근거가 실측인지 추정인지 표시했다.

| 구간 | 목표 | 근거 |
|---|---|---|
| 입력 → 화면 표시 (로컬 에코) | **< 16 ms** | 네트워크를 기다리지 않음 |
| MLS 암·복호 (1 KiB) | < 1 ms | 추정 (AEAD + 키 파생) |
| 직접 P2P 편도 (국내) | < 20 ms | 실측 서울 RTT 4.4 ms (유선) |
| 메일박스 경유 (LAN) | < 5 ms | 추정 |
| 릴레이 경유 편도 — 베타 (n0 싱가포르) | ~73 ms | 실측 기반 |
| 릴레이 경유 편도 — 출시 (서울) | ~5 ms | 실측 기반 |
| 콜드 연결 (캐시 히트) | < 300 ms | 추정. 첫 연결은 릴레이를 거치므로 릴레이 거리에 좌우된다 |
| 콜드 연결 (DHT 조회) | < 1.5 s | 추정. DHT 지연이 지배 |
| 앱 콜드스타트 → 대화 목록 (Windows) | < 600 ms | 실측 Flutter 328 ms(Windows) + DB |
| 앱 콜드스타트 → 대화 목록 (모바일) | 스파이크에서 정함 | Android 중급기·iPhone에서 측정 |
| 유휴 RSS (Windows) | < 150 MB | 실측 Hello World 83 MB + 코어/DB |
| iOS NSE 메모리 | < 15 MB | 관측값 15–24 MB (Apple 미문서화). MLS 로드+복호 피크는 Windows 근사치로 200 leaf에서 4.0 MiB 이하 |
| 푸시 발송 → 알림 표시 | 스파이크 C에서 정함 | — |

### 14.1 반응성 전략

1. **로컬 에코 우선** — 전송은 낙관적으로 표시하고 상태만 갱신한다.
2. **연결 예열** — 포그라운드 진입 시 최근 대화 상대와 메일박스에 미리 dial한다.
3. **주소 캐시 적극 활용** — DHT 조회는 캐시 미스일 때만 한다.
4. **DB 페이지네이션** — 대화당 최근 50개만 로드한다.

---

## 15. 플랫폼별 구현 노트

| | Windows | Android | iOS |
|---|---|---|---|
| 메일박스 | 가능. 사용자 동의 시 전원 연결 중 전원 요청으로 유휴 절전 방지. 사용자가 재우면 멈춤 | 상시 연결 모드에서만. 앱이 보일 때 사용자가 켬. `remoteMessaging` FGS(시간 제한 없음) + 고정 알림 + 배터리 최적화 예외 | 불가 |
| 푸시 | 없음 (트레이 앱이 직접 받음) | FCM(기본, 프록시 경유) / UnifiedPush(선택) | APNs(프록시 경유) + NSE |
| LAN 발견 | mDNS | mDNS (MulticastLock, FGS 안에서). Android 17 타깃부터 `ACCESS_LOCAL_NETWORK` | Bonjour(NWBrowser) 또는 캐시·DHT. raw mDNS는 multicast entitlement 필요 |
| 제약 | Modern Standby 중 앱 정지. 새 기본 절전 시간 5–15분 | 백그라운드에서 FGS 시작 불가. Doze, OEM 배터리 킬러 | NSE 15 MB·30초, App Group + SQLCipher 평문 헤더, Local Network 팝업 |
| 저장 키 | DPAPI | Keystore/StrongBox | Keychain (공유 그룹) |
| 배포 | — | Play Console에 `remoteMessaging` FGS 사용 신고 | 유료 Apple Developer Program 필수. TestFlight 빌드 90일 만료, 외부 테스트 첫 빌드 App Review. TestFlight는 운영 APNs 사용 |

---

## 16. 위협 모델

| 공격자 | 할 수 있는 것 | 할 수 없는 것 |
|---|---|---|
| 푸시 프록시 | 발송 관찰(보낸 사람 IP·받는 기기·대화 태그), 발송 거부 | 평문 열람, 보낸 사람 PeerId 확인 |
| 릴레이 | IP·타이밍·바이트 수 관찰, 드롭 | 평문 열람, 위조 |
| 악의적 큐 서버 | 드롭, 순서 조작 시도, 큐↔기기 매핑, IP 관찰 | 평문 열람, 위조, 순서 조작을 들키지 않기 |
| 네트워크 관찰자 | 트래픽 존재·타이밍·버킷 크기 | 내용 (QUIC+TLS+MLS 이중) |
| 기기 탈취 (잠금 해제) | 전부. IdentityKey 포함 → 아이덴티티 사칭 | — 대응: 4.5절 해제 + 아이덴티티 교체 |
| 기기 탈취 (잠금) | — | DB (SQLCipher + OS 키저장소) |
| 분실 기기 (해제 전) | 제거 커밋 전까지 새 메시지 복호 | 제거 커밋 이후 메시지 |
| 복구 문구 유출 | 아이덴티티 완전 탈취 | — 대응: 아이덴티티 교체 |
| 과거 그룹 멤버 | 재직 중 epoch | 제거 이후 epoch (MLS PCS) |
| 악의적 커미터 | 멤버 변경 거부/지연, 원치 않는 멤버 추가 시도 | 추가를 은폐 — 모든 멤버가 Commit을 검증한다 |

**forward secrecy 트레이드오프**: 지연 수신을 받아내려고 과거 epoch 5개와 발신자별 200개 범위의 키를 보관한다(6.3절). 기기를 탈취당하면 이 범위에서 아직 처리하지 않은 메시지까지 복호될 수 있다.

---

## 17. 미결정 사항 및 리스크

| # | 항목 | 영향 | 대응 |
|---|---|---|---|
| 1 | **NSE 15 MB 안에 OpenMLS 그룹 상태가 들어가는가** | iOS 알림 내용 표시 | MLS 부분은 들어간다(스파이크 B Windows 근사치: 200 leaf에서 상태 0.6–1.4 MiB, 로드+복호 피크 1.5–4.0 MiB). Swift 런타임·SQLite 캐시를 포함한 기기 실측은 TestFlight 빌드로 한다. 실패 시 "새 메시지"만 표시 |
| 2 | VPS 한 대가 푸시 프록시와 큐 서버를 함께 맡음 | VPS 장애 시 푸시·큐 동시 중단 | 감시와 자동 재시작. 규모가 커지면 이중화 |
| 3 | **한국 모바일망 직접 연결 비율** | 릴레이 부하·지연 | **스파이크 A.** KT LTE ↔ 가정 회선은 12/12 직접 연결. LTE ↔ LTE와 SKT·LG U+는 미측정 |
| 4 | 메일박스 없는 사용자 비율 | 큐 서버 부하·비용 (rev.2에서는 구성 P의 성패였으나 이제 비용 문제) | 초기 측정 |
| 5 | 커미터 인수인계 충돌 | 그룹 멤버 변경 지연 | 결정적 해소 + 직전 상태 보관. 상세는 구현 계획 |
| 6 | 키 갱신(Update) 중 송신 일시 정지 | 짧은 송신 지연 | 커미터와 연결된 상태에서만 보내고 즉시 커밋 |
| 7 | `openmls` / iroh 바인딩 성숙도 | 의존 리스크 | `flutter_rust_bridge`와 C ABI로 직접 래핑 |
| 8 | 신규 기기의 과거 메시지 | forward secrecy로 복호 불가 | 기기 연결 시 기존 기기가 평문 기록 전송 (3.6절) |
| 9 | 그룹 규모 상한 | 푸시 팬아웃(받는 기기 수), VPS 발송 부하 | 초기 50명 |
| 10 | 첨부 크기 상한 | 미정 | 초기 25 MB |
| 11 | 안전번호 검증 UX | MITM 방어 실효성 | 별도 UX 설계 필요 |
| 12 | Apple entitlement 승인 (알림 필터링, multicast) | 차단한 대화 알림 숨기기, iOS mDNS | 신청 병행. 거절되면 현행 폴백 유지 |
| 13 | Android OEM 배터리 킬러 | 상시 연결 모드 안정성 | 사용자 안내. 가동률로 자동 판정하므로 큐로 넘어감 |
| 14 | n0 공개 릴레이 정책 | 베타 중 rate limit | 출시 전 서울로 교체. 필요하면 앞당김 |
| 15 | 복구 문구 분실·유출 | 아이덴티티 상실·탈취 | 생성 시 확인 절차. 유출 시 아이덴티티 교체 |
| 16 | OpenMLS가 메시지마다 MessageSecrets 전체를 다시 저장 | 쓰기량·배터리. 200 leaf, rev.3 설정에서 메시지당 약 437 KiB(JSON 기준) | 바이너리 직렬화(6.3절 5번). `max_past_epochs` 재조정. 그래도 크면 OpenMLS에 분할 저장을 제안하거나 직접 패치 |

---

## 18. 다음 단계

승인되면 구현 계획(마일스톤 + 검증 기준)을 별도로 작성한다. 먼저 세 스파이크를 병렬로 돌린다. 셋 다 이 설계의 큰 가정을 검증한다.

- **스파이크 A — 연결**: iroh 1:1 직결률을 한국 이동통신 3사 LTE/5G ↔ 가정 회선, LTE ↔ LTE에서 잰다. n0 싱가포르 릴레이와 서울 자체 릴레이의 연결 성립 시간을 비교한다.
  - 측정 도구는 준비됐다: [`spikes/a-connectivity`](../../../spikes/a-connectivity/README.md) (Windows·Android, iroh 1.3.0). LTE ↔ LTE는 PC를 iPhone 핫스팟에 붙여 잰다.
  - KT LTE ↔ 가정 회선 측정은 끝났다: [`spikes/a-connectivity/RESULTS.md`](../../../spikes/a-connectivity/RESULTS.md).
    - 12번 모두 직접 연결로 넘어갔다.
    - 싱가포르 릴레이를 거친 첫 연결은 0.19–0.46초, 직접 경로 전환까지는 0.5–0.77초가 걸렸다.
    - 직접 연결 RTT는 p50 46–113 ms였다.
    - KT LTE는 IPv6 전용 + NAT64(464XLAT)였고, 직접 연결은 NAT64를 거친 IPv4 경로로 이뤄졌다.
- **스파이크 B — iOS NSE**: OpenMLS 그룹(50명, 1인당 기기 2대) 상태를 NSE에서 열어 15 MB·30초 안에 복호되는지 잰다. SQLCipher 평문 헤더 설정으로 0xdead10cc가 나지 않는지 확인한다.
  - Windows 근사치는 끝났다: [`spikes/b-mls-memory/RESULTS.md`](../../../spikes/b-mls-memory/RESULTS.md). MLS 부분은 15 MB 안에 충분히 들어간다.
  - 기기 실측은 클라우드 CI로 빌드한 TestFlight 앱에서 한다. NSE가 자기 메모리(phys_footprint)를 App Group 파일에 기록하고 앱이 화면에 보여 준다.
- **스파이크 C — 푸시**: FCM은 Android 폰과 Firebase 프로젝트로, APNs는 TestFlight 앱으로 실제로 보낸다. 꺼진 기기에 여러 통을 보냈을 때 각각 몇 통이 남는지 확인하고, 푸시 버킷 크기를 확정한다. 프록시가 VPS로 옮겨 가면서 Workers 확인은 필요 없어졌다.
  - FCM 에뮬레이터 측정은 끝났다: [`spikes/c-push/RESULTS.md`](../../../spikes/c-push/RESULTS.md).
    - 지연은 p50 170 ms, p95 255 ms였다(20/20 수신).
    - 푸시 버킷 크기(데이터 3,900 B)는 정상 전달됐다.
    - 문서상 한도는 4,096 B인데, 실제로는 5,000 B부터 거절했다. 설계는 문서상 한도를 유지한다.
  - 에뮬레이터로 나머지도 쟀다.
    - Doze: 높은 우선순위는 1초 안에 도착했고, 일반 우선순위는 Doze가 끝날 때까지 미뤄졌다.
    - 꺼진 기기 보관: 100통까지만 보관하고, 101통부터는 전부 버렸다.
    - 강제 종료: 메시지가 버려졌다.
  - 실제 폰에서는 제조사 배터리 관리와 LTE 지연만 확인하면 된다.

이후 순서:

1. Rust 코어 스켈레톤 + FFI 경계(`flutter_rust_bridge`, C ABI) + Flutter 셸 (3플랫폼 빌드 통과)
2. **iroh 직접 연결 1:1** — n0 릴레이, 최소 구성. 저장도 MLS도 없이 두 기기가 실제로 연결되는지만
3. 아이덴티티: 복구 문구, InstallationCert, IdentityRecord, QR 기기 연결
4. SQLCipher 저장 + 로컬 에코 + outbox + 중복 제거
5. MLS 1:1 (변경 요청 → 커미터, 지연 수신 설정)
6. **메일박스 기기 동기화** — Windows, Android 상시 연결 모드
7. 푸시 — 프록시 → FCM, APNs + NSE
8. **큐 서버 + 메일박스 자동 판정**
9. MLS 그룹 (커미터 규칙, 시퀀서)
10. 첨부파일
11. 공개 출시 전: 서울 자체 릴레이로 교체

**6번과 8번이 오프라인 전달을 완성한다.** 메일박스가 있는 사용자는 6번에서, 없는 사용자는 8번에서 오프라인 메시지를 받는다.
