# M2 전송 검증 현황과 남은 게이트

2026-10-16 한국 시간 기준 진행 기록이다. Tasks 1–12는 구현·검토를 마쳤고 Task 13 코드 지적은 다섯 번째 수정까지 닫았다. 실제 Android 야간 시험, 물리 기기 전체 행렬, 서명된 M2 TestFlight/iPhone 시험과 전체 브랜치 최종 검토는 남아 있다. Task 14와 M2는 완료 상태가 아니다. [39개 인계 항목과 후속 책임](../plans/2026-10-13-m2-carry-forward.md)을 함께 읽는다.

제품 코어 기준은 [`751bb17`](https://github.com/HyunwookYoo/chatproject/commit/751bb173623b627b85d8b688df99bccd8ba65bdf)이다. 이후 `3691396`은 시험 ready-file 한 곳, `21e806f`는 AVD 유한 진단, `41e7954`는 남은 시험 port-file 세 곳, `3780025`는 Android 야간 작업의 AVD 홈 준비를 고쳤다. 제품 바이너리는 계속 **751bb17**이며 이후 HEAD로 이름을 바꾸지 않는다. 진단·fixture·workflow 수정의 승인과 실제 호스팅 성공도 구분한다.

아래 로컬 증거 경로는 gitignored `build/e2e/` 안에 보관되어 담당자가 열람할 수 있다. 공개 다운로드 링크는 아니며 티켓·EndpointId·주소·기기 serial·원시 콘솔은 공개하지 않는다. CI 근거는 실제 실행 링크로 남긴다. 이 문서 작성 중 빌드나 광범위 시험을 다시 실행하지 않았다.

## 빌드와 기기

일반 제품은 `lib/main.dart`로 빌드했다. Windows Release 전체 폴더 14개 파일과 Android ARM64/ARMv7 Release APK를 `build/e2e/task14-preparation/751bb17/`에 따로 보존했다. 전용 `Stopped`/`NotAvailable` 계약 산출물과 섞지 않는다. APK는 65,632,255 B, SHA256 `057a1546472b1307b7fe40dd43374358bf2f5d1773230710fefc0d3601d9b488`이다. Windows 전체 파일 해시는 같은 폴더의 `SHA256SUMS`와 `artifact-index.json`에 있다.

Windows 앱은 `1.0.0+1`, Android는 `1.0.0`(1), 코어는 `0.1.0`이다. 실제 Rust 명령은 Windows·ARMv7·ARM64 모두 `rustup run 1.92.0 cargo build`였고 제품 빌드에 test-hooks를 켜지 않았다. Android minSdk 26/targetSdk 36, nondebuggable, INTERNET/ACCESS_NETWORK_STATE와 두 ARM ABI의 다섯 네이티브 라이브러리를 확인했다. Windows 네이티브 5개와 APK 네이티브 13개에서 조종 표식이 없었다. APK의 부수 x86_64 라이브러리 세 개는 완전한 x64 Flutter 실행 지원을 뜻하지 않는다. 현재 APK는 기존 로컬 debug-key 서명 설정이며 배포 서명 증거가 아니다.

실제 Android는 SM-F971N, Android 17/API 37, ARM64 전용이다. 설치된 APK 해시가 위 751 산출물과 같고 Windows Home Ethernet↔Android KT LTE에서 양쪽 `Ready`를 확인했다. Android Wi-Fi나 Windows USB 네트워크 경로를 사용하지 않았다. USB 분리·재연결 동안 두 프로세스와 키가 유지된 사실은 전송 장애·복구 시험과 구분한다. Windows는 MSI MS-7D75, Microsoft Windows 11 Pro 10.0.26300/build 26300, 64-bit/x64 PC다. 비식별 기기 사실은 로컬 `build/e2e/physical-2026-10-16/windows-device-sanitized.json`에 보관했다. 표본 뒤 실제 기본 경로도 물리 Ethernet임을 확인했다.

## 실행 결과

`path`는 실제 관찰 경로 또는 시험이 한정한 경로이며, 미실행 행은 미측정이다. Debug 자동 시험, 일반 Release, 전용 Release 계약과 실제 공개망은 각각의 행으로 구분한다.

| platform | build | corecommit | testcase | path | expected | actual | evidence | decision |
|---|---|---|---|---|---|---|---|---|
| Rust·Windows·Android·iOS·bindings | 일반 CI | 3691396 (제품 751) | 전체 9개 작업 | 격리 자동 시험 | 9/9 성공 | 9/9 성공; tools 124 PASS/5개 명시적 플랫폼 skip, 앱 widget 57 PASS·analyze clean | [CI 38900998746](https://github.com/HyunwookYoo/chatproject/actions/runs/38900998746), [CI 정의](../../../.github/workflows/ci.yml) | 해당 실행 통과 |
| Rust·Windows·Android·iOS·bindings | 일반 CI | 21e806f (제품 751) | 진단 변경 뒤 CI | 격리 자동 시험 | 9/9 성공 | native·Windows·Android·iOS Simulator/unsigned Release·Mac/Linux bindings·workflow 등 8/9 성공; tools만 inherited scalar-port 빈 파일 읽기로 실패 | [CI 38905204063](https://github.com/HyunwookYoo/chatproject/actions/runs/38905204063), [process fixture](../../../tools/e2e/test/process_runner_test.dart) | 실패 보존; 41e7954 수정의 Linux 실제 재검증은 후속 378 CI에서 통과 |
| 전체 CI | 일반 CI | 3780025 (제품 751) | fixture/AVD 홈 수정 뒤 CI | 격리 자동 시험 | 전체 성공 | tools 141 PASS/5개 명시적 플랫폼 skip·analyze clean, 앱 widget 57 PASS·analyze clean; 다른 작업 실행 중 | [CI 38908404372](https://github.com/HyunwookYoo/chatproject/actions/runs/38908404372) | Linux fixture 재검증 통과; 전체 결과 대기 |
| Windows 2개 | Debug nightly | 21e806f (제품 751) | Windows 야간 시나리오 | 로컬 relay | 양방향·복귀·소유 자원 정리 | 성공 | [nightly 38905206591](https://github.com/HyunwookYoo/chatproject/actions/runs/38905206591), [시나리오](../../../tools/e2e/bin/run.dart) | 해당 Windows 실행 통과; 378의 Android 전용 workflow 수정과 제품/Windows 런타임은 동일 |
| Linux/Android 2 AVD | Debug nightly | 21e806f (제품 751) | Android 야간·메모리 | 로컬 relay 예정 | 두 AVD 시나리오 성공·힙 판단 | 앱 설치 전 `avd_create` 실패, `avd_config_missing`, raw exit code null; 전체 시나리오 미실행 | [nightly 38905206591](https://github.com/HyunwookYoo/chatproject/actions/runs/38905206591), 해당 실행 요약 artifact `12048248308` | 실패; 기존 호스트 원인은 미확정 |
| Windows/Android | Debug nightly | 3780025 (제품 751) | AVD 홈 준비 뒤 야간 | 로컬 relay | 두 프로필 성공 | 수동 실행 중; 결과 미수신 | [nightly 38908406908](https://github.com/HyunwookYoo/chatproject/actions/runs/38908406908), [야간 정의](../../../.github/workflows/e2e.yml) | 새 Android 실제 증거 대기 |
| Windows·Android x64 emulator | 일반/전용 Release | 751 | Home/native·계약·표식·JNI/권한 | 공용망 없는 계약 | 일반 Home; 전용 Stopped/NotAvailable; 표식 없음 | 실제 Release smoke·계약 성공, Android nondebuggable; Debug 채널 가드와 R8/JNI 검사 통과 | [Release 시험](../../../tools/ci/release_smoke.dart), [계약](../../../app/integration_test/release_contract_test.dart), [369 CI](https://github.com/HyunwookYoo/chatproject/actions/runs/38900998746) | 자동 Release 범위 통과; 물리 공개 DNS의 대체 증거 아님 |
| Windows·Android ARM64/ARMv7 | 일반 Release | 751 | 정상 제품 빌드·해시·표식 | 정적 산출물 | 전체 폴더·두 ABI·Rust 1.92 | Windows 14파일/ARM APK 보존; 해시·권한·marker 검사 통과 | 로컬 `build/e2e/task14-preparation/751bb17/{artifact-index.json,SHA256SUMS}`, [검사기](../../../tools/ci/check_release_hooks.py) | 빌드 통과; ARMv7 실제 실행은 호환 기기 확보 후 배포 전 유예 |
| Windows↔SM-F971N | 일반 Release | 751 | 실제 설치·시작·ticket 연결 | Home Ethernet↔KT LTE; 관찰 Direct | ARM64 정상 실행·연결 | 설치 SHA 일치, 양쪽 Ready, fresh ticket 입력 12회 모두 connected/Direct, 측정 전송 실패 0회 | 로컬 `build/e2e/physical-2026-10-16/real-network.jsonl` | 연결 관찰 완료; 전체 물리 행렬은 미완료 |
| Windows↔SM-F971N | 일반 Release | 751 | 동일 절차 ticket 10회 성능 | Direct | connected ≤500 ms / Direct ≤1000 ms | connected 386–6728 ms, Direct 510–6881 ms; 각각 7/10·2/10 목표 미달 | 같은 로컬 JSONL의 동일 절차 10회 집계 | 목표 통과 아님; 원인 미확정·사용자 판단 대기 |
| Windows↔SM-F971N | 일반 Release | 751 | 양방향 한글·4096 UTF-8 B·송수신 수/본문 일치 | 실제 공개망 | 정확한 본문·개수 | 시험 진행 중, 확정 결과 미수신 | 로컬 물리 시험 증거 수집 중 | 대기 |
| Windows↔SM-F971N | 일반 Release | 751 | hostname DNS·EndpointId 단독 DHT·Relay fallback | ticket 성능과 별도 | 각 조회/경로 실증 | 독립 사례 결과 미수신; Direct 표본만으로 DHT-only/Relay fallback 성공을 주장하지 않음 | [공개망 구성](../../../core/chat_core/src/transport/endpoint.rs), 로컬 물리 시험 증거 수집 중 | 대기 |
| Windows↔SM-F971N | 일반 Release | 751 | Wi-Fi→LTE→Wi-Fi·offline 복귀·실패 글 미재전송·수동 disconnect·프로세스 재시작 | 실제 공개망 | 복귀 뒤 새 글 수신; 같은 프로세스 ID 유지/재시작 새 ID·새 주소 입력; 자체 재시도 없음 | USB 중단은 별도 관찰일 뿐; 요구된 전송·수명 사례의 확정 결과 미수신 | 로컬 물리 시험 증거 수집 중 | 대기; disconnect 뒤 새 원격 inbound 허용은 자체 재시도와 분리 |
| iOS Simulator/unsigned iOS Release | 자동 CI | 751 | integration·public API linking·Rust 버전 | 격리 자동 시험 | Simulator 시험/unsigned 빌드·Rust 1.92 | Simulator 3개 시험, unsigned Release 및 실제 aarch64 Rust 1.92 명령 성공 | [369 CI](https://github.com/HyunwookYoo/chatproject/actions/runs/38900998746), [CI 정의](../../../.github/workflows/ci.yml) | 자동 범위 통과; signed M2 증거는 별도 |
| Windows↔iPhone | signed M2 TestFlight | 후보 미확정 | 전경 양방향·LAN 거절 후 복구·오류 표시·서명 빌드 Rust 로그 | 실제 공개망 | 실제 iPhone 실행·Rust 1.92 | 미실행 | [기존 TestFlight 정의](../../../.github/workflows/testflight.yml) | 게시 승인·Environment owner review·실제 기기 시험 대기 |

## 실제망 표본의 해석

연결 시간은 Rust의 같은 로컬 monotonic clock에서 connect 시작 직전부터 magic-ready 뒤 connected까지, Direct 시간은 그 시작부터 첫 selected-Direct 관찰까지다. 다른 기기의 wall clock을 빼지 않았다. 원본 JSONL은 `sample,connected_ms,direct_ms,path,success,error_code` 여섯 필드이며 식별 정보는 별도 비공개 기록에 있다.

최초 두 번과 절차를 고정한 다음 열 번을 모두 보존했다. 최초 두 번 집계는 connected 238–3635 ms/Direct 239–3747 ms, 두 목표 모두 1/2 미달이다. 주 평가 열 번은 Android Stop→Stopped, Windows 수동 Disconnect→Disconnected, Android Start→Ready, 새로 전달한 ticket 입력→새 connect 순서로 반복했다. 최초 두 번을 폐기하거나 유리한 열 번을 골라 대체하지 않았다. 12회 모두 연결·Direct 관찰에 성공했지만 이는 성능 목표 통과를 뜻하지 않는다. 목표 미달의 원인은 미확정이며 cold relay 같은 가설을 결론으로 쓰지 않는다. EndpointId 단독 DHT 시간은 이 ticket 표본에 섞지 않는다.

## 야간·iOS·마무리 게이트

Android 야간의 이전 751 재시도도 이미지 설치 뒤 `avd_create/state/null`에서 실패했다(정확한 요약 artifact `12045089707`; 최초 준비 실패 artifact와 다름). 당시 Gradle peak 3,159,376 KiB, GNU time child peak 1,195,920 KiB였고, 21e 실행의 GNU time max-child는 1,198,160 KiB였다. 이들은 동시 총메모리도 Gradle 힙 실사용의 동일 측정값도 아니며 OOM 증거가 아니다. 빌드 후 소유 Gradle daemon 정리·AVD 순차 준비·RSS 계측은 구현했지만 실제 Android2 성공과 `-Xmx8G` 유지/변경 판단은 남아 있다. 3780025는 기존 Android 단계에서 `HOME/.android/avd` 생성과 `ANDROID_AVD_HOME` export를 더했다. SDK12의 홈 계약 공백은 소스로 확인했으나 과거 호스트 실패 원인까지 확정한 것은 아니다.

41e7954의 세 writer stage→rename 수정은 집중 시험 6개/analyze/검토, 3780025는 workflow 검사 30개·Dart override 1개·Bash 구문/검토를 통과했다. 이후 378의 실제 Linux tools 작업 141 PASS/5 skip·analyze clean과 앱 57 widget PASS·analyze clean을 확인해 Linux fixture 게이트는 닫았다. 전체 CI와 Android 야간 성공은 아직 별도다. 위 진행 중 실행의 결론은 담당자가 전달한 실제 결과로 갱신한다.

TestFlight Environment는 `main` 또는 `testflight-*` tag와 매 실행 owner review를 요구한다. M2 tag·upload 승인은 아직 없고 feature dispatch를 게시 가능으로 간주하지 않는다. 실제 서명된 후보를 검토·승인한 뒤 Windows↔iPhone 전경/LAN 거절·복구와 signed Rust 1.92 로그를 확보해야 한다. 기존 M1b iPhone/NSE/APNs 수치는 [기존 APNs 결과](../../../spikes/c-push/RESULTS.md)에 남는 별도 측정이며 M2 채팅 성공으로 합산하지 않는다. iOS 백그라운드 수신 보장은 없다.

물리 시험 보고서·성능 미달 원인/사용자 판단·최신 CI와 두 야간 프로필·signed iPhone·전체 브랜치 최종 검토를 확인한 뒤에만 완료를 판단한다. M4 폴더 독점 잠금, M7 Release Kotlin/JNA/UniFFI push 진입점, ARMv7 실제 실행, Apple entitlement 신청 유예는 [인계 문서](../plans/2026-10-13-m2-carry-forward.md)에 유지했다.
