# 자동 테스트 구성 설계 — 에뮬레이터와 여러 기기 시나리오

- 작성일: 2026-10-05
- 상태: 대화에서 1–3부 승인(2026-10-05). 이 문서는 사용자 검토 대기.
- 관련 문서: 설계 [`2026-10-04-p2p-chat-design.md`](2026-10-04-p2p-chat-design.md) (rev.3), 로드맵 [`../plans/2026-10-04-p2p-chat-roadmap.md`](../plans/2026-10-04-p2p-chat-roadmap.md), M1a 계획 [`../plans/2026-10-04-m1a-core-skeleton.md`](../plans/2026-10-04-m1a-core-skeleton.md)

---

## 1. 목적과 범위

**목적**

1. 폰을 연결하지 않아도 테스트가 매번 자동으로 돈다(이 PC와 CI).
2. 실제 기기는 Android 폰 1대와 iPhone 1대뿐이다. 그래서 여러 기기가 필요한 시나리오(1:1, 그룹, 오프라인 → 메일박스, 큐)를 에뮬레이터, Windows 앱, 화면 없는 Rust 참가자로 자동화한다.
3. 통신사망, 배터리, 실제 푸시 지연처럼 에뮬레이터로 흉내 낼 수 없는 것은 실기기에서 손으로 확인한다.

**범위**

- 넣는 것: Android 에뮬레이터(이 PC는 Windows, CI는 Linux), Windows 앱 여러 개, 화면 없는 Rust 참가자(`chat_node`), iOS 시뮬레이터(M1b부터).
- 빼는 것: OS 화면 조작 자동화(알림 권한 창, 잠금 화면 알림 누르기)는 M7에서 Patrol 같은 도구를 다시 검토한다. 통신사망에서의 직접 연결 비율은 실기기로만 잰다.

**결정 경위 (2026-10-05)**

- 범위: 여러 기기 시나리오까지 자동화한다.
- 방식: 처리 규칙은 화면 없는 Rust 참가자로(B), 기기에서만 드러나는 것은 앱 원격 조종 모드로(A). 둘이 같은 조종 프로토콜을 쓴다. 기성 UI 도구 중심(C)은 택하지 않았다.

## 2. 시험의 다섯 층

| 층 | 무엇을 | 어디서 | 언제 |
|---|---|---|---|
| ① Rust 단위·여러 참가자 | 코어의 처리 규칙. 네트워크에 지연·중복·순서 뒤바뀜·유실을 일부러 넣고, 한 프로세스 안에서 참가자 여러 명을 돌린다. 시계를 바꿔 끼워 재시도·24시간 보관·7일 가동률을 기다리지 않는다 | 이 PC, CI(Linux) | 매 커밋 |
| ② 화면 | Flutter 위젯 + 가짜 코어 | 이 PC, CI(Linux) | 매 커밋 |
| ③ 기기 1대 | Rust 연결 계층과 OS 동작(Kotlin이 같은 코어를 쓰는지, 키 보관함, DB) | Windows 앱, Android 에뮬레이터, iOS 시뮬레이터(M1b부터) | 매 커밋 (iOS 포함. 저장소가 공개라 macOS 러너가 무료다) |
| ④ 여러 기기 시나리오 | 원격 조종 모드 앱(에뮬레이터 2–3대, Windows 앱)과 `chat_node`가 함께 대화한다. 비행기 모드, 강제 종료, Doze를 건다 | 이 PC에서 명령 한 줄, CI 매일 밤 | M2부터 |
| ⑤ 실기기 | KT LTE 직접 연결 비율, 실제 푸시 지연, 제조사 배터리 관리, iPhone 알림 확장 메모리 | Android 폰, iPhone(TestFlight) | 마일스톤 끝에 손으로 |

## 3. 원칙

1. **외부망에 기대지 않는다.** ④는 공개 n0 릴레이와 DHT를 쓰지 않는다. 이 PC에 띄운 로컬 릴레이를 쓰고, 상대 주소와 초대장은 지휘 프로그램이 넘겨준다. 결과가 매번 같고, CI가 공개 릴레이 사용량 제한에 걸리지 않는다.
2. **에뮬레이터에서는 "연결됐다, 받았다"만 확인한다.** 에뮬레이터는 각자 가상 공유기(NAT) 뒤에 있어서 연결이 릴레이로 가기 쉽다. 직접 경로 여부는 ⑤에서 본다.
3. **시간은 주입한다.** M4부터 코어는 시계를 `Clock` trait으로 받는다. 시험은 시계를 앞으로 돌려 재시도·TTL·가동률을 확인한다. 실제로 기다리는 시험을 만들지 않는다.
4. **실패하면 증거를 남긴다.** 참가자별 이벤트 기록, logcat, Windows 앱 출력, `chat_node` 로그를 자동으로 모은다(4.7절).
5. **시험 전용 기능은 배포판에 들어가지 않는다**(7절).

## 4. 여러 기기 시험 장치

### 4.1 구성

```
┌─────────────── 이 PC (CI에서는 Linux 러너) ───────────────┐
│  지휘 프로그램  tools/e2e  (Dart, package:test 시나리오)    │
│   ├ 조종 서버      ws://127.0.0.1:47000                     │
│   ├ 로컬 릴레이    chat_node relay  :3340                    │
│   └ 기기 조작      adb: 비행기 모드·강제 종료·Doze·네트워크 지연 │
└───────┬───────────────────┬───────────────────┬──────────┘
        │ adb reverse        │                   │
 에뮬레이터 a·b          Windows 앱 w1·w2      chat_node h1·h2
 (조종 모드 앱)          (조종 모드, 폴더 따로)  (화면 없는 참가자)
        └──── 셋 다 같은 Rust 조종 루프 (chat_core의 test-hooks) ────┘
```

### 4.2 지휘 프로그램 `tools/e2e`

Dart 패키지 하나다. M1a에서는 기기 관리 명령만 만들고(`bin/devices.dart`), M2에서 시나리오 실행을 더한다.

- 로컬 릴레이를 띄우고 시험이 끝나면 내린다.
- 참가자를 띄운다.
  - Android: 디버그 APK를 한 번 빌드해 여러 에뮬레이터에 설치한다. 실행할 때 인텐트 추가 값으로 조종 서버 주소와 참가자 이름을 준다.
  - Windows: 디버그 exe를 한 번 빌드해 여러 번 실행한다. 실행 인자로 조종 서버 주소, 참가자 이름, 데이터 폴더를 준다.
  - `chat_node`: `chat_node puppet --name h1 --control ws://127.0.0.1:47000 --data-dir <임시 폴더>`
- 조종 서버를 열고, 참가자가 붙으면 이름으로 등록한다.
- 시나리오는 `package:test` 파일로 쓴다(`tools/e2e/test/*_test.dart`). 시나리오가 쓰는 함수:
  - `h.android(name)`, `h.windows(name)`, `h.headless(name)` → 참가자
  - `participant.call(op, args)` → 결과 값. 오류면 `ErrorCode` 이름을 담은 예외
  - `participant.expectEvent(kind:, within:, …)` → 조건에 맞는 이벤트가 올 때까지 기다림
  - 기기 조작(4.6절)

### 4.3 조종 루프 — Rust 한 곳

- 위치: `chat_core`의 `test-hooks` 기능 아래 `puppet` 모듈.
- 하는 일: 조종 서버에 웹소켓으로 붙고, `hello`를 보내고, 받은 `cmd`를 코어 명령으로 실행해 `result`로 답하고, 코어 이벤트를 전부 `event`로 흘려보낸다.
- 들어오는 길
  - Android 앱: `MainActivity`가 인텐트 추가 값(`control_url`, `puppet_name`)을 읽어 Dart에 넘긴다. Dart는 디버그 빌드(`kDebugMode`)일 때만 FRB 함수 `test_hooks_start(control_url, name)`을 부른다.
  - Windows 앱: Dart `main(List<String> args)`가 `--control-url`, `--puppet-name`, `--data-dir`를 읽는다. 나머지는 Android와 같다.
  - `chat_node`: 같은 `puppet` 모듈을 직접 부른다.
- 조종 서버 주소는 루프백(`127.0.0.1`, `localhost`)만 받는다. 다른 주소면 시작을 거부한다.
- 앱이 강제 종료된 뒤 다시 켜지면 같은 이름으로 다시 붙는다. 지휘 프로그램은 이름으로 같은 참가자임을 안다.

### 4.4 조종 프로토콜

웹소켓 위의 JSON, 메시지 하나가 텍스트 프레임 하나다.

```json
{"type":"hello","name":"a","kind":"android","core_version":"0.1.0","instance_id":"8123-18f3a2"}
{"type":"cmd","id":7,"op":"send_text","args":{"conversation_id":"c1","body":"안녕"}}
{"type":"result","id":7,"ok":true,"value":{"message_id":"m9"}}
{"type":"result","id":8,"ok":false,"error":{"code":"AlreadyStartedDifferentConfig","detail":"core already runs with /data/user/0/dev.chatproject.chat_app/files/core"}}
{"type":"event","seq":42,"event":{"kind":"connection_state","direct_peers":1,"mailbox_ok":false,"queue_ok":null}}
```

- `kind`: `android` | `windows` | `ios` | `headless`.
- `op` 이름은 설계 12.1절 코어 명령 이름과 같다(`core_start`, `create_invite`, `accept_invite`, `send_text`, `load_messages`, `set_relay_map`, `set_queue_mode`, `set_mailbox_mode` …). 코어에 명령이 생길 때 조종 루프에도 같은 이름으로 더한다.
- 시험에만 쓰는 `op`
  - `set_offline { offline: bool }`: 전송 계층을 끊었다 잇는다. Windows 앱과 `chat_node`의 오프라인 흉내. 에뮬레이터는 실제 비행기 모드를 쓴다.
  - `clock_advance { seconds: u64 }`: 주입한 시계를 앞으로 돌린다(M4부터).
- 오류 `code`는 `ErrorCode` 이름 그대로다. 시나리오는 오류 문자열을 비교하지 않는다.
- `event.seq`는 참가자별로 1씩 는다. 지휘 프로그램은 빠진 번호를 보면 시험을 실패로 처리한다.

### 4.5 외부망 차단

- 로컬 릴레이: `chat_node relay --port 3340`. iroh-relay 서버 모듈을 개발 모드(TLS 없음)로 띄운다. 버전은 워크스페이스의 iroh와 같다.
- Android 기기: `adb reverse tcp:3340 tcp:3340`, `adb reverse tcp:47000 tcp:47000`. 에뮬레이터와 USB 폰 모두 `127.0.0.1`로 이 PC에 닿는다.
- 모든 참가자는 시작 직후 `set_relay_map(["http://127.0.0.1:3340"])`을 받는다.
- 상대 주소(EndpointAddr)와 초대장은 지휘 프로그램이 한쪽에서 받아 다른 쪽에 넘긴다. DHT 조회는 ④에서 쓰지 않는다.

### 4.6 기기 조작

| 조작 | Android 에뮬레이터 | Windows 앱 / chat_node |
|---|---|---|
| 오프라인 | `adb shell cmd connectivity airplane-mode enable` / `disable` | `set_offline` |
| 강제 종료 | `adb shell am force-stop dev.chatproject.chat_app` | 프로세스 종료 |
| 다시 실행 | `adb shell am start -n dev.chatproject.chat_app/.MainActivity --es control_url ws://127.0.0.1:47000 --es puppet_name a` | 같은 인자로 다시 실행 |
| Doze | `adb shell dumpsys battery unplug` → `adb shell dumpsys deviceidle force-idle`, 해제는 `unforce` → `dumpsys battery reset` | 해당 없음 |
| 느린 망 | `adb emu network delay gprs`, 해제는 `none` | 해당 없음 |

### 4.7 실패 수집

시험 하나가 실패하면 `build/e2e/<실행 시각>/<시험 이름>/`에 남긴다.

- `<참가자>.events.jsonl`: 그 참가자에게서 받은 `event` 전부
- `<에뮬레이터>.logcat.txt`: `adb -s <serial> logcat -d`
- `<windows 참가자>.out.txt`, `<chat_node 참가자>.log.txt`
- `scenario.txt`: 보낸 `cmd`와 받은 `result` 순서

## 5. 에뮬레이터 구성

**이 PC (Windows, WHPX 가속 확인됨, RAM 32 GB)**

- 시험 전용 AVD 2개: `chat_e2e_1`, `chat_e2e_2`.
  - 이미지: 이미 설치된 `system-images;android-36;google_apis_playstore;x86_64`. 다운로드 없음.
  - RAM 2 GB. 콘솔 포트 5554, 5556 → 시리얼 `emulator-5554`, `emulator-5556`.
  - 기존 `flutter_emulator`, `flutter_emulator_2`, `flutter_emulator_3`는 건드리지 않는다. 사용자가 FCM 손 확인 등에 쓴다.
- 처음 한 번: `avdmanager`로 만들고, `-no-snapshot-load`로 완전 부팅한 뒤 `adb emu avd snapshot save clean`으로 깨끗한 상태를 저장하고 끈다.
- 이후 실행: `emulator -avd chat_e2e_1 -port 5554 -snapshot clean -no-snapshot-save -no-window -no-audio -no-boot-anim`. 매번 같은 깨끗한 상태에서 빠르게 켜지고, 바뀐 상태는 버린다.
- 부팅 대기: `adb -s <serial> wait-for-device` 뒤 `getprop sys.boot_completed`가 `1`이 될 때까지 기다린다. 180초를 넘기면 실패.
- 에뮬레이터 2대를 함께 띄우기 전에 남은 메모리가 6 GB 이상인지 사람이 확인한다(M1a 계획의 "시작 전 준비" 2번). 도구가 메모리를 재지는 않는다.

**CI (GitHub Actions, Linux)**

- `reactivecircus/android-emulator-runner@v2`, `api-level: 36`, `target: google_apis`, `arch: x86_64`.
- KVM 권한을 여는 단계를 먼저 둔다(이 액션 README의 udev 규칙).
- AVD 스냅숏은 `actions/cache`로 다음 실행에 재사용한다.

## 6. iOS 시뮬레이터 (M1b)

- macOS 러너에서 `tools/ci/pick_simulator.py`가 가장 새 iOS 런타임의 iPhone을 고른다. `xcrun simctl bootstatus <udid> -b`로 그 시뮬레이터를 켠다. `flutter test integration_test/core_test.dart -d <udid>`로 시험한다. 시뮬레이터는 앱 서명이 필요 없다. flutter/flutter#181771 때문에 시험이 멈출 수 있어서, 앞 시도가 멈추거나 실패하면 두 번까지 더 시도한다.
- `xcrun simctl push`는 알림 확장(NSE)을 실행하지 않는다. Xcode 11.4 릴리스 노트의 Known Issues(55822721)와 Xcode 14 릴리스 노트에 적혀 있고, Xcode 26.x까지 바뀌지 않았다. GitHub 러너의 시뮬레이터는 실제 APNs 토큰도 받지 못한다. 그래서 CI는 NSE가 빌드되어 앱 안(`Runner.app/PlugIns/NotificationService.appex`)에 들어갔는지만 확인한다. 복호와 메모리는 TestFlight 빌드로 잰다.
- 알림 확장 메모리와 APNs 보관 개수는 시뮬레이터로 대신할 수 없다. TestFlight로 실제 iPhone에서 잰다.

## 7. 안전장치

1. 조종 루프, `set_offline`, `clock_advance`는 `chat_core`의 `test-hooks` 기능 안에만 있다. `chat_ffi`도 같은 이름의 기능으로 이를 켠다.
2. `app/rust/cargokit.yaml`이 **디버그 빌드에서만** 이 기능을 켠다:

   ```yaml
   cargo:
     debug:
       extra_flags: ["--features", "test-hooks"]
   ```

   릴리스와 프로파일 빌드는 이 기능 없이 빌드된다.
3. FRB 함수 `test_hooks_start`는 언제나 존재한다. 기능이 꺼진 빌드에서는 `ErrorCode::NotAvailable`만 돌려준다. Dart의 실행 인자 처리는 `kDebugMode` 안에만 있어 릴리스에서 빠진다.
4. 조종 코드에는 표식 문자열 `chat-test-hooks:v1`을 둔다. CI는 릴리스 APK의 `libchat_ffi.so`와 Windows 릴리스의 `chat_ffi.dll`에 이 문자열이 **없는지** 검사한다.
5. 조종 서버는 `127.0.0.1`에만 열고, 조종 루프는 루프백이 아닌 주소를 거부한다(4.3절).

## 8. CI가 도는 주기

| 언제 | 작업 |
|---|---|
| 매 push | Rust 테스트(Linux), 화면 위젯 테스트(Linux), Windows 빌드 + 통합 테스트, Android 에뮬레이터 통합 테스트, 릴리스 표식 검사(조종 코드가 생기는 M2부터), iOS 시뮬레이터 통합 테스트(M1b부터) |
| 매일 밤 | 여러 기기 시나리오(M2부터): 에뮬레이터 2대 + `chat_node` |

저장소는 2026-10-05부터 공개다. 공개 저장소에서는 macOS를 포함한 기본 러너가 무료라서, iOS도 매 push마다 돈다. 여러 기기 시나리오는 시간이 오래 걸려서 매일 밤에만 돌린다.

## 9. 도입 순서

| 마일스톤 | 더하는 것 |
|---|---|
| M1a | `tools/e2e/bin/devices.dart`(AVD 만들기·켜기·대기·시험·끄기), 시험 전용 AVD 2개, Android 통합 테스트를 에뮬레이터 기본으로, CI 에뮬레이터 작업, Rust 테스트를 Linux로 |
| M1b | iOS 시뮬레이터 CI 작업, NSE 빌드·내장 확인 |
| M2 | 조종 루프(`test-hooks`), `chat_node`(조종 참가자 + 로컬 릴레이), 앱 조종 모드 실행 인자, 지휘 프로그램 시나리오 실행, 첫 시나리오(에뮬레이터 ↔ `chat_node` ↔ Windows 앱), CI 야간 작업, 릴리스 표식 검사 |
| M3 | 시나리오: 초대 → 연락처 추가, 기기 연결(두 방향), 복구 문구로 되살리기 |
| M4 | `Clock` 주입과 `clock_advance`. 시나리오: 강제 종료 뒤 보내기 줄 재개 |
| M5 | ①에 가짜 네트워크(지연·중복·순서 뒤바뀜·유실) |
| M6 | 시나리오: 에뮬레이터 비행기 모드 → Windows 앱이 메일박스로 받음 → 복귀 후 수신 |
| M7 | 이 PC에서만: Google Play 이미지 에뮬레이터 + 실제 FCM. CI는 가짜 푸시 경로. Patrol 재검토 |
| M8 | 로컬 큐 서버. 시나리오: 메일박스 없는 참가자의 오프라인 수신 |
| M9 | 시나리오: 3–5명 그룹, 변경 요청 경쟁 |

## 10. 열린 질문

- (M1b에서 답함) `xcrun simctl push`는 알림 확장을 실행하지 않는다. 6절.
- Linux CI 러너에서 에뮬레이터 2대를 함께 돌릴 때 걸리는 시간 (M2에서 재고, 길면 1대 + `chat_node`로 줄인다).
- 에뮬레이터끼리는 직접 경로를 기대하지 않는다. 릴레이 경로로 통과하면 충분하다 (M2에서 실제로 어떤지 기록).
