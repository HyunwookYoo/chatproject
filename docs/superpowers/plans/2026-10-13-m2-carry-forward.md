# M2 검증 현황과 남긴 일

2026-10-17 한국 시간 기준, 10월 16–17일 세션의 현황이다. 구현 Tasks 1–12와 Task 13 코드 지적 수정은 검토를 마쳤다. 7a Windows·Android 야간은 통과했으며 Task 13 native 전체 최종 판정에는 최신 일반 CI Android 결과가 남아 있다. Task 14의 EE 성능 수용 판단·실제 실패한 fresh-client DHT 등 공개망·서명된 iPhone·최종 브랜치 검토는 열려 있다. 751 물리 본문/복귀/이동/해제/재시작 결과와 EE 영향 경로는 분리한다. **M2 완료 선언이 아니다.** 실행별 범위·실패·산출물 정체성은 [현재 결과](../results/2026-10-16-m2-transport.md)에 기록했다.

아래 A1–A28은 [M1a M2 #1–28](2026-10-05-m1a-carry-forward.md#m2), B1–B11은 [M1b M2 #1–11](2026-10-11-m1b-carry-forward.md#m2)과 같은 번호다. 완료는 해당 인계 요구의 구현·시험 근거를 뜻하며 전체 마일스톤이나 모든 실제 기기 검증의 완료를 뜻하지 않는다. **39개 중 36개 완료, 대기 0개, 3개 유예(A10/A11/B8)**다. A28은 실제 두 AVD 야간 성공·별도 RSS 측정과 Xmx8G 유지 판단으로 항목별 완료했다. A5는 원래 ARM 빌드·ARM64 실제 실행 요구 범위에서 완료하며, 승인된 ARMv7 실제 실행 유예와 별도의 Task 14 공개망·성능 게이트를 유지한다. 부분 완료와 별도 게이트를 아래에 남긴다. 로컬 비공개 증거는 `build/e2e/`에 보존하며 공개 링크로 제공하지 않는다.

## M1a에서 받은 28개

| ID | 요구와 판정 | 유지되는 구현·시험 근거 / 남은 조건 |
|---|---|---|
| A1 | INTERNET — 완료 | [주 manifest](../../../app/android/app/src/main/AndroidManifest.xml), [플랫폼 검사·시험](../../../tools/ci/test_check_network_platform.py); 751/EE ARM Release APK 권한 확인. 실제 DNS 사례는 Task 14 별도. |
| A2 | JVM 로딩·Context — 완료 | [MainActivity](../../../app/android/app/src/main/kotlin/dev/chatproject/chat_app/MainActivity.kt), [android_jni.rs](../../../app/rust/src/android_jni.rs); JNI_OnLoad는 VM 저장, 명시적 JNI가 applicationContext GlobalRef를 Flutter/Endpoint 전에 설치. 두 ARM export·R8 보존 확인. |
| A3 | Debug 전용 시험 채널 — 완료 | MainActivity의 FLAG_DEBUGGABLE 가드, [가드 변이 시험](../../../tools/ci/test_check_network_platform.py), [Release 표식 검사](../../../tools/ci/check_release_hooks.py). M7 push 진입점 검증은 별도 유지. |
| A4 | 실제 Release smoke CI — 완료 | [ci.yml](../../../.github/workflows/ci.yml), [release_smoke.dart](../../../tools/ci/release_smoke.dart), [계약 시험](../../../app/integration_test/release_contract_test.dart); 일반 Home/native와 전용 Stopped/NotAvailable 실행. 계약은 공용 DNS 증거가 아님. |
| A5 | ARM64/ARMv7 실제 빌드·ARM64 실행 — 완료 | 751/EE 모두 두 ARM 실제 빌드·전체 payload·Rust 1.92/해시 확인. 751 물리 ARM64 한글/양방향 4096 B·복귀·이동·해제·재시작, EE 설치 SHA·Ready·실제 ticket 연결 확인. [결과](../results/2026-10-16-m2-transport.md) 참조. ARMv7 **실제 실행은 승인된 64비트 전용 기기 예외로 배포 전 유예**. hostname/DHT/Relay·성능은 별도 Task 14 게이트이며 이 행의 완료로 닫히지 않음. |
| A6 | sink 계약·poison·Mutex 제거 — 완료 | [event.rs](../../../core/chat_core/src/event.rs), [runtime.rs](../../../core/chat_core/src/runtime.rs), [convert.rs](../../../app/rust/src/convert.rs); nonblocking/no-reentry, bus/slot fail-fast poison 유지, FrbSink 직접 StreamSink 사용 및 Rust 시험. 모든 Mutex/expect 제거를 뜻하지 않음. |
| A7 | 닫힌 Dart stream — 완료 | [network_test.dart](../../../app/integration_test/network_test.dart)의 구독 취소→두 Stop→재구독과 event.rs closed-sink 단위 시험; Windows 실제 network 4 PASS. 내부 subscriber 수/heap 측정을 주장하지 않음. |
| A8 | 모든 오류·이벤트 변환 — 완료 | [convert.rs](../../../app/rust/src/convert.rs) exhaustive match/variant 시험: 오류 12개·이벤트 5개·선택값/u64/Unicode와 네트워크 enum. Rust CI 성공. |
| A9 | 전역 코어 시험 독점 주석 — 완료 | [native.rs](../../../app/rust/src/native.rs)의 단일 process-global 시험 소유 주석과 기존 시험 확장. |
| A10 | 전역 포맷 정책·CI — 유예 | touched handwritten 형식과 생성 파일 반복 안정성은 확인. 전역 rustfmt.toml/Dart 폭 합의·전체 format CI·기존 churn은 미구현 후속. 관련 없는 전체 포맷 변경을 하지 않음. |
| A11 | 폴더 독점 결정 — M4 유예 | [runtime.rs](../../../core/chat_core/src/runtime.rs)는 프로세스 안만 보호. [bootstrap.dart](../../../app/lib/bootstrap.dart)/E2E의 participant별 data-dir는 격리 수단. **M4 DB open 전에 프로세스 간 독점 정책을 결정·구현**해야 함. 파일 잠금 구현을 주장하지 않음. |
| A12 | 런타임 오류·stream 실패 표시 — 완료 | [network_model.dart](../../../app/lib/network/network_model.dart), [network_screen_test.dart](../../../app/test/network_screen_test.dart), [home_screen_test.dart](../../../app/test/home_screen_test.dart); 12오류 안내·stream error/end·실패 글 유지/재전송 없음. iPhone LAN 실제 시험은 별도. |
| A13 | 시작 오류 stack 진단 — 완료 | [bootstrap.dart](../../../app/lib/bootstrap.dart)의 error+stack, [bootstrap_test.dart](../../../app/test/bootstrap_test.dart). 원시 logcat은 비공개 보관. |
| A14 | main catch 주입 시험 — 완료 | [main.dart](../../../app/lib/main.dart)→bootstrap 위임과 [bootstrap_test.dart](../../../app/test/bootstrap_test.dart)의 초기화 실패 UI/구조화 오류 시험. |
| A15 | Flutter `--` 구분자 — 완료 | [devices.dart](../../../tools/e2e/bin/devices.dart), [README](../../../tools/e2e/README.md), [devices_cli_test.dart](../../../tools/e2e/test/devices_cli_test.dart); 실제 subprocess의 옵션 전달·잘못된 옵션 종료 확인. |
| A16 | 첫 부팅 snapshot 저장 금지 — 완료 | [emulators.dart](../../../tools/e2e/lib/src/emulators.dart), [emulators_test.dart](../../../tools/e2e/test/emulators_test.dart); 첫/test 부팅 모두 no-snapshot-load/save. |
| A17 | 외부 exit·실제 snapshot·SDK — 완료 | 같은 emulator 구현/시험과 [android_sdk.dart](../../../tools/e2e/lib/src/android_sdk.dart); list/create/save 오류·missing snapshot 확인. schema3 유한 진단은 live Android 야간 성공과 별도. |
| A18 | emulator 프로세스 출력 — 완료 | [process_runner.dart](../../../tools/e2e/lib/src/process_runner.dart), [실제 process 시험](../../../tools/e2e/test/process_runner_test.dart), [보관 정책](../../../tools/e2e/README.md); 즉시 오류·원시 exit·bounded drain·비공개 출력. fixture ready-file 수정 후 실제 Linux 집중 사례도 성공. |
| A19 | CLI/실제 프로세스 시험 — 완료 | devices_cli_test.dart와 process_runner_test.dart의 실제 OS 자식/가짜 실행 파일/CLI 분기. [369 CI](https://github.com/HyunwookYoo/chatproject/actions/runs/38900998746) tools 124 PASS/5 platform skip; [378 CI](https://github.com/HyunwookYoo/chatproject/actions/runs/38908404372) tools 141 PASS/5 skip; [7a CI](https://github.com/HyunwookYoo/chatproject/actions/runs/38917586077) tools 166 PASS/기존 platform skip 5개·analyze clean. |
| A20 | 전체 cleanup·최초 exit — 완료 | [owned_resources.dart](../../../tools/e2e/lib/src/owned_resources.dart), devices_cli_test.dart와 [run_diagnostic_test.dart](../../../tools/e2e/test/run_diagnostic_test.dart); 실패 exit 유지·성공 뒤 cleanup 실패 승격·첫 진단 순서 보존. |
| A21 | 죽은 console 진단 — 완료 | emulators.dart/emulators_test.dart의 빈 console 진단과 [README](../../../tools/e2e/README.md). 승인된 안전 조정으로 표적 수동 진단을 안내하며 전역 adb kill-server를 자동 실행하지 않음. |
| A22 | 기존 AVD 실패 보존 — 완료 | emulators_test.dart의 save 실패/missing snapshot/boot 실패 보존 및 devices_cli_test.dart 실제 subprocess. 소유하지 않은 AVD를 종료하지 않음. |
| A23 | ensure 소유·초기 PID 경합 — 완료 | emulators.dart의 managed start/소유 등록/owned stop, process_runner.dart; 실행 중 AVD 보존·첫 부팅 timeout 정리·혼합 E2E 확인. 7a4c2b0의 소유 snapshot stop 후 serial/두 포트 정리 barrier는 60 PASS/기존 Windows SIGINT 1 skip·검토 및 후속 native 야간 성공; 과거 호스트 실패와 같은 원인이었는지는 미확정. |
| A24 | SIGINT 정리 — 완료 | devices.dart/실제 CLI signal 시험; [369 Linux CI](https://github.com/HyunwookYoo/chatproject/actions/runs/38900998746) 실제 POSIX signal PASS. Windows 특정 console Ctrl+C 전달은 명시적 platform skip이며 실증했다고 하지 않음. |
| A25 | 시스템 이미지 고정 — 완료 | [avd_config.json](../../../tools/e2e/avd_config.json), emulator image/ABI/CPU 검사와 wrong-image 거절 시험. 로컬 API36 Play Store/CI API36 google_apis 구분. 실제 Android 야간 실패는 면제하지 않음. |
| A26 | 단일 AVD 설정·cache — 완료 | avd_config.json, [ci.yml](../../../.github/workflows/ci.yml), [e2e.yml](../../../.github/workflows/e2e.yml); API/target/arch와 hash cache key를 같은 설정에서 파생. |
| A27 | Windows integration 전체 폴더 — 완료 | ci.yml의 정렬된 각 파일 개별 실행과 [integration_test](../../../app/integration_test); core 3/network 4 PASS 및 ordinary Windows 성공. Flutter log-reader 우회이며 SDK 수정 없음; Android/Release 전용 skip은 별도. |
| A28 | Android2 야간·Gradle 메모리 — 완료 | e2e.yml의 빌드 선행/소유 Gradle 정리/GNU time·RSS, [gradle.properties](../../../app/android/gradle.properties)의 Xmx8G 유지. 378 scenario timeout/820 console exit 1 기록은 보존; 7a Android2 실제 scenario exit 0/성공, max-child RSS 3,323,684 KiB와 별도 owned Gradle 표본 peak 3,082,424 KiB, OOM/137/143/lost 없음. 필요 시 조정 판단 결과 Xmx8G 유지. 수치는 heap/동시 총량 아님·합산 금지·향후 하드웨어 보장 아님. |

## M1b에서 받은 11개

| ID | 요구와 판정 | 유지되는 구현·시험 근거 / 남은 조건 |
|---|---|---|
| B1 | trigger allowlist — 완료 | [check_workflows.py](../../../tools/ci/check_workflows.py), [시험](../../../tools/ci/test_check_workflows.py); 기본 push/PR/dispatch 유지, 파일 stem e2e만 schedule 예외. 일반 schedule 허용이 아님. |
| B2 | job permissions/uses/local action — 완료 | 같은 검사·시험에서 권한 축소만 허용, 외부 reusable SHA 고정, 로컬 ./ 예외. |
| B3 | 기본 정책 unit test — 완료 | 같은 시험의 최상위 read·toolchain 일치·SHA·checkout persist-credentials false 변이 검출. |
| B4 | workflow 파일 탐색 — 완료 | 같은 시험의 yaml/yml·대문자 확장자·파일 없음 사례. |
| B5 | 빈/nonmapping YAML 진단 — 완료 | 같은 검사·시험에서 파일명 포함 단일 오류, traceback 없음. |
| B6 | cargo-expand 고정 — 완료 | [ci.yml](../../../.github/workflows/ci.yml)의 1.0.126 --locked; Windows 실제 생성·Mac/Linux bindings drift 성공. 사용자 전역 설치는 변경하지 않음. |
| B7 | generator 안 pub get — 완료 | [gen_bindings.sh](../../../tools/gen_bindings.sh), [시험](../../../tools/ci/test_gen_bindings.py); app subshell pub get 선행, 실제 Windows 반복·Mac/Linux CI 성공. |
| B8 | generator 보강 — 잔여 f 유예 | gen_bindings.sh/시험/ci.yml의 (a) full diff (b) bash 안내 (c) 정확한 FRB 2.11.1 (d) metadata target-dir (e) 실제 Darwin 성공 완료. (f) upstream FRB tool lock의 yanked futures-util 경고는 미해결. 기존 pin 유지, 승인된 upstream/tool 변경 때 재검토. 제품 의존성 일괄 update 금지. |
| B9 | Simulator runtime·stdout — 완료 | [test_pick_simulator.py](../../../tools/ci/test_pick_simulator.py)의 runtime-only 거절 및 subprocess stdout UDID 한 줄/stderr 진단 시험. |
| B10 | 빈 CARGOKIT_TOOLCHAIN — 완료 | [builder.dart](../../../app/rust_builder/cargokit/build_tool/lib/src/builder.dart), [실제 Dart fixture](../../../tools/ci/test_cargokit_toolchain.py); 빈 override 조기 StateError, pin/default 유지. SDK 없는 skip을 실행 증거로 쓰지 않음. |
| B11 | Windows/iOS 실제 Rust 로그 — 완료 | ci.yml verbose·실제 rustup run 1.92.0 cargo build 검사; 751/EE Windows·ARM 실제 빌드와 hosted Simulator/unsigned Release aarch64 명령 성공. signed M2 TestFlight 로그는 아직 별도 대기. |

## 남은 실행·판단과 유예 책임

- **현재 검증 게이트:** 378 일반 CI 9/9,820 native 8개 작업과 7a flutter의 tools166/app57·analyze 성공은 확보했다. 최신 7a 일반 CI는 8작업 성공/Android 진행 중이며 두 야간은 모두 성공했다. EE 10회는 연결 1회 미달/Direct 전부 목표 충족, 추가 RCA 뒤 판단이라는 사용자 선택을 유지한다. EE fresh-client ID-only는 전경/Awake·빈 cache 조건의 첫 시도와 retry 모두 실제 transportFailed, 반대 fresh-publisher 방향은 추가 계획 사례로 미실행이며 필수 요건을 늘리지 않는다. 원인·승인 계획의 별도 ID-only DHT 연결 확인과 hostname/Relay 검증, signed M2 TestFlight/iPhone 전경·LAN 거절/복구·Rust 로그와 전체 브랜치 최종 검토/통합이 남아 있다. 751 DHT 두 timeout의 전경/절전 한계도 보존하며 Task 14/M2를 닫지 않는다.
- **M4:** A11의 데이터 폴더 독점 정책을 DB open 전에 결정·구현한다.
- **M7:** Release Kotlin/JNA/UniFFI push 진입점 검증과 기존 M1b NSE 견고성·측정용 코드 정리는 유지한다. M2 전경 채팅은 과거 APNs/NSE 측정이나 백그라운드 보장을 대체하지 않는다.
- **배포 전:** ARMv7 호환 기기 실행 검증을 확보한다. 64비트 전용 SM-F971N 결과로 대체하지 않는다. 기존 배포 서명 과제도 유지한다.
- **후속 정책/외부 의존성:** A10 전역 format 합의·기존 churn, B8f upstream yanked 경고, Apple 알림 필터링/multicast entitlement 신청은 유예다. Rust 1.92/FRB 2.11.1/UniFFI 0.32.2/libc 0.2.189 및 기존 의존성 pin을 유지한다.

TestFlight는 기존 `main`/`testflight-*` Environment 허용 ref와 실행별 owner review를 따른다. M2 tag·upload 승인은 없으며 실제 서명된 후보 검토/게시 승인 이후에만 iPhone 증거를 확보한다. [결과 문서](../results/2026-10-16-m2-transport.md)의 실행별 판정을 후속 증거로 갱신하되 과거 실패와 미측정 범위를 지우지 않는다.
