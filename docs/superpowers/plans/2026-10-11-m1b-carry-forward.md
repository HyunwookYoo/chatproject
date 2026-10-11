# M1b가 남긴 일

M1b를 만들고 최종 리뷰하는 동안 찾았지만 일부러 뒤로 미룬 일을 마일스톤별로 모은 문서다. 업스트림 수정이나 러너 이미지 전환처럼 남이 움직여야 풀리는 일은 마지막의 "지켜볼 것"에 두었다. 각 마일스톤의 상세 계획은 쓰기 전에 이 문서에서 자기 절을 읽어야 한다. 항목마다 할 일, 이유 한 문장, 위치(파일:줄 또는 문서 절)를 적었다. 줄 번호는 2026-10-11의 `m1b-ios` 브랜치 기준이다. M1a가 남긴 일은 [`2026-10-05-m1a-carry-forward.md`](2026-10-05-m1a-carry-forward.md)에 있고, 그 문서의 M1b 절은 M1b에서 끝났다.

**`cargo update`를 돌리기 전에 "지켜볼 것"의 첫 항목(`libc` 고정)을 읽는다.** 돌리면 iOS 빌드가 깨진다.

## M2

### 워크플로 검사기 (`tools/ci/check_workflows.py`)

1. **트리거를 허용 목록으로 검사한다.** `push`, `pull_request`, `workflow_dispatch`만 허용하고 나머지는 모두 막는다.
   - 이유: 검사기는 `pull_request_target`과 `workflow_run`만 막는 금지 목록인데 계획의 전역 제약은 허용 목록이라, `issue_comment` 같은 다른 트리거는 통과한다.
   - 위치: `tools/ci/check_workflows.py:35-36`, [M1b 계획](2026-10-05-m1b-ios-pipeline.md) Global Constraints의 공개 저장소 규칙.
2. **작업 수준 `permissions`와 작업 수준 재사용 워크플로 `uses:`를 검사하고, 로컬 액션(`./…`)은 SHA 고정 검사에서 뺀다.**
   - 이유: 지금은 최상위 `permissions`와 단계의 `uses:`만 보므로 작업이 권한을 올려도 통과하고, 로컬 액션은 고정되지 않았다고 잘못 걸린다.
   - 위치: `tools/ci/check_workflows.py:37-49`.
3. **기본 규칙에도 단위 테스트를 쓴다.** 최상위 `permissions`, `CARGOKIT_TOOLCHAIN`과 `RUST_TOOLCHAIN`의 일치, SHA 고정, `persist-credentials: false`.
   - 이유: 테스트는 `testflight.yml`과 `ci.yml` 전용 규칙만 고정해서, 공개 저장소의 기본 규칙은 하나를 지워도 초록으로 남는다.
   - 위치: `tools/ci/test_check_workflows.py`, `tools/ci/check_workflows.py:35-49`.
4. **`main()`의 파일 찾기를 테스트한다.** `.yaml` 확장자, 워크플로가 하나도 없는 경우, 대문자 확장자.
   - 이유: 확장자 검사와 "파일 없음" 오류는 손으로만 시험했고, 이 검사가 사라져도 테스트가 모른다.
   - 위치: `tools/ci/check_workflows.py:108-124`.
5. **빈 파일이나 매핑이 아닌 워크플로 파일에서 `AttributeError`로 죽지 않고 문제 한 줄을 낸다.** 확장자 비교는 대소문자를 가리지 않게 한다.
   - 이유: 종료 코드는 지금도 1이지만 스택만 나와서 어느 파일이 문제인지 알 수 없다.
   - 위치: `tools/ci/check_workflows.py:108-124`.

### 바인딩과 CI

6. **bindings 작업에서 `cargo-expand`를 고정한다.** `cargo install cargo-expand --version 1.0.126 --locked`를 더한다.
   - 이유: flutter_rust_bridge 코드 생성이 고정되지 않은 `cargo-expand`를 알아서 설치해서(약 1.6분), 새 버전이 나오면 생성 결과나 시간이 달라진다.
   - 위치: `.github/workflows/ci.yml:73-76`, `tools/gen_bindings.sh:22`.
7. **`flutter pub get`을 `tools/gen_bindings.sh` 안으로 옮기거나 먼저 필요하다고 적는다.**
   - 이유: CI가 스크립트 밖에서 `pub get`을 돌려서 드리프트 검사가 스크립트로 되살릴 수 없는 `pub get` 산출물까지 덮고, 스크립트는 `pub get` 없이는 도는데 그 말이 없다.
   - 위치: `.github/workflows/ci.yml:74-77`, `tools/gen_bindings.sh:22`.
8. **`tools/gen_bindings.sh`의 약한 곳을 고친다.** (a) 드리프트가 나면 `--stat`만 찍는다. 달라진 줄도 찍는다. (b) 안내 문구는 `tools/gen_bindings.sh`를 직접 실행하라고 하는데 파일 모드는 100644다. `bash tools/gen_bindings.sh`로 쓴다. (c) 버전 검사 `*2.11.1*`은 2.11.10도 통과시킨다. (d) `target/debug`가 박혀 있어서 `CARGO_TARGET_DIR`을 쓰면 깨진다. (e) Darwin 갈래는 한 번도 돌지 않았다. (f) flutter_rust_bridge 자신의 잠금 파일이 철회된(yanked) `futures-util`을 끌어와서 경고가 난다.
   - 이유: 실패했을 때 로그만으로 원인을 찾기 어렵고, 도구 버전이 몰래 올라가도 통과한다.
   - 위치: `tools/gen_bindings.sh:10-20`, `.github/workflows/ci.yml:78-87`.
9. **`tools/ci/pick_simulator.py` 테스트의 빈틈을 메운다.** watchOS 픽스처는 이름만으로도 걸러지므로 런타임으로만 걸러지는 경우를 따로 시험하고, 표준 출력에는 UDID만 나온다는 약속(워크플로가 `$(…)`로 받는다)을 테스트로 고정한다.
   - 이유: 지금 테스트가 이 두 가지를 구별하지 못해서 둘 중 하나가 깨져도 초록이다.
   - 위치: `tools/ci/test_pick_simulator.py:26-33`.
10. **`CARGOKIT_TOOLCHAIN`이 빈 문자열이면 무시하거나 오류를 낸다.**
    - 이유: vendored cargokit이 빈 값도 툴체인 이름으로 써서 `rustup run ""`을 부르는데, 소리 내어 실패하긴 해도 원인이 멀다.
    - 위치: `app/rust_builder/cargokit/build_tool/lib/src/builder.dart:138-143`.
11. **iOS와 Windows 빌드에서도 cargokit이 `CARGOKIT_TOOLCHAIN`으로 빌드했는지 로그로 확인한다.**
    - 이유: `android` 작업만 `CARGOKIT_VERBOSE` 로그를 `grep`으로 지켜서, iOS와 Windows는 러너의 stable로 몰래 빌드돼도 모른다.
    - 위치: `.github/workflows/ci.yml:173-183`(android만), `ios`·`ios-release`·`windows` 작업.

## M5와 M7

`chat_nse`는 M5에서 `chat_core`의 `nse` 기능을 감싸는 얇은 크레이트로 바뀌고, M7에서 진짜 알림 확장이 된다. 둘 중 먼저 오는 쪽에서 한다.

1. **`state.rs`의 길이 검사를 파일에 남은 크기로 묶고 `with_capacity`에 상한을 둔다.**
   - 이유: 필드 하나가 64 MiB를, 항목 수가 2^20을 주장할 수 있고 `HashMap::with_capacity(n)`은 그 수만큼 가상 메모리를 예약해서(약 96 MiB), 15 MB 프로세스에서는 깨진 상태 파일 하나가 확장을 죽일 수 있다.
   - 위치: `core/chat_nse/src/state.rs:10-13,35-56`, [M1b 계획](2026-10-05-m1b-ios-pipeline.md) Review Focus 2.
2. **`chat_nse`가 `panic = "unwind"`를 유지하게 한다.** 워크스페이스 `[profile]`에 `panic = "abort"`를 넣지 않는다.
   - 이유: UniFFI가 `catch_unwind`로 패닉을 Swift 오류로 바꾸는데, `abort`면 알림 확장이 오류 없이 그냥 죽는다.
   - 위치: 루트 `Cargo.toml`(지금은 `[profile]`이 없다), `core/chat_nse/src/lib.rs`.
3. **NSE 견고성 시험을 더한다.** `NotApplication` 갈래, 다른 epoch의 암호문, 값이 깨진 상태 파일, `probe_decrypt`를 거치는 거대한 길이. 커밋된 고정 상태 파일의 바이트를 하나씩 뒤집는 퍼즈 시험이 한 방법이다.
   - 이유: 지금 시험은 상태 파일 읽기 오류 몇 가지와 손상된 암호문만 고정하고, 복호 쪽의 나머지 오류 갈래는 한 번도 일으켜 본 적이 없다.
   - 위치: `core/chat_nse/src/lib.rs:84-113`, `tools/nse_fixture/tests/decrypt.rs`.
4. **`state.rs`의 헬퍼 `fn u32`의 이름을 바꾼다.**
   - 이유: `usize`를 돌려주는데 원시 타입 이름 `u32`를 가려서, 읽는 사람이 `u32::from_be_bytes`와 헷갈린다.
   - 위치: `core/chat_nse/src/state.rs:58-62`.
5. **`encoding_is_deterministic`이 정렬 없이도 통과하지 못하게 한다.** 항목이 많은 맵으로 시험하거나 해시 순서가 다른 두 맵을 비교한다.
   - 이유: 지금은 항목 두 개짜리 맵 둘을 비교해서 정렬을 지워도 절반쯤은 통과한다.
   - 위치: `core/chat_nse/src/state.rs:92-95`.
6. **UniFFI 이름공간이 처음 바뀔 때 `tools/gen_bindings.sh`가 생성 폴더를 먼저 지우게 한다.**
   - 이유: 스크립트가 옛 산출물을 지우지 않아서, 이름공간을 지우거나 바꿔도 커밋에 남은 낡은 파일을 CI가 알아채지 못한다.
   - 위치: `tools/gen_bindings.sh:22-27`.

## M7

1. **`Memory.sample()`의 `MemoryLayout.offset(of:)!` 강제 풀기를 `guard let`으로 바꾼다.**
   - 이유: `nil`이면 시작 기록의 딕셔너리 안에서 트랩이 나서, 확장이 시작 기록도 남기지 못하고 죽는다.
   - 위치: `app/ios/NotificationService/NotificationService.swift:130-131`.
2. **측정 화면이 남아 있다면 `initState`가 `_refresh()`를 기다리게 하고, 로그를 엄격한 UTF-8로만 읽지 않는다.**
   - 이유: 입출력 오류나 디코딩 오류가 처리되지 않아서, 확장이 쓰다 죽어 깨진 바이트가 남으면 화면이 "아직 기록이 없어요"로 조용히 비어 보인다.
   - 위치: `app/lib/probe/probe_screen.dart:46-49`, `app/lib/probe/probe_host.dart:44-48`.
3. **`FakeProbeHost`를 `probe_screen_test.dart`에서 공용 테스트 도우미로 옮긴다.**
   - 이유: `home_screen_test.dart`가 다른 테스트 파일을 import해서 가짜 객체를 얻으므로, 한쪽 파일 이름이나 내용이 바뀌면 다른 쪽이 깨진다.
   - 위치: `app/test/home_screen_test.dart:12`, `app/test/probe_screen_test.dart:10-49`.
4. **측정 기록이 (pid, seq)로 짝지어져 합쳐지는 경우를 없앤다.** 알림마다 다른 값(요청 식별자 등)을 기록 줄에 더한다.
   - 이유: 같은 NSE 프로세스가 같은 `seq`의 푸시를 두 번 받으면 두 시작 줄이 한 기록으로 합쳐지고 앞의 끝 줄이 덮어쓰인다. 평소에는 `apns-probe`의 `--start-seq`가 번호를 이어 가서 겹치지 않는다.
   - 위치: `app/lib/probe/nse_record.dart:41-64`.
5. **검사기에 비밀값 규칙을 더하고, TestFlight 규칙을 파일 이름이 아니라 `environment: testflight`에 건다.** (a) 비밀값이 워크플로 수준 `env`, 작업 수준 `env`, `Build IPA` 단계에 들어가면 막는다. (b) `environment: testflight`를 쓰는 모든 작업에 TestFlight 규칙을 적용한다.
   - 이유: 비밀값을 둘 곳은 지금 `testflight.yml` 머리의 주석으로만 지켜지고(그 규칙은 한 번도 코드로 있었던 적이 없다), TestFlight 규칙은 파일 이름 `testflight`에만 걸려서 같은 Environment를 다른 파일에서 쓰면 검사 밖이다.
   - 위치: `tools/ci/check_workflows.py:50-53,71`, `.github/workflows/testflight.yml:18-20`.
6. **`testflight.yml`의 셸을 커밋된 테스트로 시험한다.** 이른 비밀값 확인, 프로파일 설치(팀 비교, 번들 ID, App Store 프로파일 판정, 그룹), `find` 보호, 신원 개수 확인. 스크립트로 떼어 `tools/ci`에서 가짜 `security`와 `PlistBuddy`로 돌리는 것이 한 방법이다.
   - 이유: CI는 `testflight.yml`을 태그 없이는 돌리지 않아서 이 셸은 손으로 만든 가짜로 시험했을 뿐이고, 첫 실행에서 진짜로 처음 돈다. `tools/ci/check_ios_app.sh`도 같은 이유로 서명된 IPA 갈래(`--signed`)는 가짜로만 시험했다.
   - 위치: `.github/workflows/testflight.yml:46-70,113-158`, `tools/ci/check_ios_app.sh:85-110`.
7. **(선택) 프로파일이 `.p12`의 인증서로 만든 것인지 확인한다.** 프로파일의 `DeveloperCertificates`와 키체인 신원의 SHA-1을 비교한다.
   - 이유: 다른 Apple Distribution 인증서로 만든 프로파일은 `Build IPA` 안에서 `::error::` 없이 서명 오류로 끝난다.
   - 위치: `.github/workflows/testflight.yml:105-111`(`Distribution identity check`), `:113-158`.
8. **측정용 코드를 지운다.** M7이 진짜 알림 확장 경로로 바꿀 때 알림 측정 화면, `chat_app/ios_probe` 채널, NSE의 `nse_memory.jsonl` 기록과 제목의 최대 메모리 표시, App Group 파일 읽기를 함께 정리한다.
   - 이유: 자동 테스트 설계는 시험 전용 기능을 배포판에 넣지 않는다고 정했고, M1b 계획은 이것들을 TestFlight 내부 테스트용으로만 허락했다.
   - 위치: `app/lib/probe/`, `app/ios/Runner/AppDelegate.swift`, `app/ios/NotificationService/NotificationService.swift`, [M1b 계획](2026-10-05-m1b-ios-pipeline.md)의 "로드맵·설계와 달라지는 점", [자동 테스트 설계](../specs/2026-10-05-test-automation-design.md) 7절.

## 배포

1. **수출 규정(암호) 질문의 답을 정하고 `Runner/Info.plist`에 `ITSAppUsesNonExemptEncryption`을 넣는다.** 답은 사용자가 Apple의 안내를 보고 정한다.
   - 이유: 키가 없어서 빌드를 올릴 때마다 App Store Connect가 "Missing Compliance"를 묻고, 답하기 전에는 내부 테스터도 그 빌드를 설치하지 못한다. 이 앱은 표준 암호(MLS: X25519, AES-128-GCM, Ed25519)를 쓴다.
   - 위치: `app/ios/Runner/Info.plist`, [M1b 계획](2026-10-05-m1b-ios-pipeline.md) Task 8 Step 6의 4번.

## 지켜볼 것

1. **`Cargo.lock`의 `libc`를 0.2.189에 고정해 둔 것을 푼다.** rust-lang/libc#5606이 나간 릴리스가 나오면 `cargo update -p libc`로 풀고, `ios`와 `ios-release`가 초록인지 본다. 그때까지는 **`cargo update`를 하지 않는다.**
   - 이유: libc 0.2.190이 `_dyld_image_count` 같은 함수를 macOS에서만 선언해서, `backtrace` 0.3.76(flutter_rust_bridge → allo-isolate 경유)이 iOS 타깃에서 E0425로 깨진다(rust-lang/libc#5601). 전체 `cargo update`는 이 고정을 풀어서 두 iOS 작업을 빨갛게 만든다.
   - 위치: `Cargo.lock:1341-1343`, 커밋 `0a287bd`.
2. **Flutter가 flutter/flutter#193142를 내놓으면 CI의 우회를 한꺼번에 걷는다.** Flutter를 올릴 때마다 그 수정이 들어갔는지 먼저 본다(2026-10-08에 그 PR은 열려 있고 승인된 상태였다). 걷을 것: (a) `tools/ci/flutter-181771.patch`와 `Patch the Flutter tool` 단계, (b) 두 번째·세 번째 시도 단계와 작업 `timeout-minutes` 50(→ 줄인다), (c) 시도 2·3의 `-v`, (d) 시도 앞의 `ps`·`pkill` 줄, (e) `Simulator diagnostics` 단계와 그 문구, (f) `simctl terminate`에 박힌 번들 ID `dev.chatproject.chatapp`, (g) 주석의 "hang" 표현과 이슈 번호, (h) 자동 테스트 설계 6절의 "두 번까지 더".
   - 이유: 패치는 그 PR의 `simulators.dart` 부분이고, 수정이 들어간 Flutter에서는 `git apply`가 일부러 크게 실패하도록 해 두었다. 재시도는 그 수정을 믿기 전까지의 안전망이다. 이 단계들에 있는 `shellcheck` 정보 메시지 4개(SC2009 2개, SC2012 2개)도 함께 사라진다.
   - 위치: `tools/ci/flutter-181771.patch`, `.github/workflows/ci.yml:217,237-248,270-305,308-325`, [자동 테스트 설계](../specs/2026-10-05-test-automation-design.md) 6절.
3. **Flutter를 3.44.x 밖으로 올리기 전에 `chat_ffi`의 Swift Package Manager 지원을 확인한다.**
   - 이유: Flutter가 iOS 빌드 로그에 "The following plugins do not support Swift Package Manager for ios: chat_ffi"와 "This will become an error in a future version of Flutter"를 찍으므로, cargokit과 flutter_rust_bridge 템플릿이 SPM을 지원하기 전에는 그 버전 이후로 올릴 수 없다.
   - 위치: `app/rust_builder/ios/chat_ffi.podspec`, `ios`와 `ios-release` 작업의 `flutter` 단계 로그.
4. **2026-10-19부터 11-19 사이에 `ubuntu-latest`가 26.04로 바뀌는 동안 `android` 작업을 지켜본다.** 빨개지면 `runs-on`을 `ubuntu-24.04`로 고정한다. 옮긴 뒤에는 `Free disk space` 단계가 찍는 `df -h /` 두 줄을 먼저 읽는다. (M1a가 넘긴 일의 M1b 절 3·4번을 여기로 옮겼다.)
   - 이유: 이 기간에 `android`가 갑자기 빨개지면 원인은 코드가 아니라 러너 이미지일 가능성이 크고(runner-images#14748), 그 단계의 경로는 24.04 배치에 맞춘 것이라 없는 경로의 `rm -rf`는 조용히 성공해서 디스크가 안 비어도 오류가 나지 않는다.
   - 위치: `.github/workflows/ci.yml:26,42,57,89,134`(`runs-on: ubuntu-latest`), `:137-152`(`Free disk space`).
5. **`ios` 작업의 `pkill` 경로는 아직 한 번도 돈 적이 없다.** 두 번째 시도 단계가 처음 돌 때 로그의 `ps` 줄과 `pkill` 결과를 읽고, 그 단계의 셸이 살아남았는지 본다.
   - 이유: 패치 뒤 다섯 번의 실행에서 첫 시도가 모두 통과해 두 번째·세 번째 시도는 건너뛰었으므로, 그 줄의 패턴(`flutter_tools.snapshot`, `log stream`)이 러너의 셸을 죽이지 않는지는 읽어서만 믿고 있다.
   - 위치: `.github/workflows/ci.yml:290-293,301-304`.
6. **첫 시험 시도의 시간이 한도(`timeout-minutes: 15`)에 다가가는지 본다.**
   - 이유: 패치 전에는 첫 시도가 900초 한도의 625초까지 간 적이 있고, 패치 뒤 다섯 번은 367–565초였다. 느려지면 정상 실행이 한도에 걸려 재시도 뒤로 숨는다.
   - 위치: `.github/workflows/ci.yml:276-281`.
7. **`apns-probe`를 Task 10 뒤에도 쓰게 되면 고친다.** (a) 페이로드가 너무 크면 `seq`와 `done:` 요약 없이 중간에 멈춘다. (b) `start_seq + count`가 `u32`라서 `--start-seq 4294967295 --count 1`은 디버그에서 패닉이고 릴리스에서는 빈 반복으로 종료 코드 0이다. (c) 테스트가 크기 경계를 `<=`로만 확인하고, 임시 파일이 남으며, 요청 조립(URL·헤더)과 패딩된 base64 왕복은 시험하지 않는다. (d) "ciphertext line is not base64" 오류에 줄 번호가 없고 푸시마다 `body.clone()`을 한다.
   - 이유: 모두 쓰는 사람이 드문 스파이크 도구의 흠이라 Task 10까지는 두었다. M7의 APNs 발송 코드가 이 도구에서 가져오면 같이 옮겨 간다.
   - 위치: `spikes/c-push/apns-probe/src/main.rs:105-106,116,150,170,192,255`.
8. **스파이크 JSON 12개와 `.err` 2개에 iroh 엔드포인트 ID(공개키 64자리)가 남아 있다.** 이력을 다시 쓰는 일이 또 생기면 함께 지운다.
   - 이유: 비밀이 아니고 주소도 붙어 있지 않아서, M1b의 이력 정리에서는 그대로 두었다.
   - 위치: `spikes/` (`grep -rE '[0-9a-f]{64}' spikes`로 찾는다).
9. **공개 저장소 이력 스캔을 게이트로 만들면 `224.0.0.251`을 허용 목록에 넣는다.**
   - 이유: `scan_history.py`가 mDNS 그룹 주소를 공인 주소로 보고하는데, M1b 계획이 일부러 쓰는 값이다.
   - 위치: [M1b 계획](2026-10-05-m1b-ios-pipeline.md) Task 1의 `scan_history.py`(저장소 밖 `$SCRUB`), Task 10 Step 5.
