# M3 C — App Integration and Validation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** M3의 한국어 화면과 QR 입력을 연결하고 실제 플랫폼·자동화·Release 증거를 완성한다.

**Architecture:** 기존 단일 event subscription 위에 subscribe-before-snapshot AccountModel을 둔다. FRB는 얇은 typed 변환이며 일시 비밀 응답을 일반 event에 넣지 않는다. 시험은 Rust/fake widget/OS native/로컬 다중참가자/실기기를 구분한다.

**Tech Stack:** Flutter 3.44.6, FRB 2.11.1/Freezed, UniFFI 0.32.2, `qr_flutter 4.1.0`, `mobile_scanner 7.1.3`, 기존 `tools/e2e`와 chat_node. 두 신규 Flutter package는 C3에서 정확 pin/lock한다.

**Spec:** [M3 승인 설계 §9·10](../specs/2026-10-18-m3-account-devices-design.md), [전체 계획](2026-10-18-m3-account-devices.md), [자동화 설계](../specs/2026-10-05-test-automation-design.md).

## Global Constraints

전체 계획과 A/B의 타입·권한을 적용한다. 화면 ID **1–3, 6–8, 22–28**, 임시 이름 **ChatProject**, bundle/application ID 불변, 한국어 오류/키보드/스크린리더/**200%** 글자. 일반snapshot/event/진단에 문구·QR secret·private seed를 넣지 않는다. Release에는 secret 조회/가짜 승인/결정적 RNG test API를 넣지 않는다. NSE에 account 기능을 추가하지 않는다.

## Review Focus

- 화면 닫은 뒤 응답·event 재도착/순서 역전: 폐기한 flow를 재표시/승인하지 않음 — C1/C2.
- OS key 잠김/권한 거부: 재시도/복구 경로를 보여주고 성공/새계정으로 대체하지 않음 — C2/C3.
- 한글64B/65B·bidi·큰 글자/키보드만 조작: 안전한 표시와 명시적 버튼 도달 가능 — C2/C3.
- 고밀도QR/연속 camera detect/카메라 없음: 전체 payload를 한 번 검증하고 paste/반대방향 사용 — C3.
- controller 원문 기록·Release hook·재시작 assertion: 비밀을 보존하지 않고 계정/진단 시나리오를 분리 — C4.

### Task 1: C1 — 얇은FRB·계정view·순서가 보장된UI 모델

**Files:** Create `app/rust/src/api/account.rs`, `app/lib/account/{account_client,account_model}.dart`, `app/test/{account_model_test,account_bridge_test}.dart`, `app/integration_test/account_storage_test.dart`; Modify `app/rust/src/{api/mod.rs,convert.rs}`, `app/rust/src/api/chat.rs`, `core/chat_core/src/{event,error}.rs`, `app/lib/{core_client,bootstrap}.dart`, `app/test/{network_model_test,bootstrap_test}.dart`, `app/lib/network/network_model.dart`; 기존 script로만 `app/lib/src/rust/**`, `app/rust/src/frb_generated.rs`, `app/android/app/src/main/kotlin/uniffi/chat_ffi/chat_ffi.kt`, `app/ios/NotificationService/Generated/**`를 재생성한다.

**Interfaces:** A/B의 AccountHandle 메서드와 같은 이름의 snake_case FRB wrapper를 `api/account.rs`에 정의한다. DTO는 A5 AccountView/B4 ContactView/B5 PairView를 비밀 없이 변환한다. `ChatEvent::AccountChanged { revision:u64,view:AccountView }`를 추가하며 revision은 AccountHandle 안에서 단조 증가한다. `AccountClient`는 `Future<AccountView> snapshot()`과 A/B 명령의 Future를 제공하고 기존 CoreClient의 `events()`를 공유한다. `AccountModel.open()/close()/applySnapshot(AccountView)`는 subscribe→snapshot→revision 순서로 적용한다. snapshot보다 새 event를 보관했다가 적용하고 이전 revision/close 뒤 future를 버린다.

- [ ] `subscribe_before_snapshot_keeps_newer_event`, `duplicate_revision_no_rollback`, `closed_flow_ignores_late_secret_reply`, `secret_replies_not_in_events`를 추가한다. `app/`에서 `flutter test test/account_model_test.dart test/account_bridge_test.dart` → FAIL 확인.
- [ ] FRB 변환/AccountModel을 구현한다. A1에서 추가한 계정 오류 코드를 FRB에 같은 이름으로 변환하고 기존 ErrorCode와 모든 exhaustive switch를 갱신한다. detail에 비밀을 넣지 않고 UI는 code만 사용한다. snapshot/event는 공개 view, 비밀 표시는 해당 명령의 응답만 사용한다.
- [ ] Android JNI 초기화→Rust 초기화→core start→native KeyProtector 등록/open account→snapshot→UI 순서를 보장한다. OS Locked/Missing은 StartupErrorApp으로 끝내지 않고 AccountView Locked와 복구 안내를 표시한다. 미지원 OS의 fake KeyProtector를 production 기본값으로 쓰지 않는다.
- [ ] unit PASS 뒤 통합 담당 한 명이 `bash tools/gen_bindings.sh` 실행, `cargo test --locked -p chat_ffi --features test-hooks`, `app/`에서 `flutter analyze`/`flutter test` PASS 확인. 두 번째 재생성에서 생성 차이가 없음을 확인한다. `flutter test integration_test/account_storage_test.dart -d windows`에서 실제 DPAPI 재시작 보존/OS오류 표시를 검증하고 emulator/Simulator OS 보호 검증도 별도로 기록한다. NSE에 새 기능/키 참조가 없는지 확인하고 `feat(app): expose typed account commands and state` commit.

### Task 2: C2 — 생성·복구·초대·연락처 화면

**Files:** Create `app/lib/account/screens/{welcome_screen,recovery_words_screen,recovery_confirm_screen,recover_screen,invite_screen,contact_preview_screen,safety_screen}.dart`, `app/lib/account/{account_routes,error_messages,sensitive_screen}.dart`, `app/test/{account_onboarding_test,contact_safety_test,account_errors_test}.dart`; Modify `app/lib/{main,home/home_screen}.dart`.

**Interfaces:** `AccountRoutes`는 AccountView/AccountModel로 화면을 분기하고 flow 응답을 route의 짧은 수명 상태로 유지한다. `SensitiveScreen(child:Widget)`은 A3의 native `setSensitiveScreen(bool)`을 참조 횟수로 호출하며 이탈/dispose에서 표시 문구와 입력 controller를 폐기한다. background에서는 native 가림을 먼저 적용하고 복귀 시 flow 유효성을 재확인한다. 문구 확인 전 뒤로 나가기를 선택하면 Rust cancel_create까지 호출한다. `String accountErrorMessage(ErrorCode code)`는 한국어 고정표와 fallback을 제공한다.

- [ ] widget에서 신규→24단어→3위치 확인→PublishPending, 단어 오류/확인 취소/저장 실패, 같은 PeerId 복구 시 연락처 복원 없음, active 덮어쓰기 거부를 시험한다. `64/65 UTF8 name`, `duplicate_contact_keeps_alias`, `verification_requires_explicit_digest_confirm`, 200% 글자/keyboard focus/semantics 순서/bidi 격리와 문구가 기본 semantics로 읽히지 않는 시험을 추가한다. `flutter test test/account_onboarding_test.dart test/contact_safety_test.dart test/account_errors_test.dart` → FAIL 확인.
- [ ] 승인된 화면 ID1–3/6–8을 기존 tokens/theme으로 구현한다. 비공개 mockup을 새로 설계하지 않는다. 문구 자동 복사는 없고 명시적인 읽어주기 동작만 허용한다. 복구 전에 “계정 ID는 돌아오지만 내 연락처와 대화 기록은 돌아오지 않아요”, 침해된 기기에 대한 같은 PeerId 복구의 한계를 표시한다. 초대 파싱→이름 수정→명시적 추가, 안전번호64hex 전체와 독립 대조 확인을 제공한다.
- [ ] 오류표에 설계의 “복구 문구의 단어와 순서를 확인해 주세요”, “서명을 확인할 수 없는 초대예요”, “초대가 만료됐어요. 새 초대를 받아 주세요”, “연결할 수 있는 기기는 최대 5대예요”, “확인 코드가 달라요. 새 QR로 다시 연결해 주세요”, “다른 기기에서 목록이 바뀌었어요. 현재 목록을 확인해 주세요”를 그대로 넣는다. OS raw detail/enum/key를 사용자 문구에 넣지 않는다.
- [ ] 위 widget 및 전체 `flutter test`/`flutter analyze` PASS. 모바일은 화면 stack, Windows는 너비에 따라 목록+상세를 사용하고 M2 진단 진입점을 유지한다. `feat(app): add account onboarding and contact verification` commit.

### Task 3: C3 — QR·기기 관리·privacy/native UX

**Files:** Create `app/lib/account/screens/{pair_screen,devices_screen,device_warning_screen}.dart`, `app/lib/account/qr_input.dart`, `app/test/{pair_devices_test,qr_input_test}.dart`, `app/integration_test/account_privacy_test.dart`; Modify `app/pubspec.yaml`, `app/pubspec.lock`, `app/android/app/src/main/AndroidManifest.xml`, `app/ios/Runner/Info.plist`, `app/windows/runner/{flutter_window.cpp,flutter_window.h,Runner.rc,main.cpp,CMakeLists.txt}`, `app/android/app/src/main/res/values/strings.xml`(없으면 생성), `app/lib/main.dart`, A3 native privacy 등록.

**Interfaces:** `QrInput`은 `ValueChanged<String> onPayload`를 감지 flow마다 한 번 호출해 Rust parser에 전달한다. 카메라는 Android/iOS만 사용하며 Windows는 반대 방향 QR과 붙여넣기를 제공한다. `PairScreen`은 B5/B6의 PairView+PairDisplay로 승인/receipt/게시 상태를 표시하며 E 승인과 N 코드 확인을 다른 동작으로 둔다.

- [ ] 양방향 같은 SAS, E 승인 전 완료 표시 없음, 전달 전후 취소 경고 구분, QR 만료/중복 감지/권한 거부, 카메라 없는 붙여넣기, 5대 상한, 현재/마지막 기기 해제 거부, pending 폐기 실패를 widget으로 시험한다. `flutter test test/pair_devices_test.dart test/qr_input_test.dart` → FAIL 확인.
- [ ] `qr_flutter 4.1.0`(Dart>=2.19.6<4/Flutter>=3.7)과 `mobile_scanner 7.1.3`(Dart>=3.7<4/Flutter>=3.29, iOS pod 최소12.0)을 정확 pin한다. 근거는 [qr_flutter metadata](https://pub.dev/api/packages/qr_flutter/versions/4.1.0), [mobile_scanner metadata/공식 archive](https://pub.dev/api/packages/mobile_scanner/versions/7.1.3)다. 실제 Android Gradle/Flutter3.44 target build는 이 task에서 검증하고 실패 시 제품 기능/최소 OS를 무단 변경하지 않는다. QR version은 payload 길이에 따라 선택하며 overflow를 자르지 않는다. QR/텍스트는 같은 원문을 사용하고 화면 밖에 secret을 저장하지 않는다.
- [ ] CAMERA/NSCameraUsageDescription을 추가하고 거부 시 붙여넣기/반대 방향을 제시한다. Android manifest 또는 실제 참조 string, Windows window/title/product resource, iOS CFBundleDisplayName, Flutter title을 ChatProject로 통일하고 application/bundle ID는 유지한다. 기존 파일을 확인해 없는 strings 참조를 만들지 않는다.
- [ ] Android FLAG_SECURE, iOS scene preview overlay, Windows `setSensitiveScreen` channel과 [DWM 미리보기](https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/nf-dwmapi-dwmseticonicthumbnail) 가림을 구현한다. Windows는 DWMWA_FORCE_ICONIC_REPRESENTATION/DWMWA_HAS_ICONIC_BITMAP + WM_DWMSENDICONICTHUMBNAIL/WM_DWMSENDICONICLIVEPREVIEWBITMAP에 단색 bitmap을 반환하고 민감 화면 종료 시 복원한다. API 실패 시 활성화를 성공으로 표시하지 않는다. iOS에서 사용자의 직접 screenshot까지 막는다고 설명하지 않는다.
- [ ] unit PASS, `app/`에서 `flutter build apk --release`/`flutter build windows --release` 및 각 OS integration 실행. macOS의 iOS Simulator build와 각 OS 앱 전환 미리보기 실확인은 별도 증거다. key 실패/lock UI도 확인하고 `feat(app): add device pairing QR and privacy controls` commit.

### Task 4: C4 — 자동 시나리오·Release 경계·실기기 증거

**Files:** Modify `core/chat_core/src/puppet/{protocol,mod}.rs`, `tools/chat_node/src/main.rs`, `tools/e2e/lib/src/{controller,protocol,scenario}.dart`, `tools/e2e/bin/run.dart`, `tools/e2e/test/{controller_test,scenario_test,run_diagnostic_test}.dart`, `tools/ci/{release_smoke.dart,test_release_smoke.py}`, `.github/workflows/ci.yml`; Create `tools/e2e/lib/src/account_scenario.dart`, `tools/e2e/test/account_scenario_test.dart`, `app/integration_test/account_test.dart`, `docs/superpowers/results/2026-10-18-m3-account-devices.md`.

**Interfaces:** runner에 `--scenario m2|m3`를 추가한다(기본 m2). `runAccountScenario`는 기존 Controller/participant API를 사용하며 QR 표시 응답만 메모리에서 상대에게 전달한다. fake RecordBackend/Clock/로컬 relay는 기존 test-hooks + debug_assertions 이중 gate 안에서만 주입한다. 실제 키/승인 검증은 production과 같은 코드를 호출한다.

- [ ] `controller_redacts_mnemonic_qr_psk_even_on_error_timeout`, `m3_restart_same_endpoint_m2_restart_different`, `pending_import_never_normal_sync`, `release_rejects_account_test_commands`와 runner 소유 자원 cleanup 시험을 추가한다. `tools/e2e/`에서 `dart test test/account_scenario_test.dart test/controller_test.dart`, root에서 `python -m unittest discover -s tools/ci -p 'test_*.py' -v` → 신규 기대 FAIL 확인.
- [ ] hook 명령은 create/confirm/invite/contact/pair/revoke/recover의 동일 AccountHandle을 호출하며 로컬 UI 조작을 debug harness로 명시한다. 임의 private seed getter/production 가짜 승인 API를 추가하지 않는다. Controller._record는 원문을 파일에 전달하기 전에 command/응답 kind별 비밀 payload 전체를 redact한다. 예외/timeout/structured diagnostics에도 적용하며 로그 시험에는 공개 fixture만 사용한다.
- [ ] Android emulator 두 대 또는 node와 시나리오④를 추가한다: 새 계정 두 개→invite/contact/safety→양방향 pair→candidate/active restart→다른 기기 해제→같은 PeerId 복구. app restart callback은 소유한 participant에만 추가한다. M2의 익명 restart assertion을 변경하거나 fallback 성공 skip으로 통과시키지 않는다.
- [ ] 로컬 unit PASS 뒤 `dart run bin/run.dart --profile android --scenario m3 --repo-root <checkout>`를 `tools/e2e/`에서 실행한다. CI 기존 binding/각OS/native/Release jobs에 account tests를 추가하되 공개 DHT 성공을 조건으로 삼지 않는다. `cargo test --workspace`, feature별 core/ffi/node tests, clippy, Flutter analyze/test, tools/e2e dart analyze/test, Python CI tests, binding clean diff를 확인한다.
- [ ] 일반 Release smoke의 미설정 첫 화면 기대를 갱신하고 Windows/APK를 `python tools/ci/check_release_hooks.py <artifact>`로 검사한다. macOS는 기존 `check_ios_app.sh`/NSE 회귀를 유지한다. 가짜 DHT/loopback과 실제 공개 DHT 왕복을 구분한다.
- [ ] 실기기 시험의 구체적 산출물/순서를 준비해 사용자에게 제시한다. Windows↔Android 양방향 QR, offline/publishpending, 해제/복구, OS 키 보호/backup 제외, signed iPhone Keychain/first-unlock/저장/QR/같은 PeerId를⑤로 시험한다. 기존 M2 signed build를 M3 검증으로 세지 않는다. public pkarr도 대상/시작 조건을 제시한 뒤 실행하고 기기 미연결/Apple 서명 불가면 미실시로 남긴다.
- [ ] 결과 문서에 task/commit별 실측과 잔여 항목을 기록한다. whole-branch review의 중대 사항 수정 뒤 해당 검증만 재실행하고 `test(account): validate cross-platform account lifecycle` commit. M3 전체 조건이 충족되지 않으면 완료로 표시하지 않고 구체적인 잔여 항목을 보고한다. push/merge/upload는 이 commit에서 실행하지 않는다.

검증값 예: C1 `expect(model.revision, 9)` 뒤 revision8 event에도 9 유지; C2 문구 기본 semantics 포함 개수 `0`, 확대 `2.0`에서 overflow `0`; C3 승인 전 완료 문구 개수 `0`; C4 기록 문자열에 공개 fixture의 mnemonic/QR 원문 포함 여부 `false`. 각 값은 해당 task의 위 테스트에 직접 assertion으로 넣는다.
