# M1a가 남긴 일

M1a를 만들고 최종 리뷰하는 동안 찾았지만 일부러 뒤로 미룬 일을 마일스톤별로 모은 문서다. 마일스톤에 속하지 않는 일은 마지막의 "배포"와 "지켜볼 것"에 두었다. 각 마일스톤의 상세 계획은 쓰기 전에 이 문서에서 자기 절을 읽어야 한다. 항목마다 할 일, 이유 한 문장, 위치(파일:줄 또는 문서 절)를 적었다. 줄 번호는 2026-10-05의 `m1a-core-skeleton` 브랜치 기준이다.

## M1b

1. **바인딩을 한 번에 다시 만드는 스크립트를 쓴다.** flutter_rust_bridge, UniFFI Kotlin, UniFFI Swift를 모두 다룬다. CI가 이 스크립트를 돌려 커밋된 결과와 다르면 실패하게 한다.
   - 이유: 생성한 바인딩을 커밋하는데 다시 만드는 일은 손으로 치는 명령뿐이라, 빠뜨려도 CI가 잡지 못한다.
   - 위치: [M1a 계획](2026-10-04-m1a-core-skeleton.md) Task 3 Step 6과 Task 5 Step 4. Task 5 Step 4의 `grep`은 UniFFI가 백틱으로 감싼 이름(`` `nativeInstanceId` ``)을 찾지 못한다.
2. **`actions/checkout`, `actions/setup-java`, `actions/cache`를 v5(Node 24)로 올린다.**
   - 이유: v4는 Node 20 기반이라 실행마다 폐기 예고 주석이 붙는다.
   - 위치: `.github/workflows/ci.yml:17,29,54,86`(checkout), `:92`(setup-java), `:110`(cache).
3. **2026-10-19부터 11-19 사이에 `ubuntu-latest`가 26.04로 바뀌는 동안 `android` 작업을 지켜본다.** 빨개지면 `ubuntu-24.04`로 고정한다.
   - 이유: 이 기간에 `android`가 갑자기 빨개지면 원인은 코드가 아니라 러너 이미지일 가능성이 크다(runner-images#14748).
   - 위치: `.github/workflows/ci.yml:14,26,67`(`runs-on`).
4. **26.04로 옮긴 뒤에는 `Free disk space` 단계가 찍는 `df -h /` 두 줄을 먼저 읽는다.**
   - 이유: 이 단계의 경로는 ubuntu-24.04 배치에 맞춘 것이고, 없는 경로의 `rm -rf`는 조용히 성공해서 디스크가 안 비어도 오류가 나지 않는다.
   - 위치: `.github/workflows/ci.yml:70-85`.
5. **`concurrency` 그룹(진행 중인 실행 취소)과 Rust·Gradle 캐시를 더한다.** `timeout-minutes`는 M1a 마무리에서 이미 넣었다.
   - 이유: macOS 러너는 Linux보다 무료 사용 시간을 10배 빨리 깎아서, push마다 전체 작업이 쌓이면 시간이 금방 바닥난다.
   - 위치: `.github/workflows/ci.yml:12-138`, [자동 테스트 설계](../specs/2026-10-05-test-automation-design.md) 8절.
6. **서드파티 액션을 커밋 SHA로 고정하고, 워크플로 맨 위에 `permissions: contents: read`를 둔다.**
   - 이유: App Store Connect와 APNs 비밀이 저장소에 들어가면, 움직이는 참조(`@master`, `@v2`)가 바뀐 코드를 비밀과 함께 돌릴 수 있다.
   - 위치: `.github/workflows/ci.yml:18,55,96`(`dtolnay/rust-toolchain@master`), `:30,58,100`(`subosito/flutter-action@v2`), `:119,129`(`reactivecircus/android-emulator-runner@v2`), `:1-10`(`permissions`를 둘 최상위).
7. **`native.rs`와 `global.rs`의 첫 주석을 고친다.** 두 주석은 iOS 알림 확장이 이 진입점을 쓴다고 말한다.
   - 이유: 로드맵은 알림 확장을 다른 프로세스의 `core/chat_nse` 크레이트로 두므로, "이 프로세스의 코어"라는 말이 M1b 계획을 잘못된 쪽으로 끈다.
   - 위치: `app/rust/src/native.rs:1-2`, `app/rust/src/global.rs:1-2`, [로드맵](2026-10-04-p2p-chat-roadmap.md) §2와 §4 M1b.
8. **사용자가 App ID를 등록하기 전에 iOS 번들 ID를 확정하고 `project.pbxproj`에 고정한다.**
   - 이유: 템플릿이 정한 `dev.chatproject.chatApp`과 Android의 `dev.chatproject.chat_app`이 달라서, 그대로 등록하면 두 플랫폼의 ID가 어긋난다.
   - 위치: `app/ios/Runner.xcodeproj/project.pbxproj:385,564,586`(앱 대상), [로드맵](2026-10-04-p2p-chat-roadmap.md) §4 M1b의 "사용자 준비".

## M2

### Android와 릴리스 빌드

1. **주 매니페스트에 `INTERNET` 권한을 더한다.**
   - 이유: 지금 권한은 `src/debug`와 `src/profile` 매니페스트에만 있어서, 모든 시험이 디버그 빌드인 CI를 통과하고도 릴리스 빌드의 네트워크는 막힌다.
   - 위치: `app/android/app/src/main/AndroidManifest.xml:1-5`, `app/android/app/src/debug/AndroidManifest.xml:6`, `app/android/app/src/profile/AndroidManifest.xml:6`.
2. **`MainActivity`에서 `System.loadLibrary("chat_ffi")`를 명시적으로 부른다.**
   - 이유: `libchat_ffi.so`를 Dart와 JNA의 `dlopen`으로만 열면 JVM 로더를 거치지 않아 `JNI_OnLoad`가 돌지 않고, M2의 `install_android_jni_context`도 불리지 않는다.
   - 위치: `app/android/app/src/main/kotlin/dev/chatproject/chat_app/MainActivity.kt:9-25`, `app/android/app/src/main/kotlin/uniffi/chat_ffi/chat_ffi.kt:671,691`, [로드맵](2026-10-04-p2p-chat-roadmap.md) §4 M2의 범위.
3. **시험 전용 채널 `chat_app/native_core`를 디버그 빌드에서만 등록한다.** `ApplicationInfo.FLAG_DEBUGGABLE`로 거르거나 `src/debug/`로 옮긴다. M2의 릴리스 표식 작업과 함께 한다.
   - 이유: 자동 테스트 설계는 시험 전용 기능을 배포판에 넣지 않는다고 정했는데, 이 채널은 지금 릴리스 빌드에도 등록된다.
   - 위치: `app/android/app/src/main/kotlin/dev/chatproject/chat_app/MainActivity.kt:13-23`, [자동 테스트 설계](../specs/2026-10-05-test-automation-design.md) 3절 5번과 7절.
4. **릴리스 스모크 테스트를 CI에 둔다.** 릴리스 APK를 에뮬레이터에 설치해 실행하고 프로세스가 살아 있는지 본다. M2의 릴리스 표식 검사 작업을 넓혀서 한다.
   - 이유: CI는 릴리스 빌드를 컴파일만 하고 시험은 모두 디버그 빌드로 돌아서, 릴리스에서만 다른 것(R8, 권한)을 아무도 시험하지 않는다.
   - 위치: `.github/workflows/ci.yml:104-108`(Release APK 단계), [자동 테스트 설계](../specs/2026-10-05-test-automation-design.md) 7절 4번과 9절의 M2 줄.
5. **`arm64-v8a`와 `armeabi-v7a` 빌드를 실제로 돌려 본다.** M2의 폰 기기 확인에서 한다.
   - 이유: M1a는 x86_64 에뮬레이터에서만 돌렸고 ARM 빌드는 컴파일만 했다.
   - 위치: `.github/workflows/ci.yml:99`(Rust 타깃), [로드맵](2026-10-04-p2p-chat-roadmap.md) §4 M2의 "기기 확인".

### 코어와 FFI

6. **이벤트 싱크의 규칙을 문서에 적는다.** 싱크는 막히지 않고, 버스와 슬롯을 다시 부르지 않는다. 락이 중독됐을 때의 방침도 정한다. `FrbSink`의 불필요한 `Mutex`와 `expect`는 없앤다.
   - 이유: 싱크는 버스 락 안에서(첫 시작 때는 슬롯 락 안에서도) 불리므로, 느리거나 되부르는 싱크가 코어 전체를 멈춘다.
   - 위치: `core/chat_core/src/event.rs:24-27,47-62`, `core/chat_core/src/runtime.rs:40-60`, `app/rust/src/convert.rs:42-50`.
7. **끊긴 Dart 스트림이 버려지는지 통합 테스트로 확인한다.** M2가 두 번째 발행을 일으킬 수 있게 되면 쓴다.
   - 이유: 닫힌 포트에서 `FrbSink::send`가 `false`를 돌려주고 버스가 그 싱크를 버리는지 아직 어떤 테스트도 고정하지 않는다.
   - 위치: `app/rust/src/convert.rs:45-50`, [M1a 계획](2026-10-04-m1a-core-skeleton.md) Review Focus 3.
8. **모든 `ErrorCode`와 `CoreEvent` 갈래를 도는 표 테스트를 쓴다.** enum이 늘어날 때 한다.
   - 이유: 지금 변환 테스트는 `AlreadyStartedDifferentConfig`와 `ConnectionState` 갈래만 고정하고, `InvalidConfig`와 `CoreEvent::Error` 갈래는 비어 있다.
   - 위치: `app/rust/src/convert.rs:14-21,29-40,52-76`.
9. **`native.rs`의 테스트가 전역 코어를 시작해도 되는 유일한 `chat_ffi` 테스트라고 한 줄 주석을 단다.**
   - 이유: 같은 프로세스에서 다른 테스트가 전역 코어를 먼저 시작하면 `native_instance_id()`가 `None`이라는 단언이 깨진다.
   - 위치: `app/rust/src/native.rs:19-30`.
10. **rustfmt와 Dart 형식 검사를 CI에 넣는다.** 먼저 `rustfmt.toml`과 Dart 줄 너비를 정한다. 지금 코드 스타일은 `use_small_heuristics = "Max"`에 가깝다.
    - 이유: `cargo fmt --check`가 `chat_core`에서 12곳, 손으로 쓴 `chat_ffi` 파일에서 7곳을 지적하므로, M2가 코드를 늘리기 전에 기준을 정해야 한다.
    - 위치: `core/chat_core/src/`, `app/rust/src/convert.rs`, `app/rust/src/api/chat.rs`, `.github/workflows/ci.yml:22-23`.
11. **같은 데이터 폴더를 두 프로세스가 여는 것을 막을지 정한다.** 독점 잠금 파일과 새 `ErrorCode`가 한 방법이다. M2에서 정하지 못하면 M4가 DB를 열기 전에 정한다.
    - 이유: `CoreSlot`은 한 프로세스 안에서만 지켜서, Windows에서 `chat_app.exe`를 두 번 띄우면 같은 폴더를 둘이 연다.
    - 위치: `core/chat_core/src/runtime.rs:27-65`.

### 앱 화면

12. **`ChatEvent_Error`를 화면에 보이고 이벤트 스트림에 `onError`를 단다.**
    - 이유: M2가 첫 런타임 오류를 만들면 지금 코드는 그 이벤트와 스트림 오류를 조용히 버린다.
    - 위치: `app/lib/home/home_screen.dart:35-39`.
13. **시작 오류를 스택과 함께 로그에 남긴다.** logcat 수집이 M2에 생기면 거기로 모은다.
    - 이유: `main()`의 `catch`는 스택을 버리고 아무것도 기록하지 않아서, 화면에 뜬 문자열이 유일한 기록이다.
    - 위치: `app/lib/main.dart:13-17`, [자동 테스트 설계](../specs/2026-10-05-test-automation-design.md) 4.7절.
14. **`main()`의 `catch` 경로를 시험할 수 있게 구조를 바꾸고 테스트한다.** M2가 조종 모드 실행 인자 때문에 `main()`을 고칠 때 함께 한다.
    - 이유: 이 경로는 Windows 스모크 스크린샷으로만 확인했고, 시험하려면 `main()`을 나눠야 하는데 M1a는 그러지 않았다.
    - 위치: `app/lib/main.dart:9-19`, `app/test/startup_error_app_test.dart`.

### 기기 도구 (`tools/e2e`)

15. **`flutter test`에 넘기는 옵션이 `--` 뒤에서만 통과하는 점을 고치거나 사용법에 적는다.**
    - 이유: `ArgParser`가 모르는 옵션을 거절해서 `test` 명령에 `--name` 같은 옵션을 바로 넘길 수 없는데, 사용법 문구가 `--`를 알려 주지 않는다.
    - 위치: `tools/e2e/bin/devices.dart:10-23,26-35,68`.
16. **`firstBootArgs`에 `-no-snapshot-save`를 더한다.**
    - 이유: 없으면 첫 부팅을 끌 때 AVD마다 2.1 GB짜리 기본 스냅숏을 쓰느라 30초 종료 기한을 넘길 수 있다.
    - 위치: `tools/e2e/lib/src/emulators.dart:35-41,162-169`.
17. **외부 프로그램의 종료 코드를 확인한다.** 특히 `snapshot save`와 `clean` 스냅숏이 실제로 생겼는지를 본다. SDK 폴더가 있는지도 확인한다.
    - 이유: `adb`, `-list-avds`, `snapshot save`가 실패해도 지금은 조용히 지나가서 깨진 `clean`이 남을 수 있다.
    - 위치: `tools/e2e/lib/src/emulators.dart:75-78,104-105,117`, `tools/e2e/lib/src/android_sdk.dart:10-18`.
18. **에뮬레이터 출력을 자동 테스트 설계 4.7절의 실패 폴더에 모은다.**
    - 이유: `startDetached`가 출력을 버려서, 잘못된 부팅이 180초 동안 말없이 기다리다 끝난다.
    - 위치: `tools/e2e/lib/src/process_runner.dart:32-35`, `tools/e2e/lib/src/emulators.dart:112,137`, [자동 테스트 설계](../specs/2026-10-05-test-automation-design.md) 4.7절.
19. **`devices.dart`에 명령줄 수준 테스트를 쓴다.** `IoProcessRunner`도 가짜가 아닌 프로세스로 한 번 시험한다.
    - 이유: 지금 테스트는 가짜 러너를 쓰는 `Emulators`만 다루고, 명령 분기와 실제 프로세스 실행은 손으로만 확인했다.
    - 위치: `tools/e2e/bin/devices.dart:25-92`, `tools/e2e/lib/src/process_runner.dart:15-43`, `tools/e2e/test/emulators_test.dart`.
20. **`test` 명령의 `finally`에서 `_stopQuietly`를 쓰고 시험의 종료 코드를 지킨다.**
    - 이유: 지금은 에뮬레이터 하나의 `stop`이 실패하면 나머지를 끄지 못하고 시험 결과 코드까지 가린다.
    - 위치: `tools/e2e/bin/devices.dart:65-77`, `tools/e2e/lib/src/emulators.dart:171-180`.
21. **콘솔이 죽은 에뮬레이터에 더 분명한 메시지를 내고 `adb kill-server`를 권한다.**
    - 이유: adb 목록에 남은 죽은 serial은 AVD 이름이 빈 문자열이 되어 `taken by AVD ""` 오류로 멈추고, `adb kill-server` 전에는 풀리지 않는다.
    - 위치: `tools/e2e/lib/src/emulators.dart:80-97`.
22. **이미 켜져 있던 에뮬레이터가 `up`이나 `ensure`가 실패해도 살아남는지 테스트로 고정한다.**
    - 이유: 실패 정리 코드가 자기가 켜지 않은 에뮬레이터를 끄지 않는다는 약속을 지키는 테스트가 없다.
    - 위치: `tools/e2e/test/emulators_test.dart:88-215`, `tools/e2e/lib/src/emulators.dart:103-148`.
23. **`ensure`가 자기가 켜지 않은 에뮬레이터를 끄지 않게 한다.** adb가 방금 켠 에뮬레이터를 아직 목록에 올리지 않은 경쟁도 처리한다.
    - 이유: 성공 경로가 이미 돌던 에뮬레이터까지 끄고, `stop`은 adb가 모르는 에뮬레이터에서는 바로 돌아와서 켜지는 중인 에뮬레이터를 남긴다.
    - 위치: `tools/e2e/lib/src/emulators.dart:122,162-169`.
24. **`test` 명령이 Ctrl+C를 받으면 같은 정리 루프를 돌리도록 `ProcessSignal.sigint` 핸들러를 단다.**
    - 이유: 지금은 Ctrl+C가 `finally`를 건너뛰어 켠 에뮬레이터가 남고, `down`으로 치울 때까지 메모리를 3 GB쯤 쓴다.
    - 위치: `tools/e2e/bin/devices.dart:60-77`.
25. **AVD를 만들 때 쓰는 시스템 이미지가 설계 5절의 이미지와 같은지 확인한다.**
    - 이유: `flutter emulators --create`는 설치된 가장 새 이미지를 고르므로, 새 이미지를 설치하면 설계 5절의 `android-36;google_apis_playstore`와 다른 이미지로 시험이 돌 수 있다.
    - 위치: `tools/e2e/lib/src/emulators.dart:107`, [자동 테스트 설계](../specs/2026-10-05-test-automation-design.md) 5절.

### CI

26. **AVD 설정(API 36, `google_apis`, x86_64)을 한 곳에 두고 캐시 키를 그 값에서 만든다.**
    - 이유: 같은 값을 세 곳에 손으로 적고 `force-avd-creation: false`가 이미지를 비교하지 않아서, API 수준을 올리며 키를 빠뜨리면 낡은 AVD로 시험이 돈다.
    - 위치: `.github/workflows/ci.yml:116,121-123,131-133`.
27. **Windows 작업이 `integration_test` 폴더 전체를 돌게 한다.**
    - 이유: Windows는 `core_test.dart`를 이름으로 돌려서, M2의 새 통합 테스트는 `ci.yml`을 고쳐야 Windows에서 돈다.
    - 위치: `.github/workflows/ci.yml:63`(Windows), `:138`(Android는 폴더 전체).
28. **에뮬레이터 두 대를 켜는 야간 작업에서 Gradle 힙 `-Xmx8G`를 확인하고, 필요하면 줄인다.**
    - 이유: 비공개 Linux 러너는 에뮬레이터를 켠 채 메모리가 7 GB쯤이라, 힙이 8 GB면 `android`가 종료 코드 137·143이나 "runner lost"로 죽을 수 있다.
    - 위치: `app/android/gradle.properties:1`.

## M3

1. **`ErrorCode`를 한국어 문구로 바꾸는 표를 만들고, 화면에 `invalidConfig` 같은 이름이 그대로 보이지 않게 한다.**
   - 이유: 지금 시작 실패 줄은 한국어 문장 속에 코드 이름(`invalidConfig`)을 그대로 섞어 사용자에게 보여 준다.
   - 위치: `app/lib/home/home_screen.dart:42-45,55-61`.
2. **제품 이름을 확정하고 Android 런처 이름과 Windows 창 제목에 반영한다.**
   - 이유: 런처 이름과 창 제목은 템플릿이 정한 `chat_app`이고 `MaterialApp.title`만 `ChatProject`라서, 이름이 둘로 갈라져 있다.
   - 위치: `app/android/app/src/main/AndroidManifest.xml:3`, `app/windows/runner/main.cpp:30`, `app/lib/main.dart:29`, `app/lib/startup_error_app.dart:16`.
3. **코어를 멈추거나 초기화하는 API가 필요한지 정한다.**
   - 이유: `CoreSlot`에는 시작만 있고 멈춤이 없어서, 복구와 기기 해제 흐름이 새 코어를 만들려면 프로세스를 다시 띄워야 한다.
   - 위치: `core/chat_core/src/runtime.rs:27-65`, `app/rust/src/api/chat.rs:51-61`, [로드맵](2026-10-04-p2p-chat-roadmap.md) §4 M3의 범위.

## M4

1. **Windows에서 `canonicalize`가 돌려주는 `\\?\` 경로를 SQLCipher가 쓰기 전에 문서에 적거나 벗긴다.**
   - 이유: Windows의 정규 경로는 `\\?\C:\…` 꼴이라, 저장소 코드가 이 형태를 그대로 받아도 되는지 확인된 적이 없다.
   - 위치: `core/chat_core/src/runtime.rs:23-24,67-82`.
2. **CI의 `windows` 작업에 `cargo test -p chat_core`를 더한다.** M4가 경로 코드를 건드리기 전에 한다.
   - 이유: 한글 폴더 시험(`non_ascii_folder_works`)이 CI에서는 Linux에서만 돌아서, Windows 경로 코드(`\\?\` 정규형, 와이드 문자 API)는 로컬에서 누가 돌릴 때만 검증된다.
   - 위치: `.github/workflows/ci.yml:47-64`(`windows` 작업), `core/chat_core/src/runtime.rs:177`.
3. **데이터 폴더가 어디에 놓일지 정한다.** 암호화 메시지 DB가 들어오기 전에 한다.
   - 이유: 기본값이 Windows에서는 Roaming AppData(`getApplicationSupportDirectory`), Android에서는 Auto Backup 대상(`files/`, `allowBackup`과 `dataExtractionRules` 없음)이라서, 정하지 않으면 암호화 DB가 그 기본값을 따라간다.
   - 위치: `app/lib/core_client.dart:20-23`, `app/android/app/src/main/AndroidManifest.xml:2-5`.
4. **M2에서 정하지 않았다면, 같은 데이터 폴더를 두 프로세스가 여는 것을 DB를 열기 전에 막는다.** M2 11번과 같은 항목이다.
   - 이유: `CoreSlot`은 한 프로세스 안에서만 지켜서, Windows에서 `chat_app.exe`를 두 번 띄우면 같은 SQLCipher DB를 둘이 연다.
   - 위치: `core/chat_core/src/runtime.rs:27-65`.

## M7

1. **푸시 수신기를 만들기 전에 릴리스 빌드에서 Kotlin→Rust 경로를 한 번 호출해 본다.** M7의 진입 조건이다.
   - 이유: M1a는 JNA와 UniFFI 클래스가 릴리스 dex에 남는 것까지만 정적으로 확인했고, 릴리스에서 Kotlin→Rust 호출이 실제로 도는지는 돌려 보지 않았다.
   - 위치: `app/android/app/proguard-rules.pro:1-5`, `app/android/app/src/main/kotlin/dev/chatproject/chat_app/MainActivity.kt:14-23`.
2. **푸시 수신기와 `MethodChannel`이 함께 쓰는 진입점(또는 `Application` 초기화)을 두고, 거기서 `uniffiEnsureInitialized()`를 먼저 부른다.** 일부러 어긋나게 만든 바인딩으로 이 검사가 실제로 실패하는지도 한 번 확인한다. 지금은 코드를 읽어서만 믿고 있다.
   - 이유: 푸시 수신기는 `MethodChannel` 핸들러를 거치지 않으므로, 지금 구조에서는 무결성 검사 없이 UniFFI를 부르게 된다.
   - 위치: `app/android/app/src/main/kotlin/dev/chatproject/chat_app/MainActivity.kt:14-23`, `app/android/app/src/main/kotlin/uniffi/chat_ffi/chat_ffi.kt:808-835`.
3. **`cfg(unix)` 심볼릭 링크 시험을 쓰고, Kotlin이 코어를 시작하게 되면 기기에서 `/data/data/…`와 `/data/user/0/…` 두 경로로 모두 시작해 본다.**
   - 이유: 같은 폴더를 다른 경로 문자열로 넘기는 시험은 지금 `..` 경로만 다루고, 별칭이 실제로 생기는 Android 경로는 한 번도 돌려 보지 않았다.
   - 위치: `core/chat_core/src/runtime.rs:67-82,138`, [M1a 계획](2026-10-04-m1a-core-skeleton.md) Review Focus 1.

## 배포

1. **릴리스 서명 키를 만들고 `build.gradle.kts`의 디버그 키 서명을 바꾼다.** 키 파일은 저장소에 넣지 않는다.
   - 이유: 템플릿의 TODO대로 릴리스 빌드가 디버그 키로 서명되므로, 지금 APK는 배포할 수 없다.
   - 위치: `app/android/app/build.gradle.kts:28-33`.

## 지켜볼 것

1. **핫 리스타트에서 `init_app`이 같은 프로세스에서 `setup_default_user_utils()`를 두 번째로 부르는 것이 문제를 일으키는지 본다.** 문제가 보이면 한 번만 부르게 막는다.
   - 이유: 지금 시험 하네스는 핫 리스타트를 흉내 낼 수 없는데, flutter_rust_bridge는 핫 리스타트를 지원한다고 문서에 적고 있다.
   - 위치: `app/rust/src/api/chat.rs:11-14`, [M1a 계획](2026-10-04-m1a-core-skeleton.md) Review Focus 2.
2. **Gradle 구성 캐시(configuration cache)를 켜기 전에 cargokit의 `build()`가 `project`를 읽는 곳을 입력 속성으로 바꾼다.** 켜지 않는 동안은 손대지 않는다.
   - 이유: 작업이 실행되는 중에 `project`를 읽는 코드는 Gradle 구성 캐시와 충돌하고, 이 코드는 M1a의 최소 패치 밖에 있는 업스트림 코드다.
   - 위치: `app/rust_builder/cargokit/gradle/plugin.gradle:53-66`.
