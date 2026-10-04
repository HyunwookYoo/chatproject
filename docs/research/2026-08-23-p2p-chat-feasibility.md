# P2P Chat 앱 타당성 분석 (실측 기반)

- 작성일: 2026-08-23
- 상태: 분석 완료 / 설계 결정 대기
- 목적: 프레임워크 선정, 무서버 P2P 가능성, 블록체인 도입 시 사전 고려사항

## 0. 요구사항 (원문)

1. Cross-platform (Windows, Android, iOS)
2. 채팅 기록은 대화 당사자만 보유
3. 가볍고 빠른 반응성
4. 기능 확장성 염두

---

## 1. 실측 요약

### 1.1 측정 환경

| 항목 | 값 |
|---|---|
| OS | Windows 11 Pro 26200 (25H2) |
| 위치/회선 | 한국, 공인 IP `203.0.113.10`, 기본 게이트웨이 `192.168.55.1` |
| 툴체인 | Flutter 3.44.6 / Dart 3.12.2, Rust 1.92.0, Node 24.12.0, .NET 10.0.111, Android SDK 36, VS BuildTools 2022 |
| 측정일 | 2026-08-23 |

> 주의: NAT/네트워크 측정은 **이 회선 1개, 특정 시점** 결과다. 다른 ISP·모바일 네트워크·CGNAT 환경에서는 달라진다. 일반화는 3절의 대규모 공개 측정치로 보완했다.

### 1.2 NAT / 네트워크 (STUN RFC 5389·5780 직접 구현하여 측정)

| 측정 | 결과 | 의미 |
|---|---|---|
| NAT 매핑 동작 | **Endpoint-Independent** — 응답한 5개 STUN 서버 모두 동일한 `203.0.113.10:62787` 반환 | 홀펀칭에 유리 |
| 포트 보존 | local 62788→62788, 62789→62789, 62790→62790 | 포트 예측 가능, 매우 유리 |
| NAT 필터링 동작 | **Address-and-Port-Dependent** (CHANGE-REQUEST 무응답) | 종합: **Port-Restricted Cone NAT** |
| IPv6 | **없음** — link-local(fe80::) 3개뿐, `2606:4700:4700::1111` 아웃바운드 실패 | IPv6로 NAT 우회 불가 |
| UPnP IGD (SSDP) | **응답 없음** | 자력 포트 개방 불가 |
| NAT-PMP / PCP (gw:5351) | **응답 없음** | 자력 포트 개방 불가 |

**해석**: 이 회선은 홀펀칭이 잘 되는 축(Port-Restricted Cone + 포트 보존)에 속한다. 그러나 IPv6도 없고 자동 포트 개방 수단도 없어, **연결 성립에는 반드시 외부 관측자(STUN 등)가 필요**하다. 즉 "완전히 0대의 인프라"는 이 회선에서 이미 성립하지 않는다.

### 1.3 WebRTC 스택 실측 (node-datachannel 0.33.1 = libdatachannel)

| 측정 | 결과 |
|---|---|
| ICE 게더링 완료 | **47 ms** (host 3 + srflx 1) |
| srflx 후보 | 확보 — `203.0.113.10:54009` |
| DataChannel 오픈 (로컬 루프백, ICE+DTLS+SCTP 전체) | **5.4 ms** |
| DataChannel RTT 64B (로컬, n=200) | min **0.077 ms** / p50 **0.142 ms** / p95 **0.226 ms** |

**해석**: 암호화·전송 스택 자체의 오버헤드는 **0.15ms 수준으로 무시 가능**하다. 체감 지연은 100% 네트워크 왕복과 연결 성립 절차에서 나온다. → "가볍고 빠른 반응성"의 병목은 프로토콜이 아니라 **경로(직접 연결 vs 릴레이)** 다.

### 1.4 장거리 RTT (TCP connect 3회 최솟값)

| 지역 | RTT |
|---|---|
| 서울 (AWS ap-northeast-2) | **4.4 ms** |
| 도쿄 (ap-northeast-1) | 33.7 ms |
| 싱가포르 (ap-southeast-1) | 68.5 ms |
| 시드니 (ap-southeast-2) | 135.3 ms |
| 뭄바이 (ap-south-1) | 123.8 ms |
| 오리건 (us-west-2) | 139.0 ms |
| 버지니아 (us-east-1) | 186.2 ms |
| 프랑크푸르트 (eu-central-1) | 255.3 ms |
| 아일랜드 (eu-west-1) | 261.4 ms |
| 상파울루 (sa-east-1) | 300.3 ms |

### 1.5 공개 릴레이 인프라 지연 (한국 기준, TCP 443)

| 릴레이 | IP | RTT |
|---|---|---|
| iroh n0 `aps1-1.relay.iroh.network` (뭄바이) | 5.223.48.185 | **72.8 ms** ← 한국에서 가장 가까움 |
| iroh n0 `use1-1.relay.iroh.network` | 5.161.115.11 | 174.5 ms |
| iroh n0 `euw1-1.relay.iroh.network` | 116.203.71.221 | 257.8 ms |
| iroh DNS 디스커버리 `dns.iroh.link` | 49.13.207.244 (독일) | **257.8 ms** |
| Tailscale DERP (tok/sea) | — | 167.1 / 175.5 ms |
| Cloudflare STUN | 162.159.207.0 | **15.4 ms** |
| `apne1-1.relay.iroh.network` (아시아-북동) | — | **DNS 없음 (존재하지 않음)** |

**핵심 발견**: iroh의 공개 릴레이에는 **한국/일본 리전이 없다.** 한국 사용자 2명이 릴레이로 폴백되면 트래픽이 **뭄바이를 경유**한다.

| 경로 | 편도 | 왕복 |
|---|---|---|
| 국내 직접 P2P (추정, 동일국 타 ISP) | ~5–15 ms | ~10–30 ms |
| 뭄바이 릴레이 경유 (실측 기반) | ~73 ms | **~146 ms** |
| 서울 자체 릴레이 (실측 기반) | ~4.4 ms | ~9 ms |

→ **릴레이 폴백은 지역 릴레이를 직접 두면 사실상 공짜, 남의 글로벌 릴레이에 의존하면 5~15배 느려진다.** 또한 콜드 다이얼 시 `dns.iroh.link` 조회에 258 ms가 추가로 붙는다.

### 1.6 프레임워크 실측 (Hello World, Windows x64 release)

| 항목 | **Flutter 3.44.6** | **Tauri 2.x (WebView2)** |
|---|---|---|
| Windows 산출물 | 26.5 MB (디렉터리) / `flutter_windows.dll` 20.3 MB | **exe 8.58 MB** / NSIS 설치본 **1.83 MB** / MSI 2.85 MB |
| 클린 빌드 시간 | **43 s** | 122 s |
| Android APK (arm64-v8a split) | **14.55 MB** (fat 41.4 MB, armv7 11.9 MB) / 빌드 128.6 s | (미측정) |
| 실행 시 RSS (중앙값, 3회) | **83.1 MB** | **348.2 MB** (본체 28 MB + WebView2 6개 프로세스 320 MB) |
| 스레드 수 | 149 | 43 |
| 창 표시까지 | 328 ms | 69 ms |
| 시스템 공유 런타임 | 없음 (자체 엔진 번들) | WebView2 Evergreen 151.0.4129.101, 디스크 853 MB (기설치) |

**측정 주의사항 (정직하게)**
- `time-to-window`는 `MainWindowHandle != 0` 기준이다. Tauri는 웹 콘텐츠 렌더 **전에** 창 핸들이 잡히므로 69 ms는 "첫 화면이 보인 시각"이 아니다. 두 값을 직접 비교하면 안 된다.
- Tauri의 348 MB에는 WebView2가 Edge와 **공유하는 페이지**가 포함된다. 한계 증가분(marginal)은 이보다 작다. 그래도 단일 앱만 실행하는 사용자에게는 실제 메모리 압박이다.
- 둘 다 Hello World다. 실제 앱에서는 격차가 달라질 수 있다.

**그래도 방향은 명확하다**: 배포 크기는 Tauri가 압도적으로 작고(1.83 MB vs 26.5 MB), **실행 중 메모리는 Flutter가 4배 이상 가볍다.** "가볍고 빠른 반응성"이 사용자 체감이라면 후자가 더 중요하다.

---

## 2. Q1 — 어떤 프레임워크가 방향성에 맞는가

### 2.1 후보 평가

| 프레임워크 | Windows | Android | iOS | 메모리 | Rust P2P 코어 연동 | 판정 |
|---|---|---|---|---|---|---|
| **Flutter** | 1급 | 1급 | 1급 | 83 MB (실측) | `flutter_rust_bridge` 성숙, `iroh_flutter` 존재 | **권장** |
| Tauri v2 | 1급 | stable API지만 "first-class 아님" (공식 입장) | 동일 | 348 MB (실측) | 네이티브 (Rust가 호스트) | 모바일 리스크 |
| Compose Multiplatform (KMP) | JVM 기반 데스크톱 → 무거움 | 1급 | 1급 | — | Kotlin/Native FFI 번거로움 | Windows가 약점 |
| .NET MAUI | WinUI3 1급 | 보통 | 보통 | — | P/Invoke | iOS/Android 생태계 약함 |
| React Native | RNW(Microsoft) 후행 | 1급 | 1급 | — | 브리지 필요 | Windows가 약점 |

### 2.2 권장: **Flutter (UI) + Rust (P2P·암호 코어) via flutter_rust_bridge**

근거:
1. **Windows·Android·iOS 세 개를 모두 1급으로 지원하는 유일한 실용 선택지.** Tauri 모바일은 공식적으로도 "solid foundation, not the finished story"이고, KMP/MAUI/RN은 Windows 또는 iOS 중 한쪽이 약하다.
2. **실측 메모리 83 MB vs 348 MB.** 요구사항 3에 직결.
3. **P2P 스택은 Rust가 유일하게 성숙한 생태계**(iroh, libp2p, quinn, OpenMLS)이고, Flutter는 FFI로 그것을 그대로 쓸 수 있다. UI 언어와 네트워크 언어를 분리하는 게 오히려 장점이다 — 코어를 Rust로 두면 나중에 CLI·서버·데스크톱 데몬으로 재사용 가능(요구사항 4).
4. 자체 렌더링 엔진이라 3개 플랫폼 UI 편차가 없다. 채팅 리스트 스크롤 성능이 플랫폼마다 다르게 튀지 않는다.
5. **이 머신에 이미 Flutter 3.44.6 + Android SDK 36 + VS BuildTools + Rust 1.92가 전부 설치되어 있다.** 툴체인 진입 비용 0.

트레이드오프 (숨기지 않음):
- APK 14.55 MB, Windows 26.5 MB — 네이티브나 Tauri보다 크다. 채팅앱 기준으로는 허용 범위.
- Dart + Rust 2개 언어를 다뤄야 한다. FFI 경계 설계·에러 전파·비동기 스트림 처리에 초기 비용이 있다.
- `iroh_flutter` 1.0.3은 **매우 신생**(15일 전 게시, likes 1, downloads 258). 검증된 라이브러리가 아니다. 직접 `flutter_rust_bridge`로 iroh를 감싸는 편이 통제 가능성이 높다.

### 2.3 대안이 정당해지는 조건
- 팀이 Rust/웹 프론트에 강하고 **데스크톱 우선, 모바일은 나중** → Tauri v2
- 팀이 Kotlin에 강하고 **모바일 우선, Windows는 부차적** → KMP + Compose Multiplatform

---

## 3. Q2 — 서버 없이 온전한 장거리 P2P 채팅이 가능한가

### 결론부터: **네트워크 계층은 "거의" 가능. 그러나 모바일 OS 계층에서 불가능하다. iOS가 벽이다.**

### 3.1 네트워크 계층 — 가능하지만 100%는 아니다

**연결 성립 (홀펀칭)**
- 대규모 실측: IPFS/libp2p DCUtR 기준 **4.4백만 회 시도, 85,000+ 네트워크, 167개국**에서 홀펀칭 성공률 **70% ± 7.1%**. TCP와 QUIC이 통계적으로 동일(~70%). 성공한 연결의 97.6%는 첫 시도에 성립.
- iroh는 릴레이 폴백까지 포함해 **~90–95%** 연결 성공을 주장하며, 데이터의 ~95%는 직접 경로로 흐른다고 밝힌다.
- **즉 릴레이 없이 순수 홀펀칭만 쓰면 3명 중 1명은 연결 자체가 안 된다.**

**상대를 어떻게 찾는가 (디스커버리)** — 여기가 진짜 "서버 없음"의 시험대다.

| 방식 | 호스팅 인프라 필요? | 비고 |
|---|---|---|
| BitTorrent Mainline DHT (pkarr) | **불필요 — 완전 분산** | iroh가 opt-in 지원. 실질적으로 "서버 없이" 성립 가능한 유일한 경로 |
| mDNS / 로컬 디스커버리 | 불필요 | LAN 한정 |
| 티켓/QR 직접 교환 | 불필요 | 주소 변경 시 만료 |
| iroh 기본 DNS 디스커버리 | **필요 (n0 호스팅)** | 한국에서 258 ms |
| STUN | 필요 (다만 공개 서버 다수, Cloudflare 15.4 ms) | 관측만 하므로 프라이버시 영향 낮음 |

→ **DHT + 홀펀칭 조합이면 "서버 없이"가 기술적으로 성립한다.** 다만 실패율 ~30%와 콜드 다이얼 지연을 감수해야 한다.

**릴레이 폴백을 쓴다면** — 1.5절 실측대로 **반드시 서울에 두어야 한다**. 남의 글로벌 릴레이는 뭄바이 경유 146 ms다.

### 3.2 OS 계층 — 여기서 막힌다 (결정적)

**iOS**
- iOS는 임의 코드의 백그라운드 상시 실행을 허용하지 않는다. 앱이 suspend되면 소켓이 끊긴다. 상시 소켓은 내비게이션·오디오 재생 등 특정 카테고리에만 허용된다.
- **PushKit(VoIP push) 우회 불가**: iOS 13부터 VoIP 푸시를 받으면 반드시 CallKit에 통화를 보고해야 하며, 안 하면 **앱이 강제 종료**되고 반복되면 **푸시 전달 자체가 중단**된다. 과거 메신저들이 쓰던 이 우회로는 막혔다.
- **NEAppPushProvider(Local Push Connectivity) 우회 불가**: Apple에 별도 엔타이틀먼트를 신청·승인받아야 하고, 전제가 "APNs를 쓸 수 없는 네트워크"(기업 전용 Wi-Fi 등)다. 일반 소비자 P2P 채팅앱에는 승인되지 않는다.
- **Background push**: 시간당 1–2회, 회당 ~10초. 실시간 채팅 불가.
- 2026년 기준 동적 전력 예산 정책으로, 잦은 wake나 지속 연결은 "excessive"로 플래그된다.
- **⇒ iOS에서 백그라운드 메시지 수신은 APNs를 반드시 거쳐야 한다. 그리고 APNs로 푸시를 쏘려면 당신의 APNs 인증키를 가진 송신 주체가 필요하다 — 그게 곧 서버다.** (인증키를 앱에 넣는 건 보안상 불가능. 디바이스 토큰도 상대가 알 수 없다.)

**실증**: Tor 기반 P2P 메신저 **Briar는 바로 이 이유로 iOS 앱이 존재하지 않는다.** 백그라운드 상시 실행, BLE/Wi-Fi 메시, Tor 라우팅 전부 iOS 정책과 충돌한다.

#### 3.2.1 iOS 백그라운드 경로 전수 조사

| 경로 | 상시 소켓 | 서버 없이 가능 | 판정 |
|---|---|---|---|
| `beginBackgroundTask` | ~30초 | — | ❌ |
| `BGAppRefreshTask` (fetch) | ✗ | — | ❌ 시간당 수 회·수 초, 사용자가 끌 수 있음 |
| `BGProcessingTask` | ✗ | — | ❌ 충전 + 유휴 시에만 |
| BackgroundMode `audio` (무음 오디오) | ✓ | ✓ | ⚠️ 배터리 파괴 + 심사 리젝 + iOS가 감지해 종료 |
| BackgroundMode `location` | ✓ | ✓ | ⚠️ 동일 |
| BackgroundMode `bluetooth-central` | ✓ | ✓ | ❌ 근거리 전용, 장거리와 무관 |
| PushKit (`voip`) | — | ✗ | ❌ iOS 13+ CallKit 보고 필수, 미보고 시 앱 종료 + 푸시 중단 |
| `NEAppPushProvider` | ✓ | ✗ | ❌ **지정 Wi-Fi SSID에서만** 동작 (iOS 26 이더넷, iOS 27 통신사 MCX 5G 슬라이스 추가). 설계 자체가 "개발자의 로컬 provider 서버" 전제 + Apple 개별 승인 |
| APNs (alert / background) | — | ✗ | ❌ 푸시 송신 주체 필요. 인증키를 앱에 넣으면 유출 즉시 전체 파탄 |
| **`NEPacketTunnelProvider` (VPN 익스텐션)** | **✓** | **✓** | **⚠️ 유일하게 실제로 되는 경로** |

#### 3.2.2 NEPacketTunnelProvider — 유일한 진짜 우회로

VPN 익스텐션은 **Apple 개별 승인이 필요 없다.** `com.apple.developer.networking.networkextension` 의 `packet-tunnel-provider` 값은 2016년 11월 정책 변경 이후 일반 개발자가 그냥 쓸 수 있다. 앱 익스텐션으로 백그라운드 상시 실행되고 소켓이 유지되며, on-demand 룰로 항상 켜둘 수 있고, 익스텐션에서 로컬 알림도 띄울 수 있다.

**성립하는 아키텍처**: VPN 익스텐션이 피어와의 QUIC 연결 유지 → 메시지 도착 시 암호문 보관 + 로컬 알림 → 앱 실행 시 본체가 처리. **서버 0대로 iOS 백그라운드 수신이 실제로 성립한다.**

**대가**:
- **메모리 50 MB 하드 리밋** (iOS 15+; iOS 14 이하는 15 MB). 초과 시 jetsam이 `per-process-limit`으로 즉시 종료. MLS 그룹 상태 + 암호문 버퍼를 이 예산 안에 넣어야 한다. Rust 코어면 가능하나 빡빡하다.
- 사용자가 **VPN 프로파일 설치를 승인**해야 한다. 채팅앱이 VPN 권한을 요구하는 것은 신뢰 측면에서 큰 마찰이다.
- **상태바에 VPN 아이콘 상시 표시**, **다른 VPN과 배타적** (회사 VPN 사용자는 양자택일).
- 배터리 소모, 시스템 재량 종료.
- **App Store 정식 배포 불가** — 가이드라인 5.4는 VPN 앱을 **Organization 계정**으로 한정하며 VPN이 앱의 실제 목적일 것을 요구한다. 채팅앱의 VPN 익스텐션 사용은 리젝 사유다.
- **TestFlight(내부) / 개발자 서명 사이드로드에서는 동작한다.**

#### 3.2.3 세 층으로 나눈 결론

| 층 | 서버 0대로 가능? |
|---|---|
| 포그라운드 채팅 | ✅ 완전히 가능 |
| iOS 백그라운드 수신 | ⚠️ NEPacketTunnelProvider로 가능. App Store 포기 + VPN 마찰 + 50 MB |
| **오프라인 전달 (양쪽 모두 오프라인)** | ❌ **어떤 방법으로도 불가 — 논리적으로 불가능** |

**핵심**: 오프라인 전달은 정의상 "송신자도 수신자도 꺼져 있을 때 메시지를 보관할 제3자"를 요구한다. 이건 iOS 정책 문제가 아니라 **순수 논리 문제**이며, P2P만으로는 원천적으로 풀 수 없다.

**Android**
- Android 15+부터 `dataSync` 포그라운드 서비스는 **24시간 중 총 6시간** 제한. 초과 시 `onTimeout()`이 호출되고, 사용자가 앱을 포그라운드로 올려야 리셋된다.
- Android 16부터 FGS에서 시작한 백그라운드 잡도 런타임 쿼터를 따른다.
- Doze 모드, OEM별 배터리 킬러(중국 제조사 특히)까지 겹친다.
- ⇒ 상시 연결이 **가능하긴 하나 신뢰할 수 없다.** FCM 병행이 사실상 필수.

**독립 검증**: 2026년 논문 *Ember: A Serverless Peer-to-Peer E2EE Messaging System over an IPv6 Mesh Network*도 동일한 3대 한계를 명시한다 — (1) 오프라인 피어 전달 불가, (2) NAT 복잡도, (3) 모바일 배터리·연결성 제약.

### 3.3 오프라인 전달이라는 별개의 벽

순수 P2P는 **양쪽이 동시에 온라인일 때만** 전달된다. "메시지 보냈는데 상대가 자고 있으면 못 받는다"는 UX가 된다. 저장-전달(store-and-forward)이 필요하면 무언가는 항상 떠 있어야 한다.

### 3.4 현실적인 스펙트럼 — 요구사항 #2를 지키면서 고를 수 있는 것

요구사항 #2("기록은 당사자만 보유")를 **"서버가 평문과 메타데이터에 접근할 수 없다"**로 해석하면, 서버가 존재해도 요구사항은 지켜진다.

| 레벨 | 인프라 | 오프라인 전달 | iOS | 요구사항 #2 |
|---|---|---|---|---|
| **L0 순수 P2P** | 0대 (DHT + 홀펀칭) | ❌ 불가 | ❌ 사실상 불가 | ✅ 완벽 |
| **L1 최소 인프라 (권장)** | STUN + 지역 릴레이(폴백) + 푸시 릴레이 | ⚠️ 제한적 | ✅ 가능 | ✅ 유지 — 릴레이는 암호문만 통과, 저장 안 함 |
| **L2 익명 큐 (SimpleX형)** | 암호문 큐 릴레이 (TTL) | ✅ 가능 | ✅ 가능 | ✅ 유지 — 서버에 사용자 식별자 없음, 페어와이즈 익명 큐 |
| L3 일반 서버 | 중앙 서버 | ✅ | ✅ | ❌ 위배 |

**권장: L1에서 시작하고 L2를 옵션으로 설계한다.**
- 릴레이는 E2EE 암호문만 통과시키고 평문을 못 본다 (iroh 릴레이도 동일한 성질).
- 푸시 릴레이는 "디바이스 토큰 + 불투명한 wake 신호"만 다룬다. 메시지 본문은 P2P/릴레이로 별도 수신 후 NSE에서 복호화 (Signal 방식).
- 이 구조면 **"채팅 기록은 당사자만 보유"가 실제로 지켜진다.**

### 3.5 요구사항 #1과 #2의 충돌 — 명시적으로 짚어야 할 지점

> **iOS를 지원하면서 인프라 0대를 유지하는 것은 불가능하다.** 둘 중 하나를 골라야 한다.
> - (A) iOS 포기 → Windows + Android만. L0 순수 P2P 가능. Briar가 택한 길.
> - (B) 최소 인프라 수용 → 3개 플랫폼 전부. 요구사항 #2는 "서버가 평문 못 봄"으로 재정의.
>
> 이건 기술로 풀 수 있는 문제가 아니라 **제품 결정**이다.

---

## 4. Q3 — 블록체인 도입 시 지금 염두에 둘 것

### 원칙: **블록체인을 메시지 경로에 두지 말 것.**

온체인 메시지 저장은 요구사항 #2와 정면 충돌한다 — 영구 보존·공개·삭제 불가이며, forward secrecy를 원천적으로 파괴한다. 비용과 지연(초~분)도 채팅에 맞지 않다. 업계 표준 사례인 **XMTP도 메시지는 오프체인**에 두고, 온체인에는 아이덴티티와 MLS 그룹 커밋만 올린다.

### 4.1 지금 정해야 하는 것 (나중에 바꾸면 비싼 것)

**a. 키 아키텍처 — 가장 중요**
- 채팅 아이덴티티 키와 지갑 키를 **분리**하되, 서명으로 **바인딩** 가능하게 설계한다 (XMTP 방식: 지갑 서명으로 installation key를 인가).
- 곡선 선택이 나중을 좌우한다: 채팅 측은 Ed25519/X25519 (iroh의 EndpointID도 ed25519 공개키), EVM 측은 secp256k1. 둘을 억지로 통일하지 말고 **바인딩 레이어**를 처음부터 둔다.
- 원칙: **PeerID = 공개키**. 이 구조면 나중에 온체인 레지스트리를 얹는 게 자연스럽다.

**b. 주소 해석(resolver)을 플러그인화**
- `PeerID → 주소`를 해석하는 지점을 인터페이스로 추상화한다.
- 지금: DHT / QR 티켓 / mDNS. 나중: ENS류 온체인 네임 레지스트리를 **추가**만 하면 되게.
- 이걸 안 해두면 나중에 네트워크 코드 전반을 뜯어야 한다.

**c. 그룹 암호는 MLS(RFC 9420) 고려**
- Signal Double Ratchet은 1:1 설계다. 100명 그룹에서 멤버 변경 시 99회 재배포가 필요하다.
- MLS는 TreeKEM으로 **O(log N)**. 2026년 5월 Google Messages와 Apple Messages가 RCS에 MLS E2EE 롤아웃을 시작했고, Discord도 음성/영상 E2EE에 MLS를 쓴다.
- XMTP는 MLS를 쓰고 **그룹 커밋만 온체인 계약(GroupMessages)에 기록**한다 — 온체인 멤버십을 원한다면 이 경로가 정석이다.
- Rust `openmls`(참조 구현) + Dart 바인딩 `openmls` 2.0.1(flutter_rust_bridge 기반, Android/iOS/macOS/Linux/Windows/WASM 지원)이 이미 존재한다. 단 **likes 5, 신생**이므로 성숙도는 낮다.
- 1:1만 할 거면 Double Ratchet이 더 단순하다. **그룹 계획이 있으면 처음부터 MLS로 가는 편이 싸다.**

**d. 로컬 해시체인**
- 메시지에 로컬 해시 체인을 걸어두면, 나중에 **앵커링(타임스탬프 증명)** 만 온체인으로 올릴 수 있다. 메시지 자체는 계속 로컬에 남는다.
- 초기 비용 거의 0, 나중 옵션 가치 큼.

### 4.2 제품·규제 제약 (기능 범위를 결정한다)

- **Apple**: 가상자산 지갑 기능은 **Organization 계정으로 등록된 개발자만** 제공 가능하다 (개인 개발자 계정 불가). 온디바이스 채굴 금지. 거래·전송 중개는 해당 국가 라이선스 필요. ICO·파생상품은 인가 금융기관만.
- **한국**: 비수탁(non-custodial) 지갑은 일반적으로 특금법상 VASP로 보지 않지만, 교환·전송 중개 기능이 붙으면 판단이 달라진다. **기능 범위를 좁게 유지**하는 것이 안전하다. (법률 자문 필요 — 이 문서는 법률 의견이 아니다.)
- **EU**: MiCA / Travel Rule 고려 대상.

### 4.3 적합 / 부적합 용도

| 적합 | 부적합 |
|---|---|
| 아이덴티티·네임 레지스트리 (pubkey ↔ 사람 이름) | 메시지 저장 |
| 결제 / 팁 / 토큰 게이팅 | 실시간 전달 |
| 릴레이 노드 인센티브 (Session의 Oxen, Status의 Waku 모델) | 프레즌스 / 타이핑 표시 |
| 타임스탬프 앵커링 / 증명 | 연락처 목록 |

### 4.4 성능 원칙
온체인 작업은 초~분 단위다. **메시지 hot path에 절대 넣지 않는다.** 비동기 사이드채널로 분리하고, UI는 온체인 확정을 기다리지 않게 설계한다.

---

## 5. 요약 권고

1. **프레임워크**: Flutter (UI) + Rust 코어 (P2P·암호) via `flutter_rust_bridge`. 실측 메모리 83 MB, 3개 플랫폼 1급 지원, 툴체인 이미 구비.
2. **P2P**: iroh(QUIC) 기반. DHT 디스커버리 opt-in + **서울 자체 릴레이**. 순수 무서버는 iOS를 포기해야만 성립한다.
3. **요구사항 #2 재정의 권장**: "서버 부재" → "서버가 평문·메타데이터에 접근 불가". 이러면 3개 플랫폼 + 오프라인 전달을 모두 얻는다.
4. **블록체인**: 지금은 **키 구조(pubkey=identity) + resolver 추상화 + (그룹 계획 시) MLS** 세 가지만 준비. 그 외에는 나중에 붙여도 늦지 않다.

---

## 6. 미결정 사항 (설계 착수 전 확인 필요)

1. "서버를 끼지 않고"의 허용 범위 — 0대 / 평문 못 보는 최소 릴레이 / 암호문 큐잉까지
2. 오프라인 메시지 전달이 필수인가
3. 1:1 전용인가, 그룹 채팅 계획이 있는가 (→ Double Ratchet vs MLS 분기)
4. iOS App Store 정식 배포가 필요한가 (사이드로드/TestFlight면 제약 완화)

---

## 부록 A. 측정 스크립트

스크래치패드: `%LOCALAPPDATA%\Temp\claude\C--ChatProject\0fbabee5-58f6-4122-ab49-a3524c474cbe\scratchpad\`

- `nat_probe.py` — STUN 기반 NAT 매핑/필터링/포트 예측성
- `net_probe.py` — IPv6, UPnP SSDP, NAT-PMP/PCP, 글로벌 RTT
- `relay_rtt.py` — 공개 릴레이/디스커버리 인프라 RTT
- `webrtc/ice.mjs` — ICE 게더링 + DataChannel 지연
- `bench/fl_hello`, `bench/tauri_hello` — 프레임워크 벤치마크 프로젝트

전부 임시 디렉터리다. 재현이 필요하면 프로젝트로 옮길 것.

## 부록 B. 출처

- Trautwein et al., *Challenging Tribal Knowledge — Large Scale Measurement Campaign on Decentralized NAT Traversal*, arXiv:2510.27500 — https://arxiv.org/abs/2510.27500
- *Ember: A Serverless P2P E2EE Messaging System over an IPv6 Mesh Network*, arXiv:2603.16735 — https://arxiv.org/pdf/2603.16735
- iroh 릴레이 — https://docs.iroh.computer/concepts/relays
- iroh 디스커버리 — https://docs.iroh.computer/concepts/discovery
- iroh vs libp2p — https://www.iroh.computer/blog/comparing-iroh-and-libp2p
- libp2p 홀펀칭 — https://libp2p.io/docs/hole-punching/
- Tauri 2.0 릴리스 — https://v2.tauri.app/blog/tauri-20/
- Android FGS 타임아웃 — https://developer.android.com/develop/background-work/services/fgs/timeout
- Apple PushKit/VoIP 제약 — https://developer.apple.com/forums/thread/117939
- Apple NEAppPushProvider — https://developer.apple.com/documentation/networkextension/neapppushprovider
- Apple App Review Guidelines — https://developer.apple.com/app-store/review/guidelines/
- Briar iOS 타당성 검토 — https://code.briarproject.org/briar/briar/-/work_items/2282
- OpenMLS — https://github.com/openmls/openmls / Dart 바인딩 — https://pub.dev/packages/openmls
- XMTP 프로토콜 개요 — https://docs.xmtp.org/protocol/overview
- flutter_webrtc — https://pub.dev/packages/flutter_webrtc
- iroh_flutter — https://pub.dev/packages/iroh_flutter
