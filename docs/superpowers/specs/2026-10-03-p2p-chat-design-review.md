# P2P Chat 설계 리뷰 — rev.2

- 작성일: 2026-10-03
- 대상: [`2026-08-23-p2p-chat-design.md`](2026-08-23-p2p-chat-design.md) (rev.2, 상태 "설계 초안 — 리뷰 대기")
- 선행 문서: [`2026-08-23-p2p-chat-feasibility.md`](../../research/2026-08-23-p2p-chat-feasibility.md)
- 관점: **iOS와 Android를 둘 다 제대로 지원하는가** (2026-10-03 사용자 지시). Windows는 늘 켜진 동반기기 역할로 유지한다고 가정했다.
- 방법: 설계·분석 문서 대조, 외부 1차 자료 검증, 직접 계산. 소스 확인 기준은 iroh v1.3.0, OpenMLS v0.9.0, RFC 9420이다.
- **판정: 승인 보류.** 설계를 고쳐야 하는 결함이 5건이다. 그중 B2–B5 네 건은 플랫폼과 무관하게 구현 자체를 막는다.
- **결과**: 결정 4건(4절)을 받아 rev.3 [`2026-10-04-p2p-chat-design.md`](2026-10-04-p2p-chat-design.md)에 반영했다(2026-10-04).

---

## 0. 한 화면 요약

| # | 등급 | 문제 | 한 줄 |
|---|---|---|---|
| B1 | 차단 | 모바일 전용 사용자의 오프라인 전달 | 꺼진 기기에 대해 APNs는 알림을 앱당 1개만 보관한다. 늘 켜진 PC가 없으면 구성 P로는 "오프라인 전달 필수"를 못 지킨다 |
| B2 | 차단 | 대기 중인 제안이 메시지 송신을 막음 | RFC 9420과 OpenMLS 모두, 확정 안 된 제안이 있으면 메시지 송신을 금지한다. 커미터가 오프라인이면 대화가 멈춘다 |
| B3 | 차단 | 늦게·여러 번 오는 메시지를 못 엶 | OpenMLS 기본값은 이전 epoch 0개, 순서 뒤바뀜 5개까지만 허용한다. 같은 메시지를 두 번 열면 오류다 |
| B4 | 차단 | 키 계층 모순, 기기 연결·복구 흐름 없음 | 기기별 시드에서 "사용자 키"를 만든다. 동반기기 설계 전체가 이 위에 서 있다 |
| B5 | 차단 | 메시지 정렬·동기화 기준 오류 | generation은 보낸 사람마다 따로 센다. 서로 다른 사람의 메시지 순서를 이 번호로 정할 수 없다 |
| M1–M9 | 중요 | 푸시·릴레이·iOS NSE·권한·배포 | 2절 |
| C1–C11 | 정정 | 수치·지명·문구 | 3절 |

사용자 결정이 필요한 항목은 4절(D1–D4)에, 구현 순서 조정은 5절에 모았다.

등급 기준:

- **차단**: 이대로 구현하면 기능이 동작하지 않거나 보안이 깨진다. 설계를 고친 뒤 승인한다.
- **중요**: 설계 방향은 유지하되, 구현 계획 전에 결정하거나 보완해야 한다.
- **정정**: 사실·수치·문구 오류. 문서만 고치면 된다.

근거 표기: `[출처 1]` 같은 번호는 부록의 1차 자료다. `[계산]`은 직접 계산, `[문서]`는 설계·분석 문서끼리의 대조다.

---

## 1. 차단 결함

### B1. 늘 켜진 PC가 없는 사용자는 오프라인 전달을 받지 못한다 (특히 iOS)

**위치**: §2.1, §2.2, §3.4, §8.3, §17 #2

**문제**: 구성 P의 오프라인 전달은 세 가지에 기댄다. 늘 켜진 동반기기, 보낸 쪽 outbox, 푸시 페이로드에 담은 암호문이다. 늘 켜진 PC가 없는 사용자에게는 뒤의 둘만 남는데, 둘 다 오프라인 전달을 보장하지 못한다.

- **보낸 쪽 outbox**: iOS 앱은 백그라운드로 내려가면 곧 suspend된다(분석 문서 §3.2). 보낸 사람이 앱을 닫으면 재시도도 멈춘다.
- **APNs 푸시**: 받는 기기가 꺼져 있거나 망 밖이면 APNs는 **앱(번들 ID)당 알림 1개만** 보관한다. 대화별이 아니라 앱 전체에서 1개다. 그 사이 온 나머지 푸시는 사라진다. `apns-collapse-id`나 만료 시각으로도 이 한도는 바뀌지 않는다. [출처 1]
- **푸시에 담을 수 있는 크기**: 약 3 KB다(M4). 그보다 긴 메시지와 첨부는 처음부터 푸시로 못 보낸다.

결과적으로 **폰만 쓰는 두 사람 사이에서는, 받는 쪽 폰이 꺼져 있던 동안 온 메시지를 둘이 동시에 앱을 열 때까지 받지 못한다.** 설계도 이를 인정한다(§3.4 "온라인 윈도우가 겹쳐야 한다", §2.2 승격 조건 1). 하지만 사용자 결정은 "오프라인 전달 필수"다.

§17 #2는 "동반기기 없는 사용자 비율을 초기 사용자에게 측정하고, 높으면 R로 승격"이라며 판단을 미뤄 두었다. iOS·Android를 모두 1급으로 지원하는 앱에서 이 비율이 높다는 것은 측정 전에도 예상할 수 있다. 게다가 늘 켜져 있다고 가정한 Windows PC도 기본 절전 설정에서는 잠든다(M8).

**영향**: iOS가 가장 크다. Android는 FCM을 쓰면 꺼진 기기에도 메시지를 기기당 100개, 최대 28일까지 보관하므로 짧은 메시지는 대부분 도착한다. 다만 100개를 넘으면 보관분이 전부 버려지고, 긴 메시지와 첨부는 여전히 못 받는다. 그리고 지금 설계는 FCM이 아니라 UnifiedPush를 쓴다(M1).

**제안 (D1)**: 승격 단위를 "앱 전체 설정"에서 "받는 사람 개인"으로 바꾼다.

- 늘 켜진 동반기기가 없는 사용자의 클라이언트는 §9의 수신 큐를 스스로 만들고, QueueRef를 MLS 채널로 연락처에 알린다.
- 보내는 쪽은 상대가 QueueRef를 알려왔으면 큐를 쓰고, 아니면 지금의 구성 P대로 보낸다.
- 큐 프로토콜과 코드는 §9 그대로다. PC가 있는 사용자는 큐 없이 지금처럼 동작한다.
- 요구사항 #2는 구성 R과 같은 수준(우리 서버에는 암호문만 24–48시간)으로 지켜진다.

### B2. 대기 중인 제안(proposal)이 메시지 송신을 막는다

**위치**: §6.1, §6.2, §13.3

**문제**: 설계는 "누구나 Proposal을 발행하고 지정 커미터만 Commit한다. 애플리케이션 메시지는 커밋이 아니므로 커미터가 오프라인이어도 대화는 정상 동작한다"고 한다. 표준과 구현이 둘 다 이것을 금지한다.

- RFC 9420 §12.4: 유효한 proposal을 하나 이상 관찰한 멤버는 애플리케이션 데이터를 보내기 **전에 Commit을 보내야 한다(MUST)**. [출처 18]
- OpenMLS `create_message`는 proposal 저장소가 비어 있지 않으면 `MlsGroupStateError::PendingProposal`로 실패한다(`openmls/src/group/mls_group/application.rs` L92–94). [출처 18]

누군가 멤버 추가·기기 추가·키 갱신을 제안하는 순간부터 커미터가 커밋할 때까지, 제안을 받은 멤버 전원이 메시지를 보내지 못한다. 모바일 커미터는 대부분의 시간 동안 오프라인이므로 대화가 몇 시간에서 며칠씩 멈춘다.

**제안**:

1. **제안을 그룹에 브로드캐스트하지 않는다.** "변경 요청"을 애플리케이션 메시지(`System` 이벤트)로 커미터에게만 보낸다. 커미터는 그 내용을 **by-value proposal**로 담은 Commit을 발행한다. 다른 멤버의 proposal 저장소는 늘 비어 있으므로 송신이 막히지 않는다.
2. 참조형(by-reference) proposal은 쓰지 않는다. 다른 멤버가 그 proposal을 저장하지 않았으면 커밋 처리가 `StageCommitError::MissingProposal`로 실패한다. [출처 18]
3. 남는 문제는 **커미터 병목**이다. 멤버 변경은 여전히 커미터가 올 때까지 기다린다. **분실한 기기를 그룹에서 빼는 일도 기다려야 하고, 그동안 분실 기기는 새 메시지를 계속 복호할 수 있다.** §16 위협 모델에 이 항목이 없다. 권장 방향은 두 가지다.
   - 큐 릴레이가 있는 대화(D1)는 §6.1의 릴레이 시퀀서를 써서 아무 멤버나 커밋하게 한다.
   - 늘 켜진 PC가 있는 사용자는 그 PC를 커미터로 우선 지정한다.

### B3. 늦게, 순서 없이, 여러 번 도착하는 메시지를 기본 설정으로 열지 못한다

**위치**: §3.1, §8.3, §11.2, §12.3

**문제**: 이 설계에서는 같은 암호문이 직접 P2P, 동반기기 재전송, 푸시 페이로드(NSE) 같은 여러 경로로, 늦게, 순서 없이 도착한다. OpenMLS 기본 설정은 이것을 받아내지 못한다. [출처 19]

| 기본값 | 의미 | 이 설계에서 깨지는 장면 |
|---|---|---|
| `max_past_epochs = 0` | 커밋을 처리한 뒤에는 이전 epoch 메시지를 못 연다(`TooDistantInThePast`) | 폰이 새 커밋을 먼저 받은 뒤, 동반기기가 보관해 둔 이전 epoch 메시지를 넘겨받을 때 |
| `out_of_order_tolerance = 5` | 같은 발신자의 메시지가 5개 넘게 뒤바뀌면 못 연다 | 푸시로 최신 메시지를 먼저 받고, 앞선 메시지 여러 개를 나중에 동반기기에서 받을 때 |
| 같은 generation 재복호 금지 | 두 번째 복호는 `SecretReuseError` | NSE가 연 메시지를 앱이 직접 P2P로 또 받을 때. NSE는 앱이 포그라운드여도 실행된다 [출처 3] |

**제안**:

1. **복호 전에 중복을 거른다.** `msg_id`는 암호문 안에 있어서 복호 전에는 쓸 수 없다. 대신 MLS 암호문 바이트의 해시를 키로 "처리함" 표를 둔다. 동반기기는 암호문을 "그대로" 재전송하므로(§3.1) 경로가 달라도 바이트가 같다.
2. **허용 범위를 오프라인 기간에 맞게 키운다.** 예: `max_past_epochs` 5–10, `out_of_order_tolerance` 수백. 대가는 forward secrecy 약화다(옛 키를 더 오래 보관한다). 이 트레이드오프를 §16에 명시한다.
3. **그룹 상태를 바꾸는 프로세스는 앱 하나만 둔다.** NSE는 받은 암호문을 inbox에 넣고, 알림 문구용으로만 복호한 뒤 상태 변경을 저장하지 않는다(트랜잭션 롤백). 실제 처리는 앱이 inbox를 순서대로 소화하며 한다. OpenMLS의 StorageProvider에는 트랜잭션이 없으므로 이 경계는 우리가 SQLite 트랜잭션으로 직접 둔다. [출처 19]
4. 스키마 `mls_state(group_id PK, blob)`는 OpenMLS StorageProvider 구조와 맞지 않는다. StorageProvider는 그룹별로 tree·context·secret 등을 따로 읽고 쓰는 키-값 구조다. StorageProvider 구현용 키-값 표로 바꾼다. [출처 20]

### B4. 키 계층이 스스로 모순이고, 기기 연결·복구·분실 해제 흐름이 없다

**위치**: §4.1, §4.2, §3.3, §3.5

**문제**:

- §4.1은 **기기별** `DeviceSeed`에서 `IdentityKey`("이것이 곧 사용자다")를 파생한다. 그러면 기기마다 사용자 키가 달라진다.
- §3.3은 "내 기기끼리는 **같은 IdentityKey**로 상호 인증"이라고 한다. 둘은 동시에 성립할 수 없다.
- 시드를 기기끼리 복사해서 맞추면, 같은 시드에서 나오는 `TransportKey`(iroh EndpointId)까지 같아진다. 두 기기가 같은 주소를 갖게 되어 연결이 꼬인다.

빠진 흐름이 셋이다. 셋 다 구성 P의 핵심인 동반기기와 다단말의 전제다.

1. **기기 연결**: 새 기기가 어떻게 "나"로 인정받는가. 예를 들어 기존 기기가 QR로 새 기기의 installation 자격을 서명해 주는 절차.
2. **복구**: 서버가 없으므로, 모든 기기를 잃으면 아이덴티티·연락처·대화가 함께 사라진다.
3. **분실 기기 해제**: 남은 기기에서 분실 기기를 끊는 절차. B2의 커미터 병목과 맞물린다.

**제안**: `IdentityKey`는 사용자 단위로 한 번만 만든다. `DeviceSeed`는 `TransportKey`와 installation 키 전용으로 분리한다. MLS credential은 IdentityKey가 installation 키를 서명한 형태로 둔다. 연결·해제 절차를 §4에 추가한다. 복구 방식은 D4에서 정한다.

### B5. 메시지 정렬과 동반기기 동기화가 generation 번호를 잘못 쓴다

**위치**: §7.3, §3.3, §11.2

**문제**: 정렬 기준은 `(mls_epoch, mls_generation) → sent_at → msg_id`다. 그런데 generation은 **보낸 사람(leaf)마다 따로** 세고, epoch마다 0부터 다시 센다(RFC 9420 §9.1). 서로 다른 사람의 generation은 비교할 수 없다. [출처 21]

- 예: 같은 epoch에서 A가 메시지 10개(generation 0–9)를 보낸 뒤 B가 답장 1개(generation 0)를 보내면, B의 답장이 A의 두 번째 메시지 앞에 놓인다.
- 같은 결함이 `COMPANION_SYNC_REQ { since: (epoch, generation) per conversation }`에도 있다. 대화당 값 하나로는 여러 발신자의 수신 위치를 나타낼 수 없다.
- `prev_hash`는 "송신자별 체인"이다. 한 사람이 여러 기기에서 보내면 체인이 갈라진다.

**제안**:

- `ChatMessage`에 대화별 논리 시계(Lamport 또는 HLC)를 넣고 `(시계, 보낸 installation, msg_id)`로 정렬한다. `sent_at`은 표시용으로만 쓴다.
- 동반기기 동기화의 기준점은 동반기기 쪽 로컬 일련번호(`companion_store.id`)로 한다.
- `prev_hash` 체인은 installation 단위로 둔다.

---

## 2. 중요 — 구현 계획 전에 결정하거나 보완할 것

### M1. Android 푸시: UnifiedPush만으로는 대부분의 사용자가 백그라운드에서 메시지를 못 받는다

- UnifiedPush는 사용자가 distributor 앱을 따로 설치해야 동작한다. 설치 비율 통계는 공개된 것이 없다. 대표 distributor인 ntfy의 Google Play 설치 수가 10만+ 수준이다. distributor가 없는 사용자는 구성 P에서 푸시가 아예 없다(FCM 폴백은 구성 R로 미뤄져 있다). [출처 11]
- "피어가 직접 POST"하는 엔드포인트는 distributor의 서버(ntfy.sh 등)에 있다. 그 서버가 메시지를 보관했다가 전달한다(ntfy 기본 12시간). [출처 11]
- 공개 ntfy.sh 한도는 보내는 쪽 기준 하루 250건, 60건 버스트 후 5초당 1건이다. 그룹에서 활발한 사용자는 하루 250건을 쉽게 넘는다. [출처 11]
- UnifiedPush 규격은 RFC 8291(WebPush) 암호화를 필수로 한다. 보내는 피어는 URL뿐 아니라 수신자의 p256dh/auth 키도 알아야 한다. 평문 한도는 3,993 B다. [출처 11]
- §17 #3의 "ntfy 클라이언트 내장"도 공짜가 아니다. 앱이 백그라운드에서 ntfy 서버와 연결을 유지하려면 포그라운드 서비스가 필요하고, M7의 시작 제한을 그대로 받는다.
- FCM은 사정이 다르다. 기기가 꺼져 있어도 메시지를 **기기당 100개까지, 최대 28일** 보관한다. 넘치면 보관분을 전부 버리고 `onDeletedMessages()`를 호출한다. 페이로드는 4,096 B다. 높은 우선순위 메시지는 Doze 중에도 기기를 깨우고 몇 초의 처리 시간을 준다. 단 보이는 알림을 띄우지 않으면 일반 우선순위로 강등된다. [출처 10]
- FCM HTTP v1로 보내려면 서비스 계정(OAuth2)이 필요하므로 신뢰된 서버가 보내야 한다. iOS용 푸시 프록시가 이미 있으므로 같은 프록시가 FCM도 보내면 된다(같은 PushHandle 방식). [출처 10]
- 대안으로, FCM은 VAPID로 서명한 WebPush 요청을 서비스 계정 없이도 받는다. UnifiedPush의 embedded FCM distributor가 이 경로를 쓴다. 이러면 피어가 서버 없이 FCM에 직접 보낼 수 있다. 하지만 Google이 문서화하지 않은 동작에 기대고, 보내는 피어 모두가 수신자의 VAPID 개인키를 가져야 한다. [출처 10, 11]

**제안 (D2)**: FCM을 기본으로 하고, 발송은 iOS용 푸시 프록시가 함께 맡는다(문서화된 경로). UnifiedPush는 Google 서비스가 없는 기기를 위한 선택지로 남긴다. Android 푸시 수신기도 NSE처럼 Flutter 없이 Rust를 직접 부른다(M5 네이티브 바인딩).

### M2. 구성 P의 릴레이: n0 공개 릴레이는 실서비스용이 아니다

- n0 공개 릴레이는 공식 문서상 "개발·취미용"이다. rate limit 수치가 공개되지 않았고 SLA도 없다. [출처 16]
- 아시아 릴레이는 **싱가포르** 한 곳뿐이다(뭄바이가 아니다, C2 정정). 한국에서 릴레이까지 왕복 약 73 ms, 릴레이를 거친 A↔B 왕복 약 146 ms다.
- iroh 연결은 처음에 릴레이를 거쳐 만난 뒤 직접 경로로 올라간다. 그래서 **릴레이 거리는 폴백 때만이 아니라 모든 새 연결의 성립 시간에 붙는다.** [출처 16]
- 자체 iroh-relay는 바이너리(또는 Docker 이미지) 하나와 짧은 TOML 설정으로 돌릴 수 있다. 공식 문서상 **무상태**이고 애플리케이션 데이터를 저장하지 않는다. IP·시각·바이트 수는 본다. [출처 16]

**제안 (D3)**: 구성 P에도 서울 자체 릴레이를 둔다. 운영 서버는 둘(푸시 프록시, 릴레이)이 되지만 둘 다 아무것도 저장하지 않으므로 요구사항 #2는 그대로다. 분석 문서 §5도 원래 "서울 자체 릴레이"를 권고했다.

### M3. 푸시 프록시: 무상태 설계의 현실 점검

| 설계 내용 | 확인된 사실 | 조치 |
|---|---|---|
| Cloudflare Workers에서 APNs 발송 | APNs는 HTTP/2만 받는다. Workers 런타임(workerd)에는 HTTP/2가 없지만, 배포된 Worker는 Cloudflare 프록시가 HTTP/2로 바꿔 줘서 동작한다. **공식 문서에 없는 동작**이고 로컬 `wrangler dev`로는 안 된다 [출처 8] | 초기 스파이크에서 실제 발송 확인. 막히면 작은 VPS로 옮김 |
| 메모리 카운터로 handle별 rate limit | Worker 인스턴스(isolate)마다 변수가 따로라 전역 한도가 되지 않는다. Rate Limiting 바인딩은 2025-09 GA이지만 지역(location)별·근사치다 [출처 9] | Rate Limiting 바인딩 사용, 근사치임을 명시 |
| "수신자가 handle을 폐기"해 스팸 차단 | 무상태 프록시는 폐기 목록을 가질 수 없다. handle은 `expires_at`까지 유효하다 [문서] | 짧은 만료(예: 7일)와 주기적 재발급으로 대체하거나, 작은 폐기 목록(상태)을 허용 |
| 원치 않는 알림 숨기기 | NSE가 알림을 아예 안 띄우려면 filtering entitlement가 필요하고 Apple 승인을 받아야 한다. 없으면 내용만 바꿀 수 있다 [출처 4] | 승인 신청. 승인 전에는 차단한 상대의 푸시도 "새 메시지"로 뜬다 |
| 운영 부담 "거의 0 (무료 티어)" | 무료: 하루 10만 요청, 요청당 CPU 10 ms, 하위 요청 50개. 유료: 월 $5에 월 1천만 요청, 초과 100만당 $0.30 [출처 9] | 아래 계산. 소규모 테스트 이후에는 유료 플랜을 전제 |
| "프록시가 아는 것: 토큰에 푸시가 갔다뿐" | 프록시는 handle을 복호하므로 수신 기기 토큰과 대화 태그(같은 대화끼리 묶을 수 있음)를 본다. HTTP 요청에서 **보낸 사람의 IP**도 본다 [문서] | §8.2·§10·§16 문구 정정(C5) |

**무료 티어 계산** [계산] — 푸시는 직접 연결이 안 될 때만 나가는데, 받는 쪽이 모바일이면 대부분 이 경우다.

- 1:1만 쓰는 사용자 1,000명이 하루 30통씩 보내면 하루 3만 요청이다. 무료 한도 안이다.
- 같은 1,000명이 20명 그룹(1인당 기기 2대)에서 보내면 메시지 1통에 푸시 38건, 하루 114만 요청이다. 무료 한도의 11배다.
- 유료 플랜이면 한 달 약 3,420만 요청 → $5 + 2,420만 × $0.30/100만 ≈ **월 $12**. 싸지만 "무료"는 아니다.
- 여러 handle을 한 요청에 묶어 보내면 Workers 요청은 메시지당 1건으로 줄어든다. 다만 무료 플랜은 요청당 하위 요청(APNs 호출)이 50개까지다.

### M4. 푸시 페이로드 예산과 패딩

[계산] APNs 페이로드 한도는 JSON 전체 기준 4,096 B다. [출처 2] 설계의 JSON 틀(`aps` + `"e"`)이 66 B이고, 암호문은 base64로 4/3배가 된다.

| 패딩 버킷 | base64 후 | JSON 포함 | 4,096 B 안에 들어가나 |
|---|---|---|---|
| 1K (1,024 B) | 1,368 B | 1,434 B | 들어감 |
| 4K (4,096 B) | 5,464 B | 5,530 B | **안 들어감** |
| 들어가는 최대 원문 | — | — | 약 3,021 B (`aps` 필드를 더 넣으면 약 2.9 KB) |

- §8.3 "4 KB를 넘으면 '새 메시지'로만 표시"는 지금 버킷 구성에서는 실제로 "**1K 버킷을 넘으면**"이다. 평문 약 900 B 이하만 알림에 내용이 뜬다.
- 패딩을 봉투(`Envelope.padded_to`)에서 하면 길이 필드가 원래 크기를 드러낸다. 특히 APNs의 `"e"` 값은 MLS 암호문 길이를 그대로 보여준다. **패딩은 MLS 안에서**(OpenMLS `padding_size`, AEAD 안쪽) 해야 크기가 숨겨진다. [출처 20]
- `padded_to: u16`에는 64K(65,536)가 들어가지 않는다(u16 최대 65,535). 버킷 번호(u8)로 바꾼다.

**제안**: 버킷을 "푸시에 들어가는 최대 버킷이 약 2.9 KB 이하"가 되게 다시 잡는다(예: 1K / 2.9K / 16K / 60K). 패딩은 MLS `padding_size`로 옮긴다. FCM 데이터 메시지도 4,096 B이고 값이 문자열이라 base64가 필요하다. UnifiedPush는 바이너리로 평문 3,993 B까지다. 따라서 같은 버킷이 세 경로 모두에 맞는다. [출처 10, 11]

### M5. iOS 알림 처리기(NSE)의 현실

- **메모리**: Apple은 NSE 메모리 한도를 문서화하지 않았다. Apple DTS 답변은 24 MB(2024-07)였다. Beeper는 대부분 기기에서 15 MB, Apple Intelligence 지원 iPhone에서 150 MB를 관측했다(2025-10). Element X는 matrix-rust-sdk를 NSE에서 쓰다 한도를 넘겼다. **설계 기준을 15 MB로 잡고, §17 #1을 초기 스파이크로 앞당긴다.** [출처 3]
- **시간**: 약 30초. [출처 3]
- **DB 잠금 종료(0xdead10cc)**: suspend된 앱이나 확장이 파일·SQLite 잠금을 쥐고 있으면 iOS가 종료시킨다. iOS는 평문 헤더를 보고 WAL 모드 SQLite를 알아보고 예외로 처리한다. 그런데 SQLCipher는 헤더까지 암호화하므로 이 예외를 받지 못한다. App Group에 SQLCipher+WAL을 두려면 `PRAGMA cipher_plaintext_header_size = 32`로 헤더를 평문으로 두고 salt는 따로 보관해야 한다. GRDB는 DB를 공유하기보다 파일이나 IPC로 주고받기를 권한다. [출처 6]
  - 제안: 메인 DB는 위 SQLCipher 설정으로 App Group에 두되, **쓰기는 앱만** 한다. NSE는 메인 DB를 읽기만 하고, 받은 암호문은 별도 inbox(작은 파일이나 별도 DB)에만 쓴다. B3-3과 같은 방향이다.
- **네이티브 바인딩**: NSE(Swift)와 Android 푸시 수신기(Kotlin)는 Flutter 없이 Rust를 직접 불러야 한다. §12는 Dart↔Rust 경계만 정의한다. `nse` feature용 C ABI 또는 UniFFI 경계를 §12.3에 추가한다. iroh 1.0의 공식 Swift/Kotlin 바인딩도 참고할 수 있다. [출처 17]

### M6. iOS 네트워크 권한과 비공개 API

- iroh의 mDNS는 raw multicast UDP(swarm-discovery)다. iOS 14부터 이런 multicast에는 `com.apple.developer.networking.multicast` entitlement가 필요하고 **Apple 승인**을 받아야 한다. iroh 이슈 #3038도 아직 열려 있다. 시스템 Bonjour API(NWBrowser 등)로 서비스 타입을 등록해 쓰면 이 entitlement 없이 된다. [출처 5, 17]
- mDNS를 꺼도 Local Network 권한 팝업이 뜬다(iroh 이슈 #3474). `NSLocalNetworkUsageDescription` 문구가 필요하다. [출처 5, 17]
- iroh 기본 feature `fast-apple-datapath`는 Apple 비공개 API를 쓴다. App Store 정식 배포 때 문제가 될 수 있다. 이 부분은 검증하지 못했다. [출처 17]

**제안**: iOS 빌드는 iroh mDNS를 끄고, 같은 LAN의 동반기기 발견은 Bonjour(NWBrowser) 네이티브 구현이나 주소 캐시·DHT로 한다. multicast entitlement는 병행해서 신청한다. `fast-apple-datapath`는 끈 빌드와 성능을 비교한 뒤 정한다.

### M7. Android를 "충전 중 + Wi-Fi일 때 자동으로" 동반기기로 쓸 수는 없다

- Android 12부터 백그라운드에서는 포그라운드 서비스(FGS)를 시작할 수 없다. 충전 시작 브로드캐스트(`ACTION_POWER_CONNECTED`)는 매니페스트에 등록한 리시버로 받을 수 없고, 예외 사유도 아니다. WorkManager의 `setForeground`도 백그라운드에서는 예외를 던진다. [출처 12]
- FGS를 시작할 수 있는 계기는 다음과 같다. [출처 12]
  - 앱이 화면에 보일 때
  - 높은 우선순위 FCM을 받았을 때
  - UnifiedPush 메시지를 받았을 때(규격상 5초간 포그라운드 중요도를 받는다)
  - 사용자가 배터리 최적화 예외를 허용했을 때
- 설계가 걱정한 6시간 제한은 `dataSync` 타입 얘기다. 메시징용 `remoteMessaging` 타입(API 34+)은 시간 제한이 없다. 다만 Play Console 신고가 필요하다. [출처 12]
- Android 17(API 37)을 타깃으로 하면 `ACCESS_LOCAL_NETWORK` 런타임 권한이 필요해진다. mDNS뿐 아니라 iroh의 LAN 직접 연결 전체에 적용된다. Android 16부터는 백그라운드(cached) 앱의 multicast lock이 동작하지 않으므로 FGS 안에서 잡아야 한다. [출처 13]

**제안**: "사용자가 앱에서 켜면, 충전기를 뽑을 때까지 `remoteMessaging` FGS로 동반기기 역할"로 범위를 줄인다. 자동 판정 대상에서는 Android를 뺀다. §15 표의 `dataSync` 기준을 `remoteMessaging`으로 바꾸고, LAN 권한을 플랫폼 노트에 추가한다.

### M8. Windows PC도 기본 설정에서는 "늘 켜진 기기"가 아니다

- Modern Standby 중에는 트레이 앱을 포함한 데스크톱 앱이 정지된다. [출처 14]
- Windows 11 새 기본값은 전원 연결 상태에서 Modern Standby 기기 5분, 기존 절전(S3) 기기 15분 뒤 절전이다. 기존 설치에도 적용되는지는 문서에 없다. [출처 14]
- 앱이 전원 요청(`PowerSetRequest` 또는 `SetThreadExecutionState`)을 잡으면 전원 연결 중에는 유휴 절전을 무기한 막을 수 있다. 하지만 사용자가 직접 재우면(덮개 닫기, 전원 버튼, 시작 메뉴 → 절전) 요청이 풀린다. 배터리 사용 중 Modern Standby에서는 절전 시간 5분 뒤 끝난다. [출처 14]

**제안**: Windows 동반기기는 "전원 연결 + 전원 요청 유지(사용자 동의) + 사용자가 재우지 않음"일 때만 늘 켜져 있다. 노트북은 덮개를 닫으면 끝난다. §3.3의 자동 판정은 "데스크톱 플랫폼"이라는 조건을 빼고 실제 측정한 가동률로만 한다. 이 항목은 B1의 근거를 하나 더한다.

### M9. iOS 배포 조건

- 무료 Apple ID(개인 팀)로 서명하면 Push Notifications capability를 쓸 수 없다. **무료 계정으로 사이드로드한 빌드는 푸시를 못 받는다.** 유료 Apple Developer Program 가입이 사실상 필수다. [출처 7]
- TestFlight 빌드는 90일 뒤 만료된다. 외부 테스터에게 배포하는 첫 빌드는 App Review를 거친다. TestFlight 빌드는 운영(production) APNs 환경을 쓴다. [출처 7]

---

## 3. 정정 — 문서만 고치면 되는 것

| # | 위치 | 현재 | 수정 |
|---|---|---|---|
| C1 | 설계 §1 | "홀펀칭 + 릴레이 폴백 포함 ~90–95% 연결 성공" | 릴레이를 포함하면 연결은 사실상 항상 된다. iroh가 말하는 ~90%는 **직접 연결(홀펀칭) 비율**이고 방법론이 공개되지 않았다. 95%는 직접 경로로 흐른 **바이트 비율**이다. 제3자 대규모 실측은 70% ± 7.1%(릴레이 없음). 모바일망 수치는 직접 측정해야 한다 [출처 15] |
| C2 | 분석 §1.5·§3.1, 설계 §2.1·§5.2·§14 | n0 릴레이 "뭄바이" | **싱가포르**(aps1). 실측 72.8 ms는 AWS 싱가포르 68.5 ms와 맞는다. 지연 수치는 그대로 유효하다 [출처 16] |
| C3 | 설계 §5.1 | "dial 순서: 캐시 → mDNS → DHT → …" | iroh는 등록된 조회 수단을 **동시에** 실행하고 결과를 합친다. n0 preset은 `dns.iroh.link` 조회를 켜므로, 쓰지 않으려면 최소 구성에서 직접 조립한다. DHT 조회는 별도 크레이트(`iroh-mainline-address-lookup`)다 [출처 17] |
| C4 | 설계 §1.1 | 구성 P에서 "문자 그대로 성립 — 큐가 존재하지 않는다" | 기기가 꺼져 있는 동안 푸시 서버가 **암호문을 잠시 보관**한다. APNs는 앱당 1개를 최대 30일, FCM은 기기당 100개를 최대 28일, ntfy는 기본 12시간 보관한다. "우리 서버에는 저장하지 않는다"가 정확한 표현이다 [출처 1, 10, 11] |
| C5 | 설계 §8.2·§10·§16 | 프록시는 "송신자·내용·대화 불명" | 프록시는 보낸 사람 IP, 수신 기기 토큰, 대화 태그(같은 대화끼리 묶기 가능)를 본다. 연결한 상대끼리는 서로의 IP를 안다(P2P에 내재). 반면 pkarr 레코드는 기본적으로 릴레이 URL만 공개하므로 EndpointId만으로는 IP를 찾을 수 없다. 좋은 기본값이니 명시한다 [출처 17] |
| C6 | 설계 §14 | MLS < 1 ms "근거: 스택 오버헤드 p50 0.14 ms" 등 | 0.14 ms는 WebRTC 루프백 왕복이지 MLS 연산이 아니다. 콜드 연결의 "ICE 47 ms"도 WebRTC 값이다(iroh는 ICE를 쓰지 않는다). 콜드스타트 328 ms는 Windows 값이다. 근거를 "추정"으로 바꾸고, 모바일(Android 중급기·iPhone) 예산과 측정 항목을 추가한다 [문서] |
| C7 | 설계 §2.1 | 운영 부담 "거의 0 (무료 티어)" | M3 계산 반영: 소규모 테스트까지는 무료, 그 뒤 월 $5–15 수준 [계산] |
| C8 | 설계 §4.3 | Invite에 KeyPackage 1개 | QR·링크는 여러 사람이 쓸 수 있는데 KeyPackage는 원칙적으로 1회용이다. last-resort KeyPackage로 표시하거나 첫 연결 때 새 KeyPackage를 받는다. 상대 기기가 여럿이면 기기별 KeyPackage도 필요하다 [출처 20] |
| C9 | 설계 §11.2 | `push_handle(peer_id, conversation_id, …)` | 기기마다 토큰이 다르므로 installation 단위로 둔다. 푸시 팬아웃은 받는 사람 수가 아니라 받는 기기 수다 [문서] |
| C10 | 설계 §7.2 | `Edit` / `Delete` | 원래 보낸 사람의 기기만 수정·삭제할 수 있다는 검증 규칙을 추가한다 [문서] |
| C11 | 설계 §6.3 | "`ciphersuite` 필드만 두어 나중에 협상" | 기존 그룹의 사이퍼수트는 바꿀 수 없고 ReInit(그룹 재생성)이 필요하다. X-Wing은 OpenMLS에서 임시 코드 포인트를 쓰는 실험 기능이다 [출처 20] |

---

## 4. 결정이 필요한 항목

| # | 질문 | 선택지 | 권장 | 결정 (2026-10-04) |
|---|---|---|---|---|
| D1 | 늘 켜진 PC가 없는 사용자의 오프라인 전달 (B1) | (a) 받는 사람별 자동 큐 (b) 처음부터 구성 R (c) 구성 P 유지, 한계 수용 | (a) | (a). 메일박스 기기는 PC 또는 상시 연결 모드 Android. 메일박스가 없는 사람만 서버 큐를 쓰며, PC는 필수가 아니다 |
| D2 | Android 푸시 (M1) | (a) FCM 기본(푸시 프록시 경유) + UnifiedPush 선택 (b) 피어가 FCM에 VAPID로 직접 발송(비공식 동작) (c) UnifiedPush만(현행) | (a) | (a) |
| D3 | 구성 P의 릴레이 (M2) | (a) 서울 자체 릴레이(무상태) (b) n0 공개 릴레이 | (a) | 베타는 n0 공개 릴레이, 공개 출시 전 서울 자체 릴레이로 교체 |
| D4 | 모든 기기를 잃었을 때 복구 (B4) | (a) 복구 문구(시드 백업) (b) 암호화 백업 파일 (c) 복구 없음 | (a) | (a) |

---

## 5. 구현 순서 조정 제안 (설계 §18)

rev.2 §18은 "홀펀칭이 실제로 되는가"만 먼저 검증한다. 이번 리뷰로 iOS와 푸시에서 같은 무게의 미검증 가정이 더 드러났다. 셋을 첫 마일스톤의 스파이크로 묶는다.

1. **스파이크 A — 연결**: iroh 1:1 직결률을 한국 이동통신 3사 LTE/5G ↔ 가정 회선, LTE ↔ LTE에서 측정한다. 서울 자체 릴레이와 n0 싱가포르 릴레이의 연결 성립 시간을 비교한다.
2. **스파이크 B — iOS NSE**: OpenMLS 그룹(50명, 1인당 기기 2대) 상태를 NSE에서 열어 15 MB·30초 안에 복호되는지 잰다. SQLCipher 평문 헤더 설정으로 0xdead10cc가 나지 않는지 확인한다.
3. **스파이크 C — 푸시 경로**: Cloudflare Workers에서 APNs(HTTP/2)와 FCM으로 실제로 보내 본다. 꺼진 기기에 여러 통을 보냈을 때 APNs와 FCM에서 각각 몇 통이 남는지 확인한다.

이후 순서는 rev.2 §18과 같되, 4번(MLS 1:1) 앞에 B4의 키 계층과 기기 연결 절차를 넣는다.

---

## 부록 — 검증 출처

### iOS · Cloudflare (1–9)

- **[1]** APNs 오프라인 보관(번들 ID당 1개, 최대 30일): https://developer.apple.com/documentation/usernotifications/sending-notification-requests-to-apns — "APNs stores only one notification per bundle ID."
- **[2]** 페이로드 한도 4,096 B(VoIP 5,120 B): A1과 같은 문서
- **[3]** NSE 메모리·시간·포그라운드 실행
  - https://developer.apple.com/forums/thread/758982 (Apple DTS, "24MB and 30 seconds, as of this writing", 2024-07)
  - https://blog.beeper.com/2025/10/01/how-beeper-ios-implements-notifications/ (15 MB / 150 MB 관측)
  - https://github.com/element-hq/element-x-ios/issues/1923 (matrix-rust-sdk 한도 초과)
  - https://developer.apple.com/forums/thread/783502 (앱 실행 여부와 무관하게 NSE 실행, 2025-05)
- **[4]** 알림 억제 entitlement: https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.usernotifications.filtering
- **[5]** multicast entitlement·Local Network
  - https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.networking.multicast
  - https://developer.apple.com/documentation/technotes/tn3179-understanding-local-network-privacy
- **[6]** 0xdead10cc·SQLCipher
  - https://developer.apple.com/documentation/xcode/sigkill
  - https://github.com/groue/GRDB.swift/blob/master/GRDB/Documentation.docc/DatabaseSharing.md
  - https://github.com/sqlcipher/sqlcipher/issues/255
- **[7]** 배포
  - https://developer.apple.com/help/account/reference/supported-capabilities-ios
  - https://developer.apple.com/help/app-store-connect/test-a-beta-version/testflight-overview
  - https://developer.apple.com/documentation/bundleresources/entitlements/aps-environment
- **[8]** Workers → APNs: https://github.com/cloudflare/workerd/issues/4841, https://github.com/FiveSheepCo/cloudflare-apns2
- **[9]** Workers 한도·요금·rate limit
  - https://developers.cloudflare.com/workers/platform/limits/
  - https://developers.cloudflare.com/workers/platform/pricing/
  - https://developers.cloudflare.com/workers/runtime-apis/bindings/rate-limit/
  - https://developers.cloudflare.com/workers/reference/how-workers-works/

### Android · Windows (10–14)

- **[10]** FCM
  - https://firebase.google.com/docs/cloud-messaging/customize-messages/collapsible-message-types ("If the limit is reached, all stored messages are discarded.")
  - https://firebase.google.com/docs/cloud-messaging/customize-messages/set-message-type ("Maximum payload for both message types is 4096 bytes")
  - https://firebase.google.com/docs/cloud-messaging/android/message-priority ("Several seconds are given to process the message payload")
  - https://firebase.google.com/docs/cloud-messaging/send/v1-api (서버·신뢰 환경에서 호출)
- **[11]** UnifiedPush · ntfy
  - https://unifiedpush.org/developers/spec/definitions/
  - https://unifiedpush.org/developers/spec/android/ ("the cleartext content is at most 3993 bytes", RFC 8291 필수)
  - https://unifiedpush.org/kdoc/embedded_fcm_distributor/ ("Google FCM servers can handle webpush requests out of the box, but they require a VAPID authorization.")
  - https://docs.ntfy.sh/publish/#limitations ("On ntfy.sh, the daily message limit is 250.")
  - https://f-droid.org/2026/01/08/unifiedpush-5-years.html
- **[12]** 백그라운드 실행
  - https://developer.android.com/develop/background-work/services/fgs/restrictions-bg-start
  - https://developer.android.com/jetpack/androidx/releases/work (`setForeground` → `ForegroundServiceStartNotAllowedException`)
  - https://developer.android.com/develop/background-work/services/fgs/service-types (`remoteMessaging`)
  - https://developer.android.com/develop/background-work/services/fgs/timeout (`dataSync` 6시간)
- **[13]** mDNS · LAN 권한
  - https://developer.android.com/reference/android/net/wifi/WifiManager.MulticastLock
  - https://developer.android.com/privacy-and-security/local-network-permission (Android 17 `ACCESS_LOCAL_NETWORK`)
- **[14]** Windows
  - https://learn.microsoft.com/en-us/windows-hardware/design/device-experiences/prepare-software-for-modern-standby ("Windows prevents desktop applications from running during any part of modern standby")
  - https://support.microsoft.com/en-us/windows/experience/power-battery/power-settings-in-windows-11 (새 기본 절전 시간)
  - https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-powersetrequest ("power requests are terminated upon user-initiated system sleep entry")

### iroh · OpenMLS · RFC 9420 (15–21)

소스 기준: n0-computer/iroh v1.3.0 (0072d7d), openmls/openmls openmls-v0.9.0 (3a3e35d).

- **[15]** iroh 수치: https://docs.iroh.computer/about/faq ("Hole-punching works roughly 9 out of 10 times."), https://www.iroh.computer/blog/v1 (1.0 출시 2026-06-15)
- **[16]** 릴레이
  - https://docs.iroh.computer/iroh-services/relays/public ("suitable for development and hobby use only")
  - https://docs.iroh.computer/concepts/relays ("relay servers are stateless")
  - `iroh/src/defaults.rs` L33 (`aps1-1.relay.n0.iroh.link`, 싱가포르)
- **[17]** 디스커버리·모바일
  - `iroh/src/endpoint/presets.rs` L125–136 (n0 preset의 `dns.iroh.link` 조회)
  - `iroh/src/address_lookup/pkarr.rs` L207–211 (기본 `AddrFilter::relay_only`)
  - https://docs.iroh.computer/connecting/dht-address-lookup ("not enabled by default")
  - https://github.com/n0-computer/iroh/issues/3038 (iOS multicast), #3474 (Local Network 팝업)
- **[18]** 대기 중인 제안: RFC 9420 §12.4, OpenMLS `openmls/src/group/mls_group/application.rs` L92–94
- **[19]** 기본값·오류: OpenMLS `group/mls_group/config.rs` L206–207 (`max_past_epochs` 기본 0), `tree/sender_ratchet.rs` L62 (`Self::new(5, 1000)`)·L339 (`SecretReuseError`)
- **[20]** 기타: `framing/private_message.rs` L370 (padding), `commit_builder/external_commits.rs` L82, `key_packages/mod.rs` L542 (`LastResortExtension`), `traits/src/storage.rs` L29 (StorageProvider), `traits/src/types.rs` L451 (X-Wing `0x004D`)
- **[21]** RFC 9420 §9.1: https://www.rfc-editor.org/rfc/rfc9420#section-9.1 — "Each sender has their own sender ratchet"
