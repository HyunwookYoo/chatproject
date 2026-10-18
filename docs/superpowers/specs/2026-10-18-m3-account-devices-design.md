# M3 계정과 기기 상세 설계

- 작성일: 2026-10-18
- 상태: **APPROVED — 2026-10-18 사용자가 전체 설계를 승인했다. 다음 단계는 작업별 구현 계획 작성·검토다. M2의 열린 항목은 유지한다.**
- 기준 소스: 로컬 `aadd2db`. M2 코드 최종 검토 결과와 원격 통합·마일스톤 수용은 별개다. 이 문서는 push·통합·구현 착수를 승인하지 않는다.
- 상위 승인: [전체 설계 rev.3 §4·10·11·12·16](2026-10-04-p2p-chat-design.md), [로드맵 M3](../plans/2026-10-04-p2p-chat-roadmap.md#m3--계정과-기기).
- 참조: [M1a 후속 M3·M4](../plans/2026-10-05-m1a-carry-forward.md), [M1b 후속](../plans/2026-10-11-m1b-carry-forward.md), [자동화 설계](2026-10-05-test-automation-design.md).
- **아래 `제안`은 상세 설계 작성 당시의 권고를 표시하며, 2026-10-18 전체 설계 승인에 포함되었다.** 이 승인은 구현 계획 작성을 허용한다. 제품 구현은 별도 구현 계획 검토 뒤에 진행하며, 공개 네트워크 시험·서명 배포·원격 통합은 별도 실행 범위다.

## 1. 목적과 현재 경계

복구 문구가 같은 사람을 식별하고, 기기마다 다른 전송 키를 가지며, 사용자가 확인한 기기만 내 계정 키를 받는 기반을 만든다. Windows·Android·iOS 모두 한국어로 계정 생성, 초대, 안전번호, 기기 연결·해제·복구를 제공한다.

현재 코어는 런타임·전송·진단 기능이며 계정/아이덴티티 모듈은 없다. M2의 프로세스마다 새로 생성하는 전송 키와 네트워크 probe는 계정 보안 기능이 아니다. Android JNI 초기화는 M2에서 이미 다뤘으며 M3 신규 기능으로 세지 않는다.

M2의 엄격한 최초 연결 500 ms 목표 미달에 대한 사용자 수용 판단과 사용자가 유예한 iOS LAN 확인 등은 [M2 결과](../results/2026-10-16-m2-transport.md)의 열린 항목으로 남는다. M3 설계 승인이 M2 완료나 M3 구현 해제를 뜻하지 않는다.

사용자 결정: 제품 표시 이름은 **`ChatProject`를 임시로 사용**한다(2026-10-18). 최종 브랜드 확정은 아니다. Android 런처·Windows 제목·iOS 표시 이름·Flutter 제목의 임시 표기를 맞추되 bundle/application ID는 바꾸지 않는다.

## 2. 승인 범위와 이번 제안

| 항목 | 기존 승인 | M3 상세 제안 / 이후 경계 |
|---|---|---|
| 계정 | RecoverySeed 256비트, BIP-39 24단어, Ed25519 IdentityKey, base32 PeerId | 영어 단어 목록·한국어 설명, §3의 고정 파생 계약 |
| 기기 | 기기별 DeviceSeed 32B → 전송/installation 키, IdentityKey 서명 cert | OS 보호와 최소 영속 상태를 M3에 도입 |
| 공개 목록 | IdentityRecord, pkarr/Mainline, 최대 5대, 1000B, IP 비게시 | cert·URL을 목록 밖에서 얻는 2단계 조회, §5의 충돌 표시 |
| 초대 | 서명·만료·선택 표시 이름 UTF-8 64B, 안전번호 | **M3 전용 identity 초대**를 별도 타입으로 정의. 전체 `Invite v2`는 M5의 KeyPackage와 함께 완성 |
| 연결 | QR 양방향, 양 화면 확인 코드, 역방향 기존 기기 승인 필수 | 두 방향 모두 기존 기기 명시 승인, 표준 PSK 채널, §7 |
| 해제/복구 | 목록 제거·같은 PeerId 복구, 연락처·기록 복원 없음 | M3는 계정 목록/연결 허용만 처리. MLS 제거 효력은 M5·M9 |
| 저장 | 전체 DB·outbox는 M4 | 작은 암호화 계정/연락처 snapshot·미완료 의도만 앞당김. SQLCipher·메시지 DB는 만들지 않음 |

M4의 SQLCipher/outbox/메시지 상태·M5의 MLS/Welcome/KeyPackage·M6 메일박스·M7 푸시·M8 큐·M9 그룹 조정은 이 문서의 구현 범위 밖이다. 상위 설계의 연락처·대화 목록/선택 기록 전송은 존재하는 데이터에 적용한다. M3에는 연락처만 있고 대화/기록 전송 UI는 제공하지 않는다. 향후 기능을 이미 지원하는 것처럼 빈 성공을 반환하지 않는다.

## 3. 키와 복구 문구 계약 — 제안

키 생성·파생·서명·검증·민감 상태는 공통 Rust 코어가 소유한다. Flutter는 입력·표시, 네이티브 계층은 OS 보호 기능만 맡는다. Rust 1.92, 기존 Flutter, FRB 2.11.1, UniFFI 0.32.2를 유지한다. 신규 crate/API/version은 구현 계획 시 MSRV·플랫폼·라이선스·공식 API를 확인하고 고정한다.

1. OS CSPRNG로 32B `RecoverySeed`를 만든다. BIP-39의 entropy/checksum/영어 목록으로 24단어를 인코딩하고 역변환한다.
2. **256비트 entropy가 이 설계의 RecoverySeed다.** BIP-39의 별도 `mnemonic → 64B seed` PBKDF2 단계와 혼동하지 않는다. M3에는 추가 passphrase가 없다. 복구 시 24단어/사전/checksum을 검증해 원래 32B entropy를 얻는다.
3. HKDF-SHA256의 salt=`ChatProject/account-keys/v1`, IKM=RecoverySeed, info=`identity-ed25519`, L=32를 Ed25519 signing seed로 사용한다. HKDF의 문자열은 ASCII, 길이는 바이트다. RFC 5869 방식이며 자체 KDF를 만들지 않는다.
4. `PeerId`는 Ed25519 공개키 32B를 RFC 4648 base32 소문자·padding 없이 표현한 52자다. iroh EndpointId와 다른 사용자 식별자다. 내부 비교는 원시 32B로 한다.
5. 독립 CSPRNG로 기기마다 32B `DeviceSeed`를 생성한다. HKDF salt=`ChatProject/device-keys/v1`, info=`transport-ed25519` 또는 `installation-ed25519`, 각각 L=32. 다른 기기에 복사하지 않는다.
6. `TransportKey` 공개키는 해당 기기의 iroh EndpointId, `InstallationKey` 공개키는 installation 식별자이자 향후 MLS leaf 서명키다. `installation_id`는 이 공개키의 같은 base32 표기다.

BIP-39는 256비트 entropy를 24단어로 표현하며 별도의 512비트 seed 변환도 정의한다. 이 문서는 첫 변환만 복구 인코딩에 사용한다. 영어 목록 권고와 NFKD 입력 처리는 [공식 BIP-39](https://github.com/bitcoin/bips/blob/master/bip-0039.mediawiki)를 따른다. HKDF의 분리된 info 사용은 [RFC 5869](https://www.rfc-editor.org/rfc/rfc5869)에 근거한다. **이 앱의 파생 labels는 제안이며 지갑 호환 규약이 아니다.**

최초 생성 화면에서 24단어를 한 번 보여 주고 임의의 세 위치를 확인한다. 확인 전 취소하면 미완료 계정을 폐기하고 게시하지 않는다. 확인 후에는 RecoverySeed와 문구를 저장하지 않으며 다시 보기 기능이 없다. 복구 성공 후 입력도 지운다. 화면은 앱 전환 미리보기에서 숨기고 자동 clipboard 복사·로그·분석 이벤트를 만들지 않는다. 사용자가 외부에 적어 둔 문구까지 앱이 보호한다고 설명하지 않는다.

입력은 NFKD·앞뒤/연속 공백을 정규화하고 영어 소문자로 검사한다. 다른 단어/순서/checksum 오류를 자동 교정하지 않는다. 문구·키는 가능한 한 수명 짧은 secret buffer에 두며, Dart 문자열·OS 키보드의 완전한 메모리 소거는 보장하지 않는다. 시험 fixture는 공개 벡터만 사용한다.

## 4. 보호 저장과 M2 전송 키 전환 — 제안

IdentityKey의 signing seed와 DeviceSeed는 디스크에 평문으로 남기지 않는다. **모든 신뢰 기기가 IdentityKey를 가진다는 상위 모델을 유지**한다. 하드웨어 비반출 키만으로 IdentityKey를 구현하면 다른 기기에 키를 넘길 수 없으므로, OS 보호 아래 보관하고 필요한 동안 Rust 메모리에서 사용하는 모델이다.

| 플랫폼 | M3 보호 방식 | 실패/백업 경계 |
|---|---|---|
| Windows | 사용자 범위 DPAPI로 32B 저장 보호 키를 감싸고, app-private LocalAppData에 암호화 snapshot 보관 | machine 범위 플래그를 쓰지 않음. 다른 Windows 사용자/PC로 파일 복사해 복원하는 기능 없음 |
| Android | Android Keystore의 비반출 AES wrapping key로 저장 보호 키를 감싼 blob + private no-backup 파일 | **raw DeviceSeed를 Keystore 비반출 키로 저장한다는 뜻이 아님**. StrongBox는 가능할 때 사용하고 지원 여부를 실제 확인 |
| iOS | Keychain에 저장 보호 키, 승인된 `kSecAttrAccessibleAfterFirstUnlock`과 공유 access group 유지 | 잠금 해제 전 읽기 불가를 정상 상태로 처리. **제안:** ThisDeviceOnly 변형·비동기화, 앱 파일 백업 제외로 기기 복제를 방지 |

플랫폼 동작 근거: [DPAPI](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata), [Android Keystore](https://developer.android.com/privacy-and-security/keystore), [Apple accessibility](https://developer.apple.com/documentation/security/ksecattraccessibleafterfirstunlock), [ThisDeviceOnly](https://developer.apple.com/documentation/security/ksecattraccessibleafterfirstunlockthisdeviceonly). 지원하지 않는 보안 하드웨어를 있다고 표시하지 않는다. OS 보호 키가 없거나 손상되면 새 계정을 조용히 생성하지 않고 “이 기기의 계정 키를 열 수 없어요”로 복구/재시도 경로를 준다.

최소 저장은 versioned **암호화 snapshot 하나와 pending intent 목록**이다. 계정 키·내 cert·알려진 기기 목록/해제 표식·연락처 이름/검증 상태·최근 수용 record와 seq high-water·§7.3의 연결 transaction/미완료 게시 작업만 담는다. 일반 DB·범용 journal framework를 만들지 않는다. 제안 상한: 파일 4MiB, 연락처 1000명, 미완료 의도 32개. 초과 시 이전 정상 상태를 유지하고 용량 오류를 보인다.

암호화는 검증된 라이브러리의 AES-256-GCM, 매 snapshot 새 random 96비트 nonce, AAD에 format version/로컬 installation 식별자를 포함한다. 임시 파일 flush → atomic replace → 가능한 플랫폼 durability 절차로 기존 파일을 교체한다. 검증된 새 snapshot이 없으면 이전 것을 유지한다. OS wrapping key와 첫 snapshot 생성 중 중단되면 orphan 자원을 정리할 수 있으나 정상 계정으로 자동 승격하지 않는다. 변조·버전 미지원은 읽기를 거부한다. 로컬 관리자의 과거 파일/키 전체 rollback을 방어한다고 주장하지 않는다.

같은 canonical data folder의 **단일 writer 소유권**을 secret read 전에 확보한다. Windows·Android는 프로세스 간 독점 잠금으로 중복 writer를 거부한다. iOS는 앱 프로세스 하나만 account writer이며 NSE는 접근하지 않는다. 상위 승인 §11의 suspend/file-lock 경고에 따라 iOS에서는 수명 전체의 file lock을 잡지 않는다. foreground의 짧은 원자 저장 구간만 보호하고 background 전환 시 새 쓰기를 막아 진행 중 저장을 끝내거나 취소한 뒤 모든 file lock을 해제한다. 재개 시 마지막 완전 snapshot/소유권을 다시 확인하고, 소유권을 입증할 수 없으면 쓰기·비밀 전달을 보류한다. suspend 중 lock 유지·별도 account writer 도입은 허용하지 않는다.

이는 M4 후속의 최소 소유권/저장 정책만 앞당기는 제안이다. 키 파일·백업 제외·Windows LocalAppData 선택도 동일하며 M4 DB 잠금 문제 해결/완료로 세지 않는다. 기존 폴더에 M3 상태가 없을 때만 새 기본 위치를 선택하고 기존 상태를 자동 이동·덮어쓰지 않는다. M4는 이 snapshot을 성공적인 SQLCipher transaction으로 가져온 뒤에만 이전 상태를 폐기한다.

새 기기 연결을 시작할 때는 계정이 아직 없어도 먼저 **미활성 candidate DeviceSeed를 OS 보호 저장**하고, 그 TransportKey로 Endpoint를 bind한 뒤 QR을 만든다. key 저장/bind 실패 시 QR을 내보내지 않는다. 계정 생성·복구도 DeviceSeed 저장 후 해당 키로 bind한다. 재시작 시 같은 candidate/활성 기기의 EndpointId는 같고 CoreInfo.instance_id는 기존처럼 프로세스마다 새 값이다. M2의 일회성 probe key는 계정 키로 승격하거나 계정으로 추정하지 않는다. 진단 기능은 유지하되 계정 인증 성공 표시를 주지 않는다.

기존 CoreSlot 전체 stop/reset 공개 API를 추가할 필요는 없다. **제안:** 동일 코어 안에서 직렬화된 `진단 → 계정/candidate 키 사용` 전환 시 진단 세션/기존 Endpoint를 닫고 새 키로 재bind한다. 전환 중 명령은 명시적 busy, 이전 이벤트는 generation을 확인해 무시한다. bind 실패 후에는 키를 유지하고 offline 상태에서 재시도한다. 이미 활성 계정이 있는 폴더에 create/recover/link를 실행하면 거부한다. 미활성 pending import에는 §7.3의 같은 transaction 재개/폐기만 예외로 허용한다. 계정 갈아끼우기·범용 초기화/삭제는 M3 범위에 넣지 않는다.

## 5. 인증서와 공개 IdentityRecord — 제안

`InstallationCert`는 `{version, peer_id32, installation_pub32, transport_pub32, kind, created_at, sig64}`다. kind는 desktop/mobile, created_at은 UTC 초다. IdentityKey가 “이 기기를 인증한다”는 서명을 한다. 인증서만 유효하다고 현재 활성 기기인 것은 아니다. 접속 시 cert 서명·원하는 PeerId·remote EndpointId·installation proof·현재 목록/로컬 해제 표식을 함께 확인한다.

cert·초대·연결 메시지의 서명 입력은 타입별 고정 ASCII domain + version + deterministic CBOR로 한다. [RFC 8949 §4.2](https://www.rfc-editor.org/rfc/rfc8949.html#section-4.2)의 deterministic encoding을 사용하며 고정 순서 배열·고정 길이 byte string·정수만 허용한다(이름만 UTF-8). duplicate/추가 필드·비정규 encoding·미지원 version·잘못된 길이를 거부한다. 정확한 배열 순서/타입과 공개 golden vector는 구현 계획에서 문서화하되 이 계약을 임의 변경하지 않는다. domain은 `ChatProject/cert/v1`, `ChatProject/identity-invite/v1`, `ChatProject/pair-proof/v1`로 분리한다.

**공개 목록 크기 해법:** IdentityKey로 서명한 pkarr account packet에는 application version, recovery generation 16B, issued/expires 시각, 최대 5개의 `(installation_pub32, transport_pub32, kind)`만 넣는다. cert와 릴레이 URL은 이 packet에 넣지 않는다. `PeerId → account packet → EndpointId → M2의 endpoint address lookup`으로 조회하고, 연결 후 cert를 받는다. EndpointId 자체의 전송 주소 record는 각 기기의 TransportKey가 서명하며 기존 relay-only 정책을 유지한다. 단일 account packet과 endpoint 주소 record를 혼동하지 않는다.

account packet은 `_account` TXT의 base64url(no padding) deterministic CBOR payload 하나로 제안한다. 이름·IP·연락처·기기 별칭·키 material은 공개하지 않는다. DNS packet과 서명 wrapper를 포함한 앱 직렬화가 **1000B 이하**인지 5대 fixture로 확인하고, DNS value의 BEP 44 상한도 각각 만족시킨다. 한도를 넘으면 게시 전 오류이며 기기 수를 몰래 줄이거나 cert 검증을 없애지 않는다. 5개 전체 cert와 긴 relay URL을 한 packet에 넣는 방식은 채택하지 않는다.

[BEP 44](https://www.bittorrent.org/beps/bep_0044.html)는 mutable value 크기·서명·증가 seq와 노드별 선택 CAS를 정의한다. 이를 전역 트랜잭션/합의로 해석하지 않는다. [pkarr Client 문서](https://docs.rs/pkarr/latest/pkarr/struct.Client.html)의 publish도 여러 발행을 자동 직렬화해 주지 않는다. 설계 작성 당시 로드맵 pkarr `8.x`와 검색된 공식 문서 버전 차이는 미해결 가정이었다. 후속 [구현 계획 B1](../plans/2026-10-18-m3-b-identity-pairing.md) 작성에서 공식 배포 `8.1.0`/MSRV 1.85/NetworkOnly API를 확인했다. 정확 pin은 계획 검토 대상이며 실제 target compile 전 호환성 통과로 세지 않는다.

기본 제안: account record 유효 기간 7일, 전경 활성 중 24시간 이내 재게시. 만료된 record는 “마지막으로 알려진 기기”와 §5.1의 갱신 base 참고만 하며 일반 접속의 권한 근거로 삼지 않는다. 5분 초과 미래 issued_at은 시계 오류로 보류한다. 일반 접속의 cache/만료/재인증 조건은 §5.2에 따른다. 정확한 published seq는 선택 pkarr의 timestamp 계약에 맞추고, 시계 역행/미래 record는 오류를 표시하며 삭제·복구 성공으로 숨기지 않는다.

### 5.1 동시 추가·해제·복구와 오프라인

단일 기기 안의 목록 변경은 직렬화하고, 읽은 최신 record/그 hash와 로컬 intent를 durable하게 저장한 뒤 발행한다. PeerId별 **서명/형식을 검증해 관측·수용한 최대 seq와 직접 발행에 할당한 최대 seq**를 snapshot에 유지한다. 다른 payload를 만들 때는 두 high-water보다 큰 seq를 할당/저장한 뒤 송신한다. 같은 서명 packet 재시도만 같은 seq를 쓸 수 있다. 조회에 아무것도 없거나 오래된 값이 와도 high-water를 낮추지 않으며, 그보다 낮은 값/동일 seq 다른 값을 정상 최신본으로 자동 수용하지 않는다. 현재 시각·선택 라이브러리의 timestamp 규약으로 더 큰 유효 seq를 만들 수 없으면 시계/충돌 해결을 요구한다.

게시 완료 응답만으로 “전 세계 갱신 완료”라 하지 않는다. 재조회와 연결된 내 기기와의 상호 인증 sync에서 누락·동일 seq 다른 값·generation 변경을 검출한다. DHT만으로 선형화 가능한 기기 목록은 보장하지 않는다. **정상 완료한 best-effort 조회에 usable record가 없다는 결과는 전역 부재의 증명이 아니다.** timeout/error/네트워크 불가는 그 결과와 구분한다.

| 조회 결과 / 로컬 근거 | 허용하는 게시 동작 — 제안 |
|---|---|
| error/timeout/불가 | 생성·복구 키는 로컬 보관 가능, 새 게시/목록 변경은 대기. 기존 unexpired cache 접속만 §5.2 조건으로 가능 |
| 정상 완료 + 최초 계정의 record 없음 | 사용자가 문구 확인한 새 계정의 최초 generation/기기 하나를 저장된 high-water보다 큰 seq로 최초 게시 |
| 정상 완료 + 알려진 generation의 record가 만료됨, 또는 결과가 없지만 로컬에 자기 기기가 포함된 같은 generation의 signed base가 있음 | 로컬 tombstone을 적용한 **같은 기기 목록·같은 generation 갱신** 허용. 새 기기 자동 추가/누락된 기기 복원은 하지 않음. 다른 base/더 높은 seq를 관측하면 충돌로 보류 |
| 정상 완료 + 유효 최신본 | 해당 base로 갱신/사용자가 승인한 추가·해제. 이미 할당한 자기 후속 packet이 확인된 경우는 성공으로 수용하고, 예상하지 않은 변경은 충돌로 보류 |
| 정상 완료 + 문구 복구, usable record 없음/만료 또는 기존 목록 대체 필요 | 사용자가 “이전 기기를 제외하고 복구”를 확인하면 아래 replacement 게시. 관측한 만료본의 seq와 남아 있는 로컬 high-water도 보존 |

따라서 7일 넘게 모두 꺼져 있었더라도 저장된 기기는 같은 generation을 갱신하고, 모든 기기를 잃은 사용자는 문구 복구 replacement로 진행할 수 있다. 결과 없음만으로 이전 기록을 자동 덮어쓰거나 자동 복구하지 않는다. 뒤늦게 더 높은 이전 작성자의 record가 발견되면 성공을 유지한 채 숨기지 않고 충돌/기기 확인 상태로 전환한다.

**제안: 조용한 자동 병합보다 명시적인 충돌 보류를 우선한다.** 현재 base와 다른 record를 발견하면 새 추가/해제 intent를 자동으로 덮어쓰지 않고 `목록 충돌 — 내 기기에서 다시 확인`으로 둔다. 이미 승인한 추가 intent도 새 목록에 여유가 있는지 재확인한다. 어떤 발행본에도 5대를 넘겨 넣지 않으며 경쟁한 여섯 번째 기기는 활성 완료로 표시하지 않는다. 연속/동시 QR 연결은 기기별 한 세션만 허용한다.

해제한 installation은 로컬 tombstone으로 즉시 차단한다. **직접 연결된 신뢰 기기**에는 서명한 해제 intent를 보내고 각자 저장/확인한다. 이를 받지 못한 오프라인 기기까지 즉시 차단됐다고 하지 않는다. tombstone은 M3 snapshot 내 최대 256개이며 자동 만료·자동 재추가하지 않는다. 한도/수용 목록과 충돌 시 수동 해소 전 추가 발행을 보류한다. 최신 목록에서 사라진 cert도 자동 재활성화하지 않는다. 충돌 해소는 로컬 해제 표식을 유지한 상태로 사용자가 현재 기기 목록을 재확인하여 새 version을 발행하는 동작이다.

이 설계는 자동 무제한 tombstone CRDT를 만들지 않는다. 오래 오프라인이던 작성자는 최신 base를 읽고 미완료 intent를 재확인하기 전 발행할 수 없다. tombstone을 전혀 모르는 새 작성자/이전 snapshot으로 돌아간 작성자와의 전역 보안 수렴을 보장하지 않으며 이런 record 차이는 충돌로 보류한다. 더 강한 오프라인 동시 편집을 원하면 별도 프로토콜 설계가 필요하다.

복구는 위 조회/사용자 확인 후 새 random recovery generation과 새 기기 하나를 담은 **replacement**를 제안한다. 이전 기기를 병합하지 않는다. 연결된 기존 기기는 generation 변경을 보이면 자기 pending intent를 보류하고 복구/기기 목록을 재확인한다. DHT 조회 불가일 때는 같은 PeerId를 로컬에서 되살리되 `복구됨 · 기기 목록 갱신 대기`로 둔다. 정상 조회의 결과 없음/만료는 위 표에 따라 replacement를 발행할 수 있다. 병렬 복구/이전 작성자와 경합하면 자동 승리로 간주하지 않는다. 이전 작성자가 공유 IdentityKey로 다시 서명할 수 있고, 복구가 전역 폐기/신선도 증명을 주지 않는다는 경고를 확인 화면에 포함한다.

**보안 한계:** 모든 trusted device가 IdentityKey를 가진다. 제거/새 generation/같은 PeerId 복구로 이전 기기의 IdentityKey 사본을 취소할 수 없다. 공격자는 새 cert·더 높은 seq·새 목록을 서명할 수 있다. 이 상태의 해결은 새 RecoverySeed/새 PeerId로 아이덴티티 교체 및 연락처 안전번호 재검증이며, M3가 자동으로 완료해 주는 기능은 아니다. 이 한계를 완화하는 별도 root-key/위임 구조는 상위 승인 변경이므로 몰래 도입하지 않는다.

### 5.2 일반 M3 계정 접속 인증과 재인증 — 제안

일반 account 접속은 별도 `chatproject/account/1` protocol을 사용한다. M2 probe 연결, §7의 Noise pair 및 §7.3의 제한된 pair-resume를 이 권한으로 승격하지 않는다. 양쪽은 cert, 주장하는 PeerId/generation, fresh random 32B nonce를 교환한다. 발신/수신 역할 순서의 양쪽 PeerId·EndpointId·installation_pub·cert hash·generation·nonce를 담은 deterministic context로 TLS exporter 32B를 얻고, 각 InstallationKey가 domain `ChatProject/account-auth/v1` + context + exporter를 서명한다. nonce/context/서명은 다른 연결/역할/installation에서 재사용할 수 없다. 0-RTT나 transport 재연결만으로 계정 인증을 복원하지 않는다.

TLS binding은 pinned iroh 1.3.0의 `Connection::export_keying_material(output, label, context)`를 사용하며 label=`ChatProject/account-exporter/v1`이다. 실제 로컬 dependency source `build/e2e/task14-wakeup-probe/dependency-source/iroh-1.3.0/src/endpoint/connection.rs:1187`에서 이 API와 동일 label/context의 양단 출력 계약을 확인했다. 이 근거는 읽기 확인이며 실행 시험 결과가 아니다. exporter 실패나 identity/context 불일치는 접속 인증 실패다.

검증자는 cert의 IdentityKey 서명/예상 PeerId, `cert.transport_pub == 실제 remote EndpointId`, InstallationKey의 fresh exporter-bound proof, 수용한 account record의 **같은 generation에 정확한 installation/transport 쌍 존재**, 로컬 tombstone 부재를 모두 요구한다. 내 기기 sync에는 양쪽 PeerId가 내 PeerId와 같아야 하며, 다른 사람의 연락처 접속에는 그 사람의 record를 쓴다. IdentityKey 또는 TransportKey 소유만으로 installation proof를 대신하지 않는다.

유효기간 안의 마지막 수용 signed cache가 있고 이후 더 높은 record/다른 generation/제거/충돌을 관측하지 않았다면, 조회 불가에도 그 cache와 새 proof로 일반 재접속을 허용한다. UI에는 `캐시로 기기 확인 · 목록 갱신 지연`을 보인다. 새 기기 추가·해제·recovery replacement의 승인/발행에는 이 예외를 쓰지 않고 §5.1의 정상 조회/로컬 확인을 요구한다. fresh lookup도 전역 revocation 부재를 증명하지 않으므로 cache와 lookup 양쪽 모두 알려진 상태까지의 인증이라는 한계를 유지한다.

접속 권한은 검증한 record hash/generation/만료에 묶인다. 만료 시 연결을 **제한 상태**로 바꾸고 연락처/계정 데이터·변경 intent 송수신을 중단한다. 이때 공개 signed record 교환과 재인증 handshake만 허용하며, §5.1 갱신본을 검증한 뒤 새 nonce/proof가 성공해야 일반 기능을 재개한다. 같은 generation의 더 높은 record를 수용해도 재인증 전에는 기존 권한을 이어 쓰지 않는다. remote installation 제거/tombstone이면 즉시 해당 권한/연결을 종료한다. generation 변경/동일 seq 충돌은 해당 계정의 기존 권한을 모두 무효화하고, 충돌 해소 및 새 record 기반 재인증 전 데이터를 주고받지 않는다. 내 installation이 새 목록에서 빠졌으면 내 일반 계정 활동도 보류한다.

M2의 transport-connected 시각/500 ms 지표는 그대로 둔다. 추가 cert/record/proof 처리가 끝난 `account-auth-ready`를 별도 상태로 기록하며 이 늦은 시각으로 M2 측정 정의를 바꾸거나 기존 미달을 통과 처리하지 않는다.

## 6. 초대·연락처·안전번호 — 제안

상위 `Invite { version:2, key_package:MlsKeyPackage, ... }`는 M5 이전에 온전히 생성할 수 없다. **권고:** M3는 `IdentityInvite v1`/`chatproject:identity-invite/1/<payload>`라는 별도 네임스페이스로 계정과 연락처 추가만 한다. null·가짜·빈 KeyPackage를 `Invite v2`라고 내보내지 않는다. M5는 완전한 v2를 추가하며 기존 identity 초대는 연락처 추가로 계속 해석하고 자동으로 대화를 만들지 않는다. 이는 마일스톤 경계 조정 제안으로 전체 설계 승인에 포함한다.

초대 payload는 `{version, peer_id32, issuing_cert, endpoint_hint, invite_nonce16, display_name?, issued_at, expires_at, sig64}`다. IdentityKey 서명 대상은 모든 필드다. endpoint_hint는 cert의 transport_pub와 같아야 하며 최대 한 개의 HTTPS relay URL만 포함한다. 공용 초대에는 직접 IP 주소를 싣지 않는 것으로 제안한다. 이름은 생략 가능하고 UTF-8 **64바이트 이하**이며 서명 후 그대로 검증한다. 한글 글자 수와 바이트 수를 혼동하거나 잘라 저장하지 않는다. 제어문자/개행은 거부하고 bidi 제어는 표시 단계에서 경고/격리한다.

기본 만료 24시간, 최대 7일, issued/expires 역전과 5분 초과 미래 발행을 거부한다. 서명·cert·상한을 검증한 뒤 현재 시각으로 만료를 검사한다. 입력의 decoded payload는 최대 4096B, relay URL 256 UTF-8 B다. 알 수 없는 scheme/version은 네트워크 요청 없이 오류다. 받은 URL은 일반 HTTP fetch/browser navigation으로 열지 않으며 iroh relay 주소 검증만 거친다.

QR·딥링크·붙여넣기 모두 같은 Rust parser를 사용한다. 파싱만으로 연락처를 저장하거나 연결하지 않는다. 화면 7에서 초대 이름을 기본값으로 보여 주고 수정한 뒤 “연락처 추가”를 누르면 저장한다. 초대 이름은 상대의 인증된 실명이 아니며 수신자의 로컬 별칭이다. 같은 PeerId를 다시 추가하면 중복을 만들지 않고 기존 이름을 자동 덮어쓰지 않는다. 만료/위조 초대는 연락처·verified 상태를 변경하지 않는다.

연락처 추가는 오프라인에서도 가능하나 상태는 `기기 확인 전`이다. 연결 시 §5.2의 account record/cache와 cert/proof를 검증해 표시를 갱신한다. 초대가 오래된 기기를 가리키면 현재 목록을 다시 조회하고 인증 실패한 hint로 우회하지 않는다. M3의 “추가됨”은 MLS 암호화 대화 생성/메시지 송수신 완료라는 뜻이 아니다.

안전번호는 `SHA-256("ChatProject/safety/v1" || min(PeerId32_A,PeerId32_B) || max(...))`의 전체 32B를 hex 64자, 8자씩 8묶음으로 표시하는 제안이다. 양쪽이 같은 순서로 표시하고 QR에는 전체 digest와 두 PeerId를 담는다. 사용자가 독립 채널에서 전부 대조하고 “확인했어요”를 눌러야 해당 peer/key 조합을 verified로 저장한다. 초대 서명만으로 verified가 되지 않는다. 기기 변경·같은 PeerId 복구는 안전번호가 같고 “새 기기” 안내가 뜬다. 다른 PeerId는 새 연락처/안전번호이며 기존 검증을 이어받지 않는다.

## 7. 기기 연결 — 제안

M3 pair 채널은 M2 진단 메시지와 별도 application protocol/version으로 구분한다. iroh가 보호하는 스트림 위에 **`Noise_NNpsk0_25519_ChaChaPoly_SHA256`**를 사용한다. QR에 있는 32B random 일회용 secret을 PSK로 쓰며, 자체 DH/암호화 프로토콜을 조합하지 않는다. 공식 [Noise §9·§11.2](https://noiseprotocol.org/noise.html)에서 PSK pattern과 handshake-hash 기반 서명 channel binding을 확인했다. 검토된 Rust 구현의 해당 pattern/MSRV/벡터 지원 확인은 구현 계획의 선행 조건이다.

### 7.1 역할과 QR

| 방향 | QR 표시 / Noise responder | QR 촬영 / Noise initiator | 승인 권한 |
|---|---|---|---|
| 기본 | 새 기기 N | 기존 기기 E | E의 로컬 화면에서만 |
| 반대 | 기존 기기 E | 새 기기 N | **QR를 띄운 E의 로컬 화면에서만** |

새 기기는 §4처럼 candidate DeviceSeed를 먼저 저장하고 해당 TransportKey로 bind한다. QR은 `{version, direction, session_id16, qr_owner_transport32, qr_owner_installation32, existing_peer_id?, resume_pair_tx_id?, relay_hint, expires_at, secret32}`다. 기존 기기 QR에는 PeerId가 있고 새 기기 QR에는 없다. resume tx는 §7.3 재개에만 있다. base64url 전용 pair scheme, decoded 2048B 이하, 5분 만료다. QR 소유자는 대응하는 로컬 session이 살아 있을 때만 수락한다. 앱 재시작·취소·만료 시 세션/PSK는 폐기하고 자동 복원하지 않는다.

스캐너는 QR의 EndpointId를 pin해 연결한다. 양쪽은 시작 메타데이터를 교환하고 prologue에 version·direction·session·역할 E/N·양쪽 EndpointId/installation_pub·기존 PeerId·resume tx(있으면)·QR 만료를 넣는다. PSK는 prologue가 아닌 Noise PSK 입력으로 준다. EndpointId는 실제 연결의 remote와 일치해야 한다. E의 cert/유효 수용 목록과 양쪽 InstallationKey의 handshake-hash 서명을 검증한다. E의 **기존 공개 cert**, 공개키·목록·nonce·proof는 승인 전 교환 가능하다. N의 서명은 자기 installation 키 소유 증명이지 계정 권한 증명이 아니다.

### 7.2 확인과 키 전달

Noise handshake hash를 salt, QR secret을 IKM, info=`ChatProject/pair-sas/v1`, L=5로 HKDF-SHA256을 수행한다. 결과 40비트의 RFC4648 base32 대문자 8자를 `ABCD-EFGH`처럼 양 화면에 표시한다. 코드에는 방향·두 기기 공개키·기존 PeerId가 transcript로 묶여 있다. 확인 코드를 식별 가능한 기기 이름/플랫폼과 함께 읽어 준다. 코드를 내 기기 화면 밖 상대방이 대신 확인해 준 것으로 승인하지 않는다.

상태는 `QR 대기 → 채널 검증 → 코드 비교 대기 → E 승인됨 → 키 전달 → N 보관 확인 → 목록 게시 → 활성`이다. **두 방향 모두 E의 명시적 로컬 “이 기기 연결” 승인 전에는 IdentityKey 개인키·연락처·N에게 새로 발급한 cert를 N에 전달하지 않는다.** E의 기존 공개 cert는 이 gate 대상이 아니다. N의 “일치” 응답이나 원격 조종 메시지는 E 승인으로 대체할 수 없다. N도 로컬 코드 확인 후에만 수신에 동의한다. 불일치/거절 시 세션을 폐기하고 새 QR을 요구한다.

한 세션은 한 후보에만 묶고, 후보 교체는 QR 재발급으로만 한다. handshake 실패 3회 또는 세션 만료 시 폐기한다. 기본 5분 전체 만료, 승인 후 전달/저장 확인 60초 타임아웃이다. 이미 사용한 session 재전송·역할 뒤집기·다른 endpoint에서의 승인 재사용·이전 hash 서명을 거부한다. Noise message nonce/AEAD 검증 실패는 채널 종료다. 사진으로 QR를 훔친 상대가 먼저 연결하면 승인을 기다리는 방해는 가능하며 자동 승인으로 풀지 않는다.

E는 **§7.3의 승인 transaction/노출 가능 표식 저장이 성공한 뒤에만** Noise transport로 `{pair_tx_id16, IdentityKey signing_seed32, N의 서명된 cert, account generation/base, 연락처 snapshot}`를 N에 보낸다. RecoverySeed/문구/DeviceSeed는 보내지 않는다. N은 키에서 PeerId를 재계산하고 자신의 두 공개키와 cert가 일치하는지 검증한 뒤 pending import와 receipt를 OS 보호 저장하고 ack한다. 같은 tx 재전송은 중복 생성 없이 같은 결과를 돌려주며 다른 내용은 거부한다. 연결 중 넘기는 연락처는 수신 snapshot 한도 안에서만 허용한다.

E는 N의 저장 ack 후 pending add를 발행한다. N은 최신 record에서 자기 기기를 확인하기 전 `연결됨 · 목록 게시 대기` 상태이며 일반 계정 활동을 시작하지 않는다. 네트워크 실패는 저장된 intent를 재시도/충돌 확인한다. 전달 전 취소에는 키 노출이 없지만, **전달 뒤 취소·타임아웃·게시 경쟁에서는 N이 IdentityKey 사본을 받았을 수 있다.** UI에 이를 알리고 N 데이터 폐기 안내/목록 제거를 제공한다. 삭제 ack만으로 원격 비밀 사본 소거를 보장하지 않는다. 키 노출이 의심되면 §5.1의 새 아이덴티티 권고를 따른다.

N 저장 직전/직후, E ack 수신 전/후, 게시 전/후의 crash에서 성공을 미리 알리지 않는다. 세션은 재시작 후 재사용하지 않으며 §7.3의 새 세션으로 저장된 pair_tx를 reconcile한다. E/N 양쪽이 없어지면 사용자는 문구로 복구해야 한다. 여러 기기를 동시에 연결해 max5를 넘는 레코드를 만드는 것은 오류다.

### 7.3 전달 transaction·재시작·미활성 import 폐기 — 제안

E는 첫 private-key byte를 보내기 **전에**, `pair_tx_id`, E/N의 정확한 PeerId·transport/installation 공개키, N cert/payload hash, 승인한 base hash/generation, 로컬 사용자 승인 사실, 상태=`disclosure_possible`을 하나의 durable snapshot으로 저장한다. 이 상태는 송신 전 crash여도 **키가 전달됐을 가능성이 있음**으로 보수적으로 취급한다. 저장 실패 시 gated payload 0B다. E는 receipt 검증 결과/게시 intent/관측한 게시본도 순서대로 저장한다. 이미 승인한 키가 달라지면 같은 tx가 아니다.

N은 candidate 키와 별도로 E의 PeerId·transport/installation 키 및 tx pin을 저장한다. import 수신 시 검증한 payload/hash·IdentityKey·N cert·연락처·상태=`receipt_ready`를 atomic snapshot으로 확정한 뒤에만 해당 tx/hash의 저장 ack를 보낸다. ack 송신 여부가 불확실해도 durable receipt는 재사용할 수 있다. E는 현재 채널 proof와 일치하는 receipt를 저장한 뒤 add 게시로 진행한다. N의 상태는 `candidate → receipt_ready/게시 대기 → active`이며 최신 수용 record에서 자신의 정확한 키 쌍을 확인해야 active가 된다. E/N 화면은 서로의 receipt/게시 상태를 구분한다.

**같은 pending N 재개:** 어느 쪽이 재시작했든 사용자가 “중단된 기기 연결 재개”를 골라 **새 QR·새 secret/PSK·새 session·새 Noise handshake·새 SAS와 E의 새 로컬 승인**을 수행한다. 이전 PSK/session/승인 서명은 복원하지 않는다. E는 durable tx에 pin된 N의 두 키/tx와 fresh handshake-hash proof가 정확히 같은 경우만, N은 pin된 E의 키/PeerId/tx가 같은 경우만 허용한다. 일반 account membership에 아직 없는 N을 허용하는 이 예외는 **그 tx의 receipt 확인·동일 payload 재전송·게시 결과 확인만** 가능하며 일반 sync·새 연락처 데이터 조회·다른 기기 추가/해제를 허용하지 않는다. 최초 전송 payload와 다른 내용은 새 tx/새 연결 절차로 다시 승인해야 한다.

E는 재개 때 현재 목록/base·자신의 활성 권한·남은 5대 한도를 다시 확인한다. base 변경/해제/새 generation이면 자동 게시하지 않고 목록 충돌/해제 상태로 보류한다. N이 이미 포함된 정상 게시본을 확인하면 중복 추가 없이 active로 정리한다. E가 unavailable이거나 tx를 잃었다면 N에게 무한 재개 대기를 요구하지 않는다.

**미활성 pending import만 폐기 가능:** N이 아직 active로 전환한 적 없을 때 로컬 “중단된 연결 버리기”로 해당 import/receipt·candidate DeviceSeed·보호 blob/전용 OS secret을 제거하고 미설정 화면으로 돌아간다. E에는 제거된 원격 사본의 소거를 보장할 수 없으며, E의 `disclosure_possible` 이력/보안 경고도 삭제하지 않는다. 삭제 실패는 완료로 표시하지 않는다. 공개 목록에 이미 포함됐다면 N의 로컬 폐기가 그 목록을 철회하지 않으므로 E에서 해제해야 함을 안내한다. E 없이도 N은 이 폐기 후 보관한 문구를 통한 **일반 복구**를 선택할 수 있다. 이 예외는 활성 계정 삭제/덮어쓰기 API가 아니다.

## 8. 해제와 복구의 사용자 동작

기기 목록에는 현재 기기, 플랫폼, 마지막 확인, 게시/충돌 상태를 보여 준다. 기기 별칭은 로컬 정보다. **제안:** 현재 기기를 이 화면에서 해제하지 않으며 마지막 기기를 삭제하는 계정 폐기 기능도 넣지 않는다. 다른 기기 해제 시 대상과 “이 기기가 계정 키를 가졌을 수 있어요” 경고를 확인하고, 로컬 차단을 저장한 뒤 공개 목록 변경을 요청한다.

M3 상태는 `이 기기에서 차단됨 / 공개 목록 갱신 대기 / 공개 목록 반영 확인 / 목록 충돌`을 구분한다. M5 이전에는 “모든 대화에서 제거됨” 또는 “새 메시지를 더는 읽을 수 없음”이라고 말하지 않는다. M5·M9에서 각 대화 제거 요청/commit 확인을 별도 상태로 붙인다. 상위 설계처럼 제거 commit 전에는 분실 기기가 새 메시지를 읽을 수 있다.

복구 화면은 “계정 ID는 돌아오지만 내 연락처와 대화 기록은 돌아오지 않아요”를 입력 전에 설명한다. 문구 검증 → 같은 IdentityKey/PeerId → 새로운 DeviceSeed/cert → OS 저장 → §5.1 replacement 게시 순서다. 새 기기의 EndpointId/installation_id는 이전과 달라야 한다. 계정이 있는 폴더에는 덮어쓰지 않는다. 오프라인 복구도 로컬 계정 복원과 게시 대기를 구분한다.

대화 상대가 같은 PeerId를 계속 알고 있으면 나중에 새 기기를 발견할 수 있지만, M3에서 연락처 목록 자동 복원·과거 기록 다운로드·MLS 재참가가 이미 된 것으로 보여 주지 않는다. “내 기기가 전부 없어진 상황”과 “잠금 해제된 기기가 타인 손에 간 상황”을 구분하며 후자는 같은 PeerId 복구만으로 안전해지지 않는다고 안내한다.

## 9. 화면·접근성·Rust 경계

승인된 화면 ID를 기준으로 기능을 정의하며 비공개 목업을 재설계하거나 서버/시각 mockup을 띄우지 않는다. Windows 넓은 화면은 목록+상세, 모바일은 한 단계 한 화면으로 같은 상태/명령을 제공한다. 뒤로 가기는 key-transfer 이전 취소인지 이후 해제 안내인지에 따라 문구를 달리한다.

| 화면 | M3 동작 |
|---|---|
| 1–3 시작·문구·확인 | 새 계정/복구/기기 연결 선택, 24단어 보관 경고, 위치별 확인, 저장 실패/재시작 복구 |
| 6–8 QR·이름·안전번호 | 초대 표시/스캔/붙여넣기, 이름 수정 후 연락처 추가, 전체 안전번호와 수동 확인 |
| 22–28 연결·해제·복구·경고 | QR 방향 전환, 8자 코드, 기존 기기 승인, 5대 목록, 단계별 미완료/손실 경고 |

키보드만으로 이동/승인, 스크린리더의 필드 순서와 오류 연결, 200% 텍스트 확대, 색 외의 상태 문구를 지원한다. QR 카메라가 없는 Windows에서는 반대 방향을 기본 제안하고 붙여넣기 입력 경로도 제공한다. 안전번호/코드의 QR·텍스트를 같은 원본에서 그린다. 복구 문구를 스크린리더로 읽는 동작은 사용자 명시 요청 시만 수행한다.

M1a 후속의 ErrorCode→한국어 대응표를 넣는다. 예: 문구 오류 “복구 문구의 단어와 순서를 확인해 주세요”, 위조 “서명을 확인할 수 없는 초대예요”, 만료 “초대가 만료됐어요. 새 초대를 받아 주세요”, 용량 “연결할 수 있는 기기는 최대 5대예요”, 코드 불일치 “확인 코드가 달라요. 새 QR로 다시 연결해 주세요”, 충돌 “다른 기기에서 목록이 바뀌었어요. 현재 목록을 확인해 주세요”. 내부 enum/원시 키/OS 오류는 사용자 문구에 삽입하지 않는다.

Rust는 create/recover, 초대 parse/accept, 안전번호 확인, pair 시작/입력/로컬 확인/취소, 기기 해제/게시 재확인 명령을 소유한다. `accept_invite`가 M3에서 ConversationId를 꾸며 반환하지 않도록 연락처 결과를 별도 의미로 둔다. 일반 상태는 기존 단일 events stream의 account/contact/pair/device 상태와 구조화 error로 전달한다. 화면 진입 시 snapshot을 읽고 후속 event sequence로 갱신한다. duplicate/과거 event로 승인을 되돌리지 않는다.

문구·QR secret·private seed는 일반 event/log/진단 snapshot에 싣지 않는다. 최초 문구/해당 세션 QR 표시용 응답만 필요한 화면에 일시 전달한다. production 명령으로 E 승인 자동화를 노출하지 않는다. test-hooks는 기존 이중 gate 안에서만 같은 코어 동작을 조종하며 Release에 secret 조회/가짜 승인/결정적 RNG 경로를 넣지 않는다. iOS NSE에는 M3 account/pair/DHT 기능을 추가하지 않는다.

## 10. 완료를 판단할 증거 — 아직 실행하지 않음

아래는 향후 승인된 구현 계획의 수용 계약이다. 이 문서를 쓰며 코드·빌드·시험·장치·공개 DHT/릴레이 통신을 수행했다는 뜻이 아니다.

| 층 | 요구 증거 |
|---|---|
| Rust 단위/여러 참가자 | 공개 BIP-39 entropy round-trip/파생 golden vectors, 같은 문구→같은 PeerId, DeviceSeed별 key 분리; cert/초대/서명 domain·길이·version·만료·변조 거부 |
| 보안 상태 | 양쪽 같은 코드, 양방향 E 승인 전 **IdentityKey 개인키·연락처·N 신규 cert 0B**(E 공개 cert 등 허용); replay/다른 endpoint/역할 바꾸기/QR 탈취 후보/거절/만료/3회 실패, 일반 접속 exporter proof의 세션 간 재사용 거부 |
| record/복원력 | 5대 실제 DNS/wrapper byte 수, 6대 거부, IP 미포함, concurrent add/revoke/recover 충돌; 최초 게시/7일 넘은 같은 generation 갱신/문구 replacement; 조회 불가와 정상 결과 없음 구분, durable high-water·rollback/tombstone 한도, cached auth/만료 제한/제거·generation 변경 시 권한 무효화 |
| 저장/플랫폼 | OS 잠김/키 없음/손상, backup 제외, 플랫폼별 단일 writer와 iOS suspend 전 lock 해제, atomic replace crash; E disclosure_possible 저장 실패 시 gated 0B, N receipt/ack 유실·E 재시작 후 새 QR/SAS/승인으로 같은 tx 재개, 다른 N 거부, pending 폐기/복구 경로, candidate/활성 재시작 EndpointId 유지·instance_id 변경 |
| Flutter | 생성·문구 확인·64/65 UTF-8 B·중복 연락처·안전번호 미검증/검증·pair 양방향 승인·해제 경고·복구/게시 대기·한국어 오류·키보드/큰 글자 |
| 자동 시나리오④ | 기존 로컬 relay·가짜 record resolver + Android 에뮬레이터 2대/`chat_node`: 초대→연락처, 두 방향 연결, 재시작, 해제, 같은 PeerId 복구. QR 내용만 지휘 프로그램이 전달, 카메라 없음 |
| 실제 플랫폼⑤ | Windows↔Android 양방향 QR·해제·복구; iOS signed 앱의 Keychain/첫 unlock/저장·QR·같은 PeerId; 공개 pkarr 왕복은 별도 허가된 실행 때만 |
| 회귀 | M2 진단 인터페이스/Android JNI 및 기존 binding 재생성·Release 시험 표식 차단 정책 유지. M2 미해결 latency/LAN 증거를 M3 성공으로 대체하지 않음 |

시간 관련 단위 검증은 주입된 시각/타이머로 수행하도록 제안한다. 이는 M4 Clock 후속의 최소 보안 검증 부분만 앞당기며 outbox clock_advance/가짜 메시지 네트워크는 M4·M5에 남긴다. CI는 인터넷 DHT·공개 relay를 성공 조건으로 삼지 않는다. native 실제 저장 시험은 메모리 fake 시험과 구분한다. iOS Simulator 결과를 signed 물리 iPhone 증거로 바꿔 쓰지 않는다.

## 11. 승인 대상과 구현자가 남겨 둘 일

2026-10-18 사용자의 “승인”은 전체 문서와 **별도 M3 초대 타입**, **M4 전 최소 암호화 영속 상태/lock/백업 정책**, **record의 2단계 조회와 명시적 충돌 보류**, **표준 PSK pairing + 두 방향 기존 기기 승인**, **같은 PeerId 복구/해제의 한계**를 포함한다. 제품 임시 표시 이름은 앞선 사용자 결정대로 ChatProject다.

구현자는 승인 뒤 별도 계획에서 정확한 Rust crate/version/MSRV·플랫폼 native API·wire 배열과 golden vectors·pkarr timestamp/크기·시험 파일 배치를 확정한다. primitive를 바꾸거나 사용자 권한/복구 의미·기기 상한·비밀 전달 조건·오프라인 충돌 동작을 바꾸는 선택은 이 상세 설계의 갱신 대상이다. 일반 API 이름/파일 분할/한국어 조사 선택은 구현자의 재량이다.

**현재 결과는 builder 검토와 사용자 전체 승인을 마친 설계다. 다음 단계는 별도 구현 계획 작성·검토다. M2의 열린 수용 항목은 이 승인으로 면제하지 않는다. 실제 코드 착수 범위·작업 격리·선행 조건은 구현 계획에서 명시한다.**
