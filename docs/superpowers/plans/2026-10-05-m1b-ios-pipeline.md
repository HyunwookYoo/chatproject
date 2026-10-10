# M1b iOS 파이프라인 + 기기 실측 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 공개 저장소의 GitHub Actions macOS 러너가 iOS 앱과 알림 확장(NSE)을 빌드해 TestFlight에 올린다. 실제 iPhone에서 NSE 메모리와 APNs 보관 개수를 잰다.

**Architecture:**
- Mac이 없다. 그래서 iOS 빌드, 서명, 업로드는 모두 CI에서 한다.
- 알림 확장 타깃은 커밋하지 않는다. Ruby `xcodeproj` 스크립트가 CI에서 빌드 직전에 매번 프로젝트에 더한다. 스크립트가 원본이고, 두 번 돌려도 결과가 같다.
- NSE는 Flutter 없이 Rust 정적 라이브러리 `chat_nse`를 UniFFI Swift로 부른다. NSE는 번들에 든 200 leaf OpenMLS 그룹 상태로 푸시의 암호문을 복호한다. 자기 메모리는 App Group 파일에 쓴다.
- 앱의 "알림 측정" 화면이 그 파일과 APNs 토큰을 보여 준다.
- APNs 발송은 이 PC의 Rust 명령줄 도구가 한다.

**Tech Stack:** Flutter 3.44.6 / Dart 3.12.2, Xcode 26.6(`macos-26` 러너), Rust 1.92.0, UniFFI 0.32.2(Swift), OpenMLS 0.9.0, Ruby `xcodeproj` 1.28.1, `apple-actions/import-codesign-certs` v7.0.0, `apple-actions/upload-testflight-build` v5.5.0, reqwest 0.13 + jsonwebtoken 11(APNs), git-filter-repo 2.47.0.

**Spec:**
- 설계 [`docs/superpowers/specs/2026-10-04-p2p-chat-design.md`](../specs/2026-10-04-p2p-chat-design.md) (rev.3): 8.2, 12.3, 14, 17절 #1·#12, 18절.
- 자동 테스트 설계 [`docs/superpowers/specs/2026-10-05-test-automation-design.md`](../specs/2026-10-05-test-automation-design.md): 6, 8, 9, 10절.
- 범위와 완료 기준: [`2026-10-04-p2p-chat-roadmap.md`](2026-10-04-p2p-chat-roadmap.md)의 M1b.
- M1a가 넘긴 일: [`2026-10-05-m1a-carry-forward.md`](2026-10-05-m1a-carry-forward.md)의 M1b 절. 8개 항목 모두 이 계획의 태스크에 들어 있다(아래 표).

## 사용자 결정 (2026-10-05)

1. **저장소를 공개한다.** 공개 저장소에서는 macOS를 포함한 기본 러너가 무료다. 공개하기 전에 집 공인 IP, 폰 IP, 폰 시리얼, Windows 계정 경로를 이력에서 가린다.
2. **iOS 번들 ID**는 다음과 같다. Android의 `dev.chatproject.chat_app`은 그대로 둔다. iOS 번들 ID에는 밑줄을 쓸 수 없다.
   - 앱: `dev.chatproject.chatapp`
   - 알림 확장: `dev.chatproject.chatapp.NotificationService`
   - App Group: `group.dev.chatproject.chatapp`
3. **수동 서명.** 같은 팀의 기존 Apple Distribution 인증서(.p12)를 다시 쓴다. 앱용과 NSE용 App Store 프로파일 2개를 새로 만든다. 업로드는 App Store Connect API 키로 한다.

## 로드맵·설계와 달라지는 점

| 원래 | 이 계획 | 이유 |
|---|---|---|
| 로드맵 M1b: App Store Connect API 키로 클라우드 서명 | 수동 서명 | 사용자 결정 3. BeanProfile에서 같은 러너로 검증된 방식이다 |
| 로드맵 M1b: APNs .p8을 GitHub 시크릿에 | .p8은 이 PC의 `spikes/c-push/secrets/`에만 | M1b의 APNs 발송은 이 PC에서만 한다. 서버용 시크릿은 M7에서 만든다 |
| 자동 테스트 설계 8절: iOS는 main push와 매일 밤 | iOS 시뮬레이터 작업도 매 push | 공개 저장소라 macOS 러너가 무료다 |
| 자동 테스트 설계 10절: `simctl push`가 NSE를 실행하는지 확인 | 실행하지 않는다고 기록하고, CI는 NSE 빌드와 내장만 확인 | Xcode 11.4·14 릴리스 노트에 "simulated push는 NSE를 실행하지 않는다"고 적혀 있고, 26.x까지 고쳐지지 않았다. GitHub 러너의 시뮬레이터는 실제 APNs 토큰도 받지 못한다 |
| 설계 12.3: 코어를 `full`/`nse` 기능으로 나눈다 | M1b는 별도 크레이트 `core/chat_nse`가 OpenMLS를 직접 쓴다 | MLS는 M5에서야 `chat_core`에 들어온다. 그때 `chat_nse`가 `chat_core`의 `nse` 기능을 감싸게 바꾼다 |
| 자동 테스트 설계 3절 5번: 시험 전용 기능은 배포판에 넣지 않는다 | 알림 측정 화면과 NSE 측정 코드가 TestFlight 빌드에 들어간다 | 기기 실측은 TestFlight 빌드로만 할 수 있다. 내부 테스트 전용이고, M7이 실제 NSE 경로로 바꾸면서 지운다 |
| (없음) | vendored cargokit이 `CARGOKIT_TOOLCHAIN` 환경 변수를 읽게 고친다 | cargokit은 `rust-toolchain.toml`을 무시하고 늘 `rustup run stable`로 빌드한다. 그래서 M1a의 Android·Windows 앱은 1.92.0이 아니라 러너의 stable(1.98.1)로 빌드됐다 |

## M1a가 넘긴 일 (M1b 절) → 태스크

| 넘긴 일 | 태스크 |
|---|---|
| 1. 바인딩을 한 번에 다시 만드는 스크립트와 CI 검사 | Task 5 |
| 2. checkout·setup-java·cache를 Node 24 버전으로 | Task 2 |
| 3. `ubuntu-latest` 26.04 전환 동안 `android` 작업 지켜보기 | 진행 방식(지켜볼 것) |
| 4. 26.04 뒤에는 `Free disk space`의 `df` 두 줄 먼저 읽기 | 진행 방식(지켜볼 것) |
| 5. `concurrency`와 Rust 캐시 | Task 2 |
| 6. 액션을 커밋 SHA로 고정, `permissions: contents: read` | Task 2 |
| 7. `native.rs`·`global.rs`의 첫 주석 | Task 3 |
| 8. iOS 번들 ID 확정 | Task 3 |

## Global Constraints

- Rust는 `rust-toolchain.toml`의 1.92.0이다. 새 크레이트 `chat_nse`, `nse_fixture`, `apns-probe`는 edition 2024를 쓴다.
- UniFFI는 `=0.32.2`다. `chat_ffi`, `chat_nse`, `tools/uniffi-bindgen`이 모두 같은 버전을 쓴다.
- OpenMLS는 `openmls =0.9.0`, `openmls_rust_crypto =0.6.0`, `openmls_basic_credential =0.6.0`이다.
  - 사이퍼수트는 `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`다.
  - 설정은 rev3(`max_past_epochs(5)`, `SenderRatchetConfiguration::new(200, 1000)`)다.
- Flutter는 3.44.6이다. iOS 작업은 `runs-on: macos-26`에서 `xcode-select`로 `/Applications/Xcode_26.6.app`을 고른다.
- iOS 최소 버전은 **15.0**이다. 이 값은 Runner와 RunnerTests(프로젝트 설정), NotificationService, `chat_ffi.podspec`에 똑같이 들어간다.
- ID와 이름:
  - 팀 ID: `9J2FNH63M2`. BeanProfile과 같은 팀이다. 시작 전 준비 1번에서 확인한다.
  - 프로파일 이름: `ChatProject App Store`, `ChatProject NSE App Store`
  - GitHub Environment: `testflight`
- `testflight` Environment 시크릿은 다음 7개다. 저장소 수준 시크릿은 만들지 않는다.
  - `APPSTORE_CERTIFICATES_FILE_BASE64`
  - `APPSTORE_CERTIFICATES_PASSWORD`
  - `APPSTORE_PROFILE_APP_BASE64`
  - `APPSTORE_PROFILE_NSE_BASE64`
  - `APPSTORE_ISSUER_ID`
  - `APPSTORE_API_KEY_ID`
  - `APPSTORE_API_PRIVATE_KEY`
- APNs:
  - TestFlight 빌드는 운영 환경 `https://api.push.apple.com`을 쓴다.
  - `apns-topic`은 앱 번들 ID다.
  - 페이로드는 4,096 B 이하이고, `aps.alert`(title과 body)와 `aps.mutable-content: 1`이 있다.
  - 사용자 정의 키(`seq`, `sent_at`, `e`)는 `aps`와 같은 층에 둔다.
- NSE 메모리 기준은 **피크 15 MB 미만**이다(설계 12.3, 14). 피크는 `TASK_VM_INFO`의 `ledger_phys_footprint_peak`다.
- NSE 기록 파일은 App Group 루트의 `nse_memory.jsonl`이다. 한 줄이 JSON 하나다. 형식은 Task 6이 정하고 Task 7이 읽는다.
- 색은 `app/lib/theme/tokens.dart`에만 둔다. 화면 문구와 알림 문구는 한국어로 쓴다.
- 커밋 금지 파일:
  - M1a 목록: `spikes/c-push/secrets/`, `*.p8`, `*.p12`, `*.jks`, `*.keystore`, `google-services.json`, `GoogleService-Info.plist`
  - 새로 더하는 것: `*.mobileprovision`, `*.cer`
- 공개 저장소 규칙:
  - 비밀값은 Environment 시크릿에만 둔다.
  - 워크플로는 `push`, `pull_request`, `workflow_dispatch`로만 시작한다. `pull_request_target`은 쓰지 않는다.
  - 비밀값과 프로파일 내용을 로그에 찍지 않는다.
  - 모든 액션은 커밋 SHA로 고정한다.
- 커밋 메시지는 Conventional Commits로 쓰고, 끝에 실행 세션이 정한 attribution 줄을 붙인다.

## Review Focus

1. **NSE가 메모리 한도나 30초를 넘겨 죽거나, 복호에 실패한다.**
   - 기대: 알림은 원래 문구("알림 N번")로라도 뜬다. 측정 기록에 "시작함"이 남아 있어 중단을 알아볼 수 있다.
   - 확인: Task 4 `corrupted_ciphertext_is_an_error_not_a_panic`, Task 7 `start_without_end_is_reported_as_stopped`.
2. **깨진 상태 파일이 거대한 길이 값을 담고 있다.**
   - 기대: NSE가 수 GB를 할당하다 죽지 않고 `BadState` 오류를 돌려준다.
   - 확인: Task 4 `oversized_length_is_rejected_without_allocating`.
3. **App Group 기록 파일이 없거나 일부 줄이 깨졌다.** 원인은 처음 실행, 잘못된 그룹, 쓰는 도중 종료 등이다.
   - 기대: 측정 화면이 깨지지 않는다. 기록이 없으면 "아직 기록이 없어요"를 보여 주고, 깨진 줄은 건너뛴다.
   - 확인: Task 7 `garbage_lines_are_skipped`, `shows empty state without records`.
4. **APNs 페이로드가 4,096 B를 넘거나 암호문 파일이 비었다.**
   - 기대: 보내기 전에 거부하고 이유를 출력한다. Apple이 거절하면 상태 코드와 `reason`을 그대로 찍는다.
   - 확인: Task 9 `payload_size_boundary_is_4096`, `empty_ciphertext_file_is_rejected`.
5. **TestFlight 작업을 다시 실행하거나 서명 재료가 틀렸다.** 재실행하면 같은 빌드 번호가 되고, 프로파일에는 그룹이 빠졌거나 개발용이거나 다른 ID일 수 있다.
   - 기대: 업로드 전에 명확한 `::error::`로 멈춘다.
   - 확인: Task 8의 `Refuse re-runs` 단계와 `Install provisioning profiles` 단계의 검사를 정적 검사 스크립트 `tools/ci/check_workflows.py`가 고정한다.

## 진행 방식

- **Task 1**은 main의 이력을 다시 쓰고 저장소를 바꾸는 운영 작업이다. 컨트롤러가 사용자와 함께 실행한다. 바깥으로 나가는 명령은 각각 실행 직전에 사용자에게 한 번 더 확인한다.
- **Task 2부터**는 다음과 같이 진행한다.
  - Task 1이 끝날 때 다시 쓴 main에서 `m1b-ios` 브랜치를 만들고, main으로 가는 draft PR을 연다. 이후 push마다 CI가 돈다.
  - 실행자(구현 에이전트 포함)는 `m1b-ios`를 origin에 push해도 된다. 이 계획을 승인하면 그 push에도 동의한 것으로 본다. main에 직접 push하거나 force-push하지 않는다.
- **TestFlight 실행.** `m1b-ios`의 커밋에 `testflight-<n>` 태그를 push해서 시작한다. Environment `testflight`는 `main`과 `testflight-*` 태그만 받는다. 실행마다 사용자가 GitHub에서 승인을 눌러야 한다.
- **Mac이 없다.** 그래서 iOS 코드는 CI에서만 컴파일된다. 각 태스크의 "CI 확인" 단계가 그 태스크의 시험이다. 실패하면 로그의 첫 오류를 고쳐 다시 push한다.
- **지켜볼 것.**
  - 2026-10-19부터 11-19 사이에 `ubuntu-latest`가 26.04로 바뀐다. 이때 `android`가 빨개지면 러너 이미지부터 의심하고 `ubuntu-24.04`로 고정한다.
  - 26.04로 옮긴 뒤에는 `Free disk space` 단계의 `df -h /` 두 줄을 먼저 읽는다.
- **병합.** 마지막 리뷰 뒤 PR을 merge commit이나 fast-forward로 병합한다. squash와 rebase는 쓰지 않는다. 이 문서와 M1a 계획이 커밋 SHA를 인용하기 때문이다.

## 시작 전 준비 (사용자)

1. **팀 ID를 확인한다.** developer.apple.com → Membership에서 Team ID가 `9J2FNH63M2`이고 Apple Developer Program이 유효한지 본다. 다르면 이 계획의 팀 ID를 모두 바꾼다.
2. **Task 1 전에:** 다시 쓴 이력을 새 공개 저장소에 올리는 데 동의한다. 이 계획을 승인하면 동의한 것으로 본다. 다만 이름 바꾸기, 새 저장소 만들기, push 직전에는 한 번씩 더 확인한다.
3. **Task 8 전에:** Apple 쪽 준비를 한다(Task 8 Step 1의 목록).
   - App ID 2개와 App Group
   - 프로파일 2개
   - APNs 키
   - App Store Connect 앱 레코드와 API 키
   - `testflight` Environment 시크릿 7개
4. **Task 10:** iPhone에 TestFlight 앱을 깔고 측정 절차를 함께 진행한다. 알림 필터링·multicast entitlement 신청서도 이때 제출한다.

## 파일 구조

```
ChatProject/
├─ .gitattributes                           *.bin binary                         Task 4
├─ .gitignore                               *.mobileprovision, *.cer             Task 2
├─ .github/
│  ├─ dependabot.yml                        github-actions 주간 갱신             Task 2
│  └─ workflows/
│     ├─ ci.yml                             SHA 고정·권한·동시 실행(2), ios(3,6), bindings(5)
│     └─ testflight.yml                     서명 빌드 → TestFlight              Task 8
├─ Cargo.toml                               멤버 chat_nse, nse_fixture / apns-probe 제외   Task 4, 9
├─ core/chat_nse/                           NSE용 MLS 복호 (staticlib + lib)       Task 4
│  ├─ Cargo.toml
│  └─ src/lib.rs, src/state.rs
├─ tools/
│  ├─ nse_fixture/                          200 leaf 그룹 상태와 암호문 생성기     Task 4
│  │  ├─ Cargo.toml
│  │  ├─ src/lib.rs, src/main.rs
│  │  └─ tests/decrypt.rs
│  ├─ gen_bindings.sh                       FRB·Kotlin·Swift 바인딩 재생성         Task 5
│  └─ ci/
│     ├─ check_workflows.py                 워크플로 정적 검사                     Task 2, 8
│     ├─ test_check_workflows.py                                                   Task 8
│     ├─ pick_simulator.py                  CI가 쓸 iPhone 시뮬레이터 고르기       Task 3
│     └─ test_pick_simulator.py                                                    Task 3
├─ app/
│  ├─ rust/src/native.rs, global.rs         첫 주석만                              Task 3
│  ├─ rust_builder/
│  │  ├─ ios/chat_ffi.podspec               iOS 15.0                               Task 3
│  │  └─ cargokit/build_tool/lib/src/builder.dart   CARGOKIT_TOOLCHAIN          Task 2
│  ├─ ios/
│  │  ├─ Runner.xcodeproj/project.pbxproj   번들 ID, 최소 버전                     Task 3
│  │  ├─ Runner/AppDelegate.swift           알림 권한·토큰 채널                     Task 7
│  │  ├─ Runner/Runner.entitlements         App Group + aps-environment            Task 6
│  │  ├─ scripts/add_notification_service.rb  NSE 타깃·서명 설정 (CI에서 실행)   Task 6, 8
│  │  └─ NotificationService/
│  │     ├─ NotificationService.swift, Info.plist, NotificationService.entitlements   Task 6
│  │     ├─ NotificationService-Bridging-Header.h                                  Task 6
│  │     ├─ Generated/chat_nse.swift, chat_nseFFI.h, chat_nseFFI.modulemap (생성됨) Task 5
│  │     └─ Fixture/nse_state.bin, ciphertexts.txt (생성됨)                        Task 4
│  ├─ lib/
│  │  ├─ main.dart                          iOS에서 측정 화면 연결                  Task 7
│  │  ├─ home/home_screen.dart              측정 화면 단추 (주입식)                 Task 7
│  │  └─ probe/probe_host.dart, nse_record.dart, probe_screen.dart                 Task 7
│  └─ test/nse_record_test.dart, probe_screen_test.dart, home_screen_test.dart    Task 7
├─ spikes/c-push/
│  ├─ apns-probe/                           APNs 발송 도구 (워크스페이스 밖)        Task 9
│  └─ RESULTS.md                            iOS 절 추가                             Task 10
├─ spikes/b-mls-memory/RESULTS.md           기기 실측 절 추가                       Task 10
└─ docs/                                    설계 8.2·14·17, 로드맵, 자동 테스트 설계  Task 3, 6, 10
```

---

### Task 1: 이력 정리와 공개 저장소 전환

컨트롤러가 사용자와 함께 진행하는 운영 작업이다. 작업 폴더는 저장소 밖의 `$SCRUB`다. 예: `SCRUB=/c/Users/<you>/AppData/Local/Temp/chatproject-scrub`, 또는 실행 세션의 scratchpad.

**이 계획에는 가릴 값의 원문을 적지 않는다.** 이 문서도 공개되기 때문이다. 원문은 `$SCRUB`의 파일에만 둔다.

**Files:**
- 저장소 밖(`$SCRUB`): `scan_history.py`, `scrub_history.py`, `known.txt`, `replacements.txt`, `ruleset.json`, `env.json`
- Modify(이력 전체): 2026-10-05 점검이 찾은 파일들
  - `docs/research/2026-08-23-p2p-chat-feasibility.md`
  - `docs/study/p2p-chat/index.html`
  - `spikes/a-connectivity/README.md`, `spikes/a-connectivity/RESULTS.md`, `spikes/a-connectivity/results/2026-10-04-kt/*`
  - `spikes/a-connectivity/scripts/*.sh`, `spikes/a-connectivity/.cargo/config.toml`
  - `spikes/c-push/RESULTS.md`
  - `docs/superpowers/plans/2026-10-04-m1a-core-skeleton.md`
  - `tools/e2e/test/emulators_test.dart`
- Modify(새 커밋, `m1b-ios`): `docs/superpowers/plans/2026-10-04-m1a-core-skeleton.md`(커밋 SHA 갱신)
- Create(새 커밋, `m1b-ios`): `docs/superpowers/plans/2026-10-05-m1b-ios-pipeline.md`(이 문서)

**Interfaces:**
- Produces:
  - 공개 저장소 `HyunwookYoo/chatproject`. 이력이 다시 써져서 모든 커밋 SHA가 바뀐다.
  - 비공개 백업 `HyunwookYoo/chatproject-private-backup`. 옛 이력, PR #1, 실행 기록이 남는다.
  - 저장소 설정
    - 외부 기여자의 워크플로는 항상 승인을 받아야 한다.
    - 비밀 검사와 push 보호가 켜진다.
    - `GITHUB_TOKEN`은 읽기 전용이다.
    - main ruleset: 삭제와 force-push를 막고, PR과 필수 검사 `rust`·`flutter`·`windows`·`android`를 요구한다. 관리자는 우회할 수 있다.
  - Environment `testflight`: 검토자는 사용자 본인이다. 배포는 `main`과 `testflight-*` 태그만 받는다.
  - 브랜치 `m1b-ios`와 main으로 가는 draft PR.

**정하지 않고 두는 것: 저장소 수준의 "액션 SHA 고정 필수" 정책은 켜지 않는다.**
- `subosito/flutter-action`은 composite 액션이다. 안에서 `actions/cache@v5`를 태그로 부른다.
- 그래서 이 정책을 켜면 Flutter를 쓰는 모든 작업이 "Set up job"에서 죽는다(flutter-action issue #404, upstream이 고치지 않기로 함).
- 대신 `tools/ci/check_workflows.py`(Task 2)가 우리 워크플로의 모든 `uses:`가 SHA로 고정됐는지 검사한다.

- [ ] **Step 1: 도구를 준비하고 새 복제본을 만든다**

```bash
SCRUB=/c/Users/$USERNAME/AppData/Local/Temp/chatproject-scrub
rm -rf "$SCRUB" && mkdir -p "$SCRUB"
python -m pip install --user git-filter-repo==2.47.0
cd /c/ChatProject && git status --short   # 비어 있어야 한다(이 계획 파일만 ?? 로 보이면 괜찮다)
git clone --no-local /c/ChatProject "$SCRUB/repo"
```

- [ ] **Step 2: 가릴 값 목록을 만든다(원문은 `$SCRUB`에만)**

`$SCRUB/known.txt`에 한 줄에 하나씩 적는다. 이 파일은 저장소에 넣지 않는다.
- 폰의 adb 시리얼: `git grep -n "폰(" -- docs/superpowers/plans/2026-10-04-m1a-core-skeleton.md`가 보여 주는 괄호 안 값.
- Firebase 프로젝트 ID: `spikes/c-push/RESULTS.md` 14행의 값.

Windows 계정 이름은 `known.txt`에 넣지 않는다. 다른 낱말의 일부일 수 있어서다(예: 계정 이름이 사용자 이름의 앞부분). 스캔 스크립트가 `Users/<이름>` 경로 꼴로 따로 찾는다.

`$SCRUB/scan_history.py`:

```python
#!/usr/bin/env python3
"""Lists what must not become public in every blob and commit message reachable from any ref
of the repository in the current directory. Prints masked values with sample paths; writes the
raw values to candidates.txt next to this script (never inside the repository)."""
import collections
import ipaddress
import pathlib
import re
import subprocess

HERE = pathlib.Path(__file__).parent
IPV4 = re.compile(rb"(?<![\w.])(?:\d{1,3}\.){3}\d{1,3}(?![\w.])")
IPV6 = re.compile(rb"(?<![\w:.])[0-9A-Fa-f]{0,4}(?::[0-9A-Fa-f]{0,4}){2,7}(?![\w:])")
ACCOUNT = re.compile(rb"Users(?:/|\\\\|\\)([A-Za-z0-9._-]+)")
TICKET = re.compile(rb"\b(?:endpoint|node)[a-z2-7]{50,}\b")


def git(*args, data=None):
    return subprocess.run(["git", *args], input=data, capture_output=True, check=True).stdout


def texts():
    """(where, bytes) for every text blob in history and every commit message."""
    listing = git("rev-list", "--objects", "--all").decode().splitlines()
    paths = {}
    for line in listing:
        sha, _, path = line.partition(" ")
        if path:
            paths.setdefault(sha, path)
    batch = git("cat-file", "--batch", data="\n".join(paths).encode() + b"\n")
    pos = 0
    while pos < len(batch):
        header_end = batch.index(b"\n", pos)
        sha, kind, size = batch[pos:header_end].split()
        body = batch[header_end + 1 : header_end + 1 + int(size)]
        pos = header_end + 1 + int(size) + 1
        if kind == b"blob" and b"\0" not in body[:8000]:
            yield paths[sha.decode()], body
    for message in git("log", "--all", "--format=%B%x00").split(b"\0"):
        yield "(commit message)", message


def mask(value):
    text = value.decode(errors="replace")
    return f"{text[:4]}*** ({len(text)} chars)"


def main():
    known = [line.strip().encode() for line in (HERE / "known.txt").read_text().splitlines() if line.strip()]
    found = collections.defaultdict(set)
    for where, data in texts():
        for m in IPV4.finditer(data):
            try:
                if ipaddress.ip_address(m.group().decode()).is_global:
                    found[("ipv4", m.group())].add(where)
            except ValueError:
                pass
        for m in IPV6.finditer(data):
            try:
                if ipaddress.ip_address(m.group().decode()).is_global:
                    found[("ipv6", m.group())].add(where)
            except ValueError:
                pass
        for m in ACCOUNT.finditer(data):
            found[("account-path", m.group())].add(where)
        for m in TICKET.finditer(data):
            found[("iroh-ticket", m.group()[:12])].add(where)
        for value in known:
            if value in data:
                found[("known", value)].add(where)
    raw = []
    for (kind, value), places in sorted(found.items()):
        sample = ", ".join(sorted(places)[:4])
        print(f"{kind:13} {mask(value):22} {len(places):3} places  {sample}")
        raw.append(f"{kind}\t{value.decode(errors='replace')}")
    (HERE / "candidates.txt").write_text("\n".join(raw) + "\n", encoding="utf-8")


if __name__ == "__main__":
    main()
```

Run: `cd "$SCRUB/repo" && python ../scan_history.py`
Expected:
- 다음이 2026-10-05 점검과 같은 곳에서 나온다.
  - `ipv4`: 집 공인 IP, 폰 LTE IP
  - `ipv6`: 폰 IPv6 2개
  - `account-path`: 계정 이름이 든 경로
  - `iroh-ticket`: `pc-listen.out`, `phone-listen.out`
  - `known` 3개
- `ipv4` 후보에는 4자리 버전 문자열이 섞여 나올 수 있다. 경로가 잠금 파일이나 vendored 코드라면 버전이다. 그런 후보는 가리지 않는다.

`$SCRUB/replacements.txt`를 만든다. 한 줄이 `원문==>자리표시`다. `candidates.txt`에서 실제 주소와 값만 옮긴다.
- 집 공인 IPv4 → `203.0.113.10`. 폰 IPv4 → `198.51.100.20`. 둘 다 RFC 5737 문서용 대역이다.
- IPv6 두 개 → `2001:db8::1`, `2001:db8::2`(RFC 3849).
- 폰 시리얼 → `PHONESERIAL`. 11자로 같은 길이다.
- 계정 경로의 모든 표기 → 같은 표기의 `Users/user`. 예: `C:/Users/<계정>` → `C:/Users/user`, `C:\Users\<계정>` → `C:\Users\user`, `C:\\Users\\<계정>` → `C:\\Users\\user`. 계정 이름만 따로 바꾸는 줄은 만들지 않는다.
- Firebase 프로젝트 ID → `fcm-probe-project`.

iroh 티켓은 `scrub_history.py`가 정규식으로 따로 지운다.

- [ ] **Step 3: 이력을 다시 쓴다**

`$SCRUB/scrub_history.py`:

```python
#!/usr/bin/env python3
"""Rewrites history in this fresh clone: applies replacements.txt (one `raw==>placeholder` per
line) to every text blob and commit message, and blanks iroh tickets. Run from the clone's root."""
import pathlib
import re

import git_filter_repo as fr

HERE = pathlib.Path(__file__).parent
PAIRS = []
for line in (HERE / "replacements.txt").read_bytes().splitlines():
    if line.strip() and not line.startswith(b"#"):
        raw, new = line.split(b"==>", 1)
        PAIRS.append((raw, new))
PAIRS.sort(key=lambda pair: len(pair[0]), reverse=True)  # longest first
TICKET = re.compile(rb"\b(endpoint|node)[a-z2-7]{50,}\b")


def scrub(data):
    for raw, new in PAIRS:
        data = data.replace(raw, new)
    return TICKET.sub(rb"\1REDACTED", data)


def blob_callback(blob, _metadata):
    if b"\0" not in blob.data[:8000]:
        blob.data = scrub(blob.data)


def message_callback(message):
    return scrub(message)


args = fr.FilteringOptions.parse_args([], error_on_empty=False)
fr.RepoFilter(args, blob_callback=blob_callback, message_callback=message_callback).run()
```

```bash
cd "$SCRUB/repo"
python ../scrub_history.py
cp .git/filter-repo/commit-map "$SCRUB/commit-map"
python ../scan_history.py
```

Expected:
- 두 번째 스캔에 `known`과 `iroh-ticket`이 없다.
- `account-path`는 자리표시 `Users/user`(10자)만 남는다.
- `ipv4`·`ipv6`는 Step 2에서 버전 문자열로 판정한 후보만 남는다.
- 자리표시 주소는 문서용 대역이라 `is_global`이 거짓이다. 그래서 목록에 나오지 않는다.

- [ ] **Step 4: 바뀐 범위와 시험을 확인한다**

```bash
cd /c/ChatProject
git fetch "$SCRUB/repo" main:refs/scrub/main
git diff --stat main refs/scrub/main
cd "$SCRUB/repo/tools/e2e" && dart pub get && dart test
```

Expected:
- `diff --stat`에는 Files 목록의 파일만 나온다.
- `dart test`는 `+14: All tests passed!`다. 시리얼 고정 값이 함께 바뀌었으므로 시험이 그대로 통과한다.
- 참고: `spikes/a-connectivity/.cargo/config.toml`의 경로가 `Users/user`가 되므로 스파이크 A는 이 PC에서 그대로는 빌드되지 않는다. 끝난 스파이크라 받아들인다.

- [ ] **Step 5: 옛 저장소 이름을 바꾸고 로컬 origin을 백업으로 돌린다 (사용자에게 확인 후)**

새 저장소가 옛 이름을 가져가면, 옛 이름을 가리키는 로컬 origin은 새 공개 저장소로 연결된다. 그 상태로 push하면 가리기 전 이력이 공개 저장소에 올라갈 수 있다. 그래서 이름을 바꾼 즉시 origin을 백업으로 돌린다.

```bash
gh repo rename chatproject-private-backup -R HyunwookYoo/chatproject --yes
git -C /c/ChatProject remote set-url origin git@github.com:HyunwookYoo/chatproject-private-backup.git
gh api repos/HyunwookYoo/chatproject-private-backup --jq '[.private,.full_name]'
```

Expected: `[true,"HyunwookYoo/chatproject-private-backup"]`

- [ ] **Step 6: 새 공개 저장소를 만들고 설정한다 (사용자에게 확인 후)**

```bash
gh repo create HyunwookYoo/chatproject --public --description "P2P chat app: Flutter UI + Rust core (iroh, OpenMLS)" --disable-issues --disable-wiki
# 새 저장소인지 확인한다(이름 바꾼 직후 옛 이름은 백업으로 넘어간다).
gh api repos/HyunwookYoo/chatproject --jq '[.private,.visibility,.size]'
```

Expected: `[false,"public",0]`. 다르면 멈춘다.

```bash
gh api -X PUT repos/HyunwookYoo/chatproject/actions/permissions/fork-pr-contributor-approval -f approval_policy=all_external_contributors
gh api -X PUT repos/HyunwookYoo/chatproject/actions/permissions/workflow -f default_workflow_permissions=read -F can_approve_pull_request_reviews=false
gh api -X PATCH repos/HyunwookYoo/chatproject --silent -f "security_and_analysis[secret_scanning][status]=enabled" -f "security_and_analysis[secret_scanning_push_protection][status]=enabled"
```

Git Bash에서는 `gh api`의 경로를 `/`로 시작하지 않는다. `/repos/...`라고 쓰면 Git Bash가 Windows 경로로 바꿔 버린다.

- [ ] **Step 7: 다시 쓴 이력을 push한다 (사용자에게 확인 후)**

```bash
cd "$SCRUB/repo"
git remote add origin git@github.com:HyunwookYoo/chatproject.git
git push -u origin main
```

push하면 `ci.yml`의 `push: branches: [main]`로 CI가 돈다. 공개 저장소라 무료다.

```bash
RUN_ID=$(gh run list -R HyunwookYoo/chatproject --limit 1 --json databaseId --jq '.[0].databaseId')
gh run watch "$RUN_ID" -R HyunwookYoo/chatproject --exit-status
```

Expected: `rust`, `flutter`, `windows`, `android` 네 작업이 녹색이다.
- 공개 러너의 Linux는 RAM이 16 GB다. 그래서 M1a 마지막 main 실행에서 메모리 부족으로 보였던 `android` 강제 종료가 다시 나지 않아야 한다.
- 다시 나면 실패 로그를 보고서에 적는다.

- [ ] **Step 8: 로컬 저장소를 새 이력으로 옮긴다**

```bash
cd /c/ChatProject
git status --short          # 이 계획 파일(??) 말고는 비어 있어야 한다
git remote set-url origin git@github.com:HyunwookYoo/chatproject.git
git fetch origin
git reset --hard origin/main
git update-ref -d refs/scrub/main
git log --oneline -1
```

Expected:
- 마지막 커밋 메시지가 `docs: list where M1a deviated from its plan`이고 SHA는 새 값이다.
- 추적하지 않는 파일과 무시되는 파일(`spikes/c-push/secrets/` 등)은 그대로 남는다.

- [ ] **Step 9: main ruleset과 `testflight` Environment를 만든다**

`$SCRUB/ruleset.json`:

```json
{
  "name": "default-branch-protection",
  "target": "branch",
  "enforcement": "active",
  "bypass_actors": [
    { "actor_id": 5, "actor_type": "RepositoryRole", "bypass_mode": "always" }
  ],
  "conditions": { "ref_name": { "include": ["~DEFAULT_BRANCH"], "exclude": [] } },
  "rules": [
    { "type": "deletion" },
    { "type": "non_fast_forward" },
    { "type": "pull_request", "parameters": {
        "required_approving_review_count": 0,
        "dismiss_stale_reviews_on_push": false,
        "require_code_owner_review": false,
        "require_last_push_approval": false,
        "required_review_thread_resolution": false } },
    { "type": "required_status_checks", "parameters": {
        "strict_required_status_checks_policy": false,
        "do_not_enforce_on_create": false,
        "required_status_checks": [
          { "context": "rust", "integration_id": 15368 },
          { "context": "flutter", "integration_id": 15368 },
          { "context": "windows", "integration_id": 15368 },
          { "context": "android", "integration_id": 15368 } ] } }
  ]
}
```

- `integration_id` 15368은 GitHub Actions 앱이다.
- `actor_id` 5 + `RepositoryRole`은 저장소 관리자다. GitHub 자체 스크립트가 같은 값을 쓴다.

`$SCRUB/env.json`. 43915071은 `HyunwookYoo`의 사용자 ID다.

```json
{
  "wait_timer": 0,
  "prevent_self_review": false,
  "reviewers": [ { "type": "User", "id": 43915071 } ],
  "deployment_branch_policy": { "protected_branches": false, "custom_branch_policies": true }
}
```

```bash
cd "$SCRUB"
gh api -X POST repos/HyunwookYoo/chatproject/rulesets --input ruleset.json --silent
gh api repos/HyunwookYoo/chatproject/rulesets --jq '.[]|[.id,.name,.enforcement]'
RULESET_ID=$(gh api repos/HyunwookYoo/chatproject/rulesets --jq '.[] | select(.name=="default-branch-protection") | .id')
gh api repos/HyunwookYoo/chatproject/rulesets/$RULESET_ID --jq '.current_user_can_bypass'

gh api -X PUT repos/HyunwookYoo/chatproject/environments/testflight --input env.json --silent
gh api -X POST repos/HyunwookYoo/chatproject/environments/testflight/deployment-branch-policies -f name=main -f type=branch --silent
gh api -X POST repos/HyunwookYoo/chatproject/environments/testflight/deployment-branch-policies -f 'name=testflight-*' -f type=tag --silent
gh api repos/HyunwookYoo/chatproject/environments/testflight/deployment-branch-policies --jq '.branch_policies[]|[.type,.name]'
```

Expected:
- ruleset 줄은 `[<id>,"default-branch-protection","active"]`다.
- `current_user_can_bypass`는 `"always"`다.
- 배포 정책은 `["branch","main"]`과 `["tag","testflight-*"]` 두 줄이다.

설정을 모두 확인한다:

```bash
gh api repos/HyunwookYoo/chatproject --jq '{visibility,ss:.security_and_analysis.secret_scanning.status,pp:.security_and_analysis.secret_scanning_push_protection.status}'
gh api repos/HyunwookYoo/chatproject/actions/permissions/fork-pr-contributor-approval --jq .approval_policy
gh api repos/HyunwookYoo/chatproject/actions/permissions/workflow --jq '[.default_workflow_permissions,.can_approve_pull_request_reviews]'
```

Expected:
- 첫 줄: `{"pp":"enabled","ss":"enabled","visibility":"public"}`
- 둘째 줄: `all_external_contributors`
- 셋째 줄: `["read",false]`

- [ ] **Step 10: `m1b-ios` 브랜치를 만들고, 문서의 커밋 SHA를 새 값으로 고친다**

M1a 계획의 "계획과 달라진 점"이 옛 SHA를 인용한다. 커밋 메시지 안의 SHA는 filter-repo가 이미 고쳤다. 파일 내용 안의 SHA는 고치지 않았으므로 여기서 고친다.

```bash
cd /c/ChatProject
git switch -c m1b-ios
python - "$SCRUB/commit-map" <<'PY'
import pathlib, sys
pairs = [line.split() for line in open(sys.argv[1], encoding="utf-8").read().splitlines()[1:]]
doc = pathlib.Path("docs/superpowers/plans/2026-10-04-m1a-core-skeleton.md")
text = doc.read_text(encoding="utf-8")
for old, new in pairs:
    text = text.replace(old[:7], new[:7])
doc.write_text(text, encoding="utf-8")
PY
git diff --stat
```

Expected: `2026-10-04-m1a-core-skeleton.md`만 바뀐다. 바뀐 줄은 "계획과 달라진 점"의 SHA다.

```bash
git add docs/superpowers/plans/2026-10-04-m1a-core-skeleton.md
git commit -m "docs: point the M1a plan at the rewritten commit ids"
git add docs/superpowers/plans/2026-10-05-m1b-ios-pipeline.md
git commit -m "docs: M1b implementation plan"
git push -u origin m1b-ios
printf 'Plan: docs/superpowers/plans/2026-10-05-m1b-ios-pipeline.md\n' > "$SCRUB/pr-body.md"
# 실행 세션이 정한 PR attribution 줄을 pr-body.md 끝에 더한 뒤:
gh pr create --draft --base main --head m1b-ios --title "M1b: iOS pipeline and device measurements" --body-file "$SCRUB/pr-body.md"
```

이후 push마다 PR에서 CI가 돈다.

컨트롤러는 옛 SHA와 새 SHA의 대응을 agentmemory에 `[ChatProject]` 메모로 남긴다. 최소한 M1a의 첫 커밋과 마지막 커밋은 적는다.


### Task 2: CI 정비 — SHA 고정, 권한, 동시 실행, 캐시, cargokit 툴체인

**Files:**
- Modify: `.github/workflows/ci.yml`
- Modify: `app/rust_builder/cargokit/build_tool/lib/src/builder.dart`
- Modify: `.gitignore`
- Create: `.github/dependabot.yml`, `tools/ci/check_workflows.py`

**Interfaces:**
- Consumes: M1a의 `ci.yml`(작업 `rust`, `flutter`, `windows`, `android`).
- Produces:
  - 환경 변수 `CARGOKIT_TOOLCHAIN`이 있으면 cargokit이 그 툴체인으로 빌드한다. `ci.yml`과 `testflight.yml`은 워크플로 `env`에 `CARGOKIT_TOOLCHAIN: 1.92.0`을 둔다.
  - `python tools/ci/check_workflows.py`는 모든 워크플로를 검사한다. 문제가 있으면 줄마다 하나씩 출력하고 종료 코드 1로 끝난다. Task 8이 검사를 더한다.
  - CI 작업 `workflows`가 이 검사를 돈다.

- [ ] **Step 1: 워크플로 검사 스크립트를 쓰고, 지금의 `ci.yml`에서 실패하는지 본다**

`tools/ci/check_workflows.py`:

```python
#!/usr/bin/env python3
"""Static checks for .github/workflows/*.yml (public repo rules, M1b plan Global Constraints).

Prints one line per problem and exits 1 if there is any.
"""
import pathlib
import re
import sys

import yaml

ROOT = pathlib.Path(__file__).resolve().parents[2]
PINNED = re.compile(r"^[\w.-]+/[\w.-]+(/[\w./-]+)?@[0-9a-f]{40}$")


def triggers(doc):
    # PyYAML (YAML 1.1) reads the bare key `on` as True.
    on = doc.get("on", doc.get(True))
    if isinstance(on, str):
        return {on: None}
    if isinstance(on, list):
        return {name: None for name in on}
    return on or {}


def steps(doc):
    for job_id, job in (doc.get("jobs") or {}).items():
        for index, step in enumerate(job.get("steps") or []):
            yield job_id, index, job, step


def check(path, doc):
    name = path.name
    problems = []
    if "pull_request_target" in triggers(doc) or "workflow_run" in triggers(doc):
        problems.append(f"{name}: pull_request_target/workflow_run is not allowed in a public repo")
    if doc.get("permissions") != {"contents": "read"}:
        problems.append(f"{name}: top-level permissions must be exactly 'contents: read'")
    env = doc.get("env") or {}
    if "CARGOKIT_TOOLCHAIN" in env and str(env["CARGOKIT_TOOLCHAIN"]) != str(env.get("RUST_TOOLCHAIN")):
        problems.append(f"{name}: CARGOKIT_TOOLCHAIN must equal RUST_TOOLCHAIN")
    for job_id, index, job, step in steps(doc):
        uses = step.get("uses")
        if not uses:
            continue
        if not PINNED.match(uses):
            problems.append(f"{name}: {job_id} step {index}: '{uses}' is not pinned to a full commit SHA")
        if uses.startswith("actions/checkout@") and (step.get("with") or {}).get("persist-credentials") is not False:
            problems.append(f"{name}: {job_id} step {index}: checkout must set persist-credentials: false")
    return problems


def main():
    problems = []
    for path in sorted((ROOT / ".github" / "workflows").glob("*.yml")):
        problems += check(path, yaml.safe_load(path.read_text(encoding="utf-8")))
    if problems:
        print("\n".join(problems))
        return 1
    print("workflows ok")
    return 0


if __name__ == "__main__":
    sys.exit(main())
```

Run: `cd /c/ChatProject && python tools/ci/check_workflows.py; echo "exit=$?"`
Expected:
- `ci.yml: top-level permissions must be exactly 'contents: read'`
- `'actions/checkout@v4' is not pinned ...` 같은 줄이 여러 개 나온다.
- 마지막은 `exit=1`이다.

PyYAML이 없으면 `python -m pip install --user pyyaml==6.0.3`.

- [ ] **Step 2: cargokit이 `CARGOKIT_TOOLCHAIN`을 읽게 고친다**

vendored cargokit은 `rust-toolchain.toml`을 무시하고 늘 `rustup run stable cargo build`를 부른다(`builder.dart`의 `_toolchain`). `app/rust_builder/cargokit/build_tool/lib/src/builder.dart`의 import 줄 맨 위에 다음을 더한다.

```dart
import 'dart:io' show Platform;

```

`_toolchain` getter를 바꾼다:

```dart
  /// ChatProject: CARGOKIT_TOOLCHAIN pins the toolchain to rust-toolchain.toml's channel.
  /// Upstream cargokit always builds with `stable` unless cargokit.yaml says otherwise.
  String get _toolchain =>
      Platform.environment['CARGOKIT_TOOLCHAIN'] ??
      _buildOptions?.toolchain.name ??
      'stable';
```

이 변경도 R8과 같은 vendored 패치다. `flutter_rust_bridge_codegen integrate`를 다시 돌리거나 flutter_rust_bridge를 올리면 덮어써진다. 그러면 Step 6의 CI 검사가 실패해서 알 수 있다.

- [ ] **Step 3: `ci.yml`을 고친다**

`.github/workflows/ci.yml` 전체:

```yaml
name: ci

on:
  push:
    branches: [main]
  pull_request:

permissions:
  contents: read

# A new push to a PR cancels that PR's older run; pushes to main always finish.
concurrency:
  group: ci-${{ github.ref }}
  cancel-in-progress: ${{ github.event_name == 'pull_request' }}

env:
  FLUTTER_VERSION: 3.44.6
  RUST_TOOLCHAIN: 1.92.0
  # cargokit (app/rust_builder) builds with this toolchain. Must equal RUST_TOOLCHAIN.
  CARGOKIT_TOOLCHAIN: 1.92.0

jobs:
  workflows:
    runs-on: ubuntu-latest
    timeout-minutes: 10
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      # Ubuntu's system Python refuses pip installs (PEP 668), so use a venv.
      - name: Python with PyYAML
        run: |
          python3 -m venv "$RUNNER_TEMP/venv"
          "$RUNNER_TEMP/venv/bin/pip" install --quiet pyyaml==6.0.3
          echo "$RUNNER_TEMP/venv/bin" >> "$GITHUB_PATH"
      - run: python tools/ci/check_workflows.py

  rust:
    runs-on: ubuntu-latest
    timeout-minutes: 20
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - uses: dtolnay/rust-toolchain@7e38f4b43b4db5c8dd498af069a4f6196df1d067 # master 2026-10-01
        with:
          toolchain: ${{ env.RUST_TOOLCHAIN }}
          components: clippy
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
      - run: cargo test --workspace
      - run: cargo clippy --workspace --all-targets -- -D warnings

  flutter:
    runs-on: ubuntu-latest
    timeout-minutes: 15
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - uses: subosito/flutter-action@1a449444c387b1966244ae4d4f8c696479add0b2 # v2.23.0
        with:
          flutter-version: ${{ env.FLUTTER_VERSION }}
          channel: stable
      - name: App analysis and widget tests
        working-directory: app
        run: |
          flutter pub get
          flutter analyze
          flutter test
      - name: Device tool tests
        working-directory: tools/e2e
        run: |
          dart pub get
          dart analyze
          dart test

  windows:
    runs-on: windows-latest
    timeout-minutes: 45
    defaults:
      run:
        working-directory: app
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - uses: dtolnay/rust-toolchain@7e38f4b43b4db5c8dd498af069a4f6196df1d067 # master 2026-10-01
        with:
          toolchain: ${{ env.RUST_TOOLCHAIN }}
      - uses: subosito/flutter-action@1a449444c387b1966244ae4d4f8c696479add0b2 # v2.23.0
        with:
          flutter-version: ${{ env.FLUTTER_VERSION }}
          channel: stable
      - run: flutter pub get
      - run: flutter test integration_test/core_test.dart -d windows
      - run: flutter build windows --release

  android:
    runs-on: ubuntu-latest
    timeout-minutes: 60
    steps:
      - name: Free disk space
        run: |
          # The emulator image and AVD need room. Never touch /usr/local/lib/android
          # (the SDK), Java, or anything the later steps install.
          df -h /
          sudo rm -rf \
            /usr/share/dotnet \
            /usr/local/.ghcup \
            /opt/ghc \
            /usr/share/swift \
            /usr/local/share/boost \
            /opt/hostedtoolcache/CodeQL \
            /usr/local/share/powershell \
            /usr/local/share/chromium
          sudo docker image prune --all --force
          df -h /
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Enable KVM
        run: |
          echo 'KERNEL=="kvm", GROUP="kvm", MODE="0666", OPTIONS+="static_node=kvm"' | sudo tee /etc/udev/rules.d/99-kvm4all.rules
          sudo udevadm control --reload-rules
          sudo udevadm trigger --name-match=kvm
      - uses: actions/setup-java@de7274f081f381c8f8158605e0321c36c376e2e6 # v6.0.1
        with:
          distribution: temurin
          java-version: '21'
      - uses: dtolnay/rust-toolchain@7e38f4b43b4db5c8dd498af069a4f6196df1d067 # master 2026-10-01
        with:
          toolchain: ${{ env.RUST_TOOLCHAIN }}
          targets: aarch64-linux-android,armv7-linux-androideabi,x86_64-linux-android,i686-linux-android
      - uses: subosito/flutter-action@1a449444c387b1966244ae4d4f8c696479add0b2 # v2.23.0
        with:
          flutter-version: ${{ env.FLUTTER_VERSION }}
          channel: stable
      - name: Release APK
        working-directory: app
        env:
          CARGOKIT_VERBOSE: '1'
        run: |
          # The default step shell is `bash -e` without pipefail; tee would hide a failed build.
          set -o pipefail
          flutter pub get
          flutter build apk --release 2>&1 | tee "$RUNNER_TEMP/apk.log"
          # The vendored cargokit patch (CARGOKIT_TOOLCHAIN) must still be in effect.
          grep -q "Running command rustup run ${CARGOKIT_TOOLCHAIN} cargo build" "$RUNNER_TEMP/apk.log"
      - name: AVD cache
        uses: actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0
        id: avd-cache
        with:
          path: |
            ~/.android/avd/*
            ~/.android/adb*
          key: avd-api36-google_apis-x86_64
      - name: Create AVD snapshot for the cache
        if: steps.avd-cache.outputs.cache-hit != 'true'
        uses: reactivecircus/android-emulator-runner@a421e43855164a8197daf9d8d40fe71c6996bb0d # v2.38.0
        with:
          api-level: 36
          target: google_apis
          arch: x86_64
          force-avd-creation: false
          emulator-options: -no-window -gpu swiftshader_indirect -noaudio -no-boot-anim -camera-back none
          disable-animations: false
          script: echo "AVD snapshot created"
      - name: Integration tests on the emulator
        uses: reactivecircus/android-emulator-runner@a421e43855164a8197daf9d8d40fe71c6996bb0d # v2.38.0
        with:
          api-level: 36
          target: google_apis
          arch: x86_64
          force-avd-creation: false
          emulator-options: -no-snapshot-save -no-window -gpu swiftshader_indirect -noaudio -no-boot-anim -camera-back none
          disable-animations: true
          working-directory: app
          script: flutter test integration_test -d emulator-5554
```

`.github/dependabot.yml`:

```yaml
version: 2
updates:
  - package-ecosystem: github-actions
    directory: /
    schedule:
      interval: weekly
```

`.gitignore` 끝의 비밀 목록 아래에 두 줄을 더한다:

```gitignore
*.mobileprovision
*.cer
```

- [ ] **Step 4: 검사가 통과하는지 본다**

Run: `cd /c/ChatProject && python tools/ci/check_workflows.py; echo "exit=$?"`
Expected: `workflows ok`, `exit=0`

- [ ] **Step 5: 로컬에서 cargokit이 1.92.0으로 빌드하는지 본다**

```bash
cd /c/ChatProject/app
CARGOKIT_TOOLCHAIN=1.92.0 CARGOKIT_VERBOSE=1 flutter build apk --debug --target-platform android-x64 2>&1 | grep "Running command rustup run"
```

Expected: `Running command rustup run 1.92.0 cargo build ... --target x86_64-linux-android ...` 한 줄 이상.

- [ ] **Step 6: 커밋하고 push해서 CI를 확인한다**

```bash
cd /c/ChatProject
git add .github/workflows/ci.yml .github/dependabot.yml .gitignore tools/ci/check_workflows.py app/rust_builder/cargokit/build_tool/lib/src/builder.dart
git commit -m "ci: pin actions by SHA, read-only token, cancel stale PR runs, build Rust with the pinned toolchain"
git push
```

CI 확인 (Task 1이 연 draft PR에서):
- `workflows`, `rust`, `flutter`, `windows`, `android`가 모두 녹색이다.
- `android`의 Release APK 단계가 grep 검사를 통과한다. 통과는 cargokit이 1.92.0으로 빌드했다는 뜻이다.
- 로그에 Node 20 폐기 경고(`Node.js 20 is deprecated`)가 없다.

### Task 3: iOS 기본값과 시뮬레이터 CI 작업

**Files:**
- Modify: `app/ios/Runner.xcodeproj/project.pbxproj` (9줄)
- Modify: `app/rust_builder/ios/chat_ffi.podspec:23`
- Modify: `app/rust/src/native.rs:1-2`, `app/rust/src/global.rs:1-2`
- Modify: `.github/workflows/ci.yml` (작업 `ios` 추가, `workflows` 작업에 unittest 추가)
- Create: `tools/ci/pick_simulator.py`, `tools/ci/test_pick_simulator.py`
- Modify: `docs/superpowers/specs/2026-10-05-test-automation-design.md` (2절 ③ 줄, 6절, 8절)

**Interfaces:**
- Consumes: Task 2의 `ci.yml`과 `check_workflows.py`.
- Produces:
  - CI 작업 `ios`(`macos-26`, Xcode 26.6). Task 6이 이 작업에 NSE 단계를 더한다.
  - `python3 tools/ci/pick_simulator.py <simctl-json>`은 쓸 iPhone 시뮬레이터의 UDID 한 줄을 stdout에 낸다.

- [ ] **Step 1: 시뮬레이터 고르기 시험을 쓴다**

러너 이미지가 바뀌면 기기 이름과 런타임도 바뀐다. 그래서 이름을 박아 두지 않고, 그 자리에서 가장 새 iOS 런타임의 iPhone을 고른다.

`tools/ci/test_pick_simulator.py`:

```python
import unittest

from pick_simulator import pick

RUNTIME = "com.apple.CoreSimulator.SimRuntime.iOS-{}"


def device(name, udid, available=True):
    return {"name": name, "udid": udid, "isAvailable": available}


class PickTest(unittest.TestCase):
    def test_newest_runtime_wins(self):
        listing = {"devices": {
            RUNTIME.format("26-2"): [device("iPhone 17", "old")],
            RUNTIME.format("26-5"): [device("iPhone Air", "new")],
        }}
        self.assertEqual(pick(listing)[0], "new")

    def test_prefers_iphone_17_within_a_runtime(self):
        listing = {"devices": {RUNTIME.format("26-5"): [
            device("iPhone 17 Pro", "pro"), device("iPhone 17", "plain"),
        ]}}
        self.assertEqual(pick(listing)[0], "plain")

    def test_skips_unavailable_and_non_iphone_devices(self):
        listing = {"devices": {
            RUNTIME.format("26-5"): [device("iPhone 17", "gone", available=False), device("iPad Air", "ipad")],
            RUNTIME.format("26-4"): [device("iPhone 17e", "ok")],
            "com.apple.CoreSimulator.SimRuntime.watchOS-26-5": [device("Apple Watch", "watch")],
        }}
        self.assertEqual(pick(listing)[0], "ok")

    def test_no_iphone_is_an_error(self):
        with self.assertRaises(SystemExit):
            pick({"devices": {}})


if __name__ == "__main__":
    unittest.main()
```

Run: `cd /c/ChatProject/tools/ci && python -m unittest test_pick_simulator -v`
Expected: `ModuleNotFoundError: No module named 'pick_simulator'`

- [ ] **Step 2: 고르기 스크립트를 쓴다**

`tools/ci/pick_simulator.py`:

```python
#!/usr/bin/env python3
"""Prints the UDID of the iPhone simulator to test on: the newest iOS runtime,
and "iPhone 17" when that runtime has one.

Usage: xcrun simctl list devices available iOS --json > sim.json
       python3 pick_simulator.py sim.json
"""
import json
import sys

PREFIX = "com.apple.CoreSimulator.SimRuntime.iOS-"


def pick(listing):
    """Returns (udid, name, runtime). Exits with a message when there is no iPhone."""
    best = None
    for runtime, devices in (listing.get("devices") or {}).items():
        if not runtime.startswith(PREFIX):
            continue
        version = tuple(int(part) for part in runtime[len(PREFIX):].split("-"))
        for device in devices:
            if not device.get("isAvailable", True) or not device.get("name", "").startswith("iPhone"):
                continue
            key = (version, device["name"] == "iPhone 17")
            if best is None or key > best[0]:
                best = (key, device["udid"], device["name"], runtime)
    if best is None:
        sys.exit("no available iPhone simulator")
    return best[1], best[2], best[3]


if __name__ == "__main__":
    with open(sys.argv[1], encoding="utf-8") as f:
        udid, name, runtime = pick(json.load(f))
    print(f"picked {name} on {runtime}", file=sys.stderr)
    print(udid)
```

Run: `cd /c/ChatProject/tools/ci && python -m unittest test_pick_simulator -v`
Expected: `Ran 4 tests ... OK`

- [ ] **Step 3: 번들 ID와 최소 버전을 바꾼다**

`app/ios/Runner.xcodeproj/project.pbxproj`. 이 9줄만 바꾼다. 다른 줄은 건드리지 않는다.

- 385, 564, 586행:
  - 이전: `PRODUCT_BUNDLE_IDENTIFIER = dev.chatproject.chatApp;`
  - 이후: `PRODUCT_BUNDLE_IDENTIFIER = dev.chatproject.chatapp;`
- 401, 418, 433행:
  - 이전: `PRODUCT_BUNDLE_IDENTIFIER = dev.chatproject.chatApp.RunnerTests;`
  - 이후: `PRODUCT_BUNDLE_IDENTIFIER = dev.chatproject.chatapp.RunnerTests;`
- 363, 489, 540행:
  - 이전: `IPHONEOS_DEPLOYMENT_TARGET = 13.0;`
  - 이후: `IPHONEOS_DEPLOYMENT_TARGET = 15.0;`

확인:

```bash
cd /c/ChatProject
grep -c "dev.chatproject.chatApp" app/ios/Runner.xcodeproj/project.pbxproj   # 0
grep -c "IPHONEOS_DEPLOYMENT_TARGET = 15.0;" app/ios/Runner.xcodeproj/project.pbxproj   # 3
```

`app/rust_builder/ios/chat_ffi.podspec:23`:
- 이전: `  s.platform = :ios, '11.0'`
- 이후: `  s.platform = :ios, '15.0'`

- [ ] **Step 4: `native.rs`와 `global.rs`의 첫 주석을 고친다**

iOS 알림 확장은 다른 프로세스에서 `core/chat_nse`를 쓴다(로드맵 §2). 그래서 이 두 파일은 iOS NSE의 진입점이 아니다.

`app/rust/src/native.rs:1-2`:

```rust
//! Entry points for native code that runs without Flutter in this process: the Android
//! push receiver (Kotlin, design 12.3). The iOS notification extension runs in its own
//! process and links `core/chat_nse` instead.
```

`app/rust/src/global.rs:1-2`:

```rust
//! Process-wide core state. Every entry point in this process (Dart through `api`, Kotlin
//! through `native`) goes through these two statics.
```

Run: `cd /c/ChatProject && cargo test -p chat_ffi && cargo clippy -p chat_ffi --all-targets -- -D warnings`
Expected: `3 passed`, 경고 없음.

- [ ] **Step 5: CI에 `ios` 작업을 더한다**

`.github/workflows/ci.yml`의 `workflows` 작업 마지막에 다음 단계를 더한다:

```yaml
      - run: python -m unittest discover -s tools/ci -p 'test_*.py' -v
```

`android` 작업 아래에 다음 작업을 더한다:

```yaml
  ios:
    runs-on: macos-26
    timeout-minutes: 45
    env:
      # One Rust slice (arm64) for the simulator instead of arm64 + x86_64.
      FLUTTER_XCODE_ARCHS: arm64
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - name: Pin Xcode
        run: |
          sudo xcode-select -s /Applications/Xcode_26.6.app/Contents/Developer
          xcodebuild -version
      - uses: dtolnay/rust-toolchain@7e38f4b43b4db5c8dd498af069a4f6196df1d067 # master 2026-10-01
        with:
          toolchain: ${{ env.RUST_TOOLCHAIN }}
          targets: aarch64-apple-ios-sim
      - uses: subosito/flutter-action@1a449444c387b1966244ae4d4f8c696479add0b2 # v2.23.0
        with:
          flutter-version: ${{ env.FLUTTER_VERSION }}
          channel: stable
      - name: Flutter packages
        working-directory: app
        run: |
          flutter precache --ios
          flutter pub get
      - name: Pick and boot an iPhone simulator
        id: sim
        timeout-minutes: 10
        run: |
          xcrun simctl list devices available iOS --json > "$RUNNER_TEMP/sim.json"
          udid="$(python3 tools/ci/pick_simulator.py "$RUNNER_TEMP/sim.json")"
          echo "udid=$udid" >> "$GITHUB_OUTPUT"
          xcrun simctl bootstatus "$udid" -b
      - name: Integration tests on the simulator
        working-directory: app
        run: flutter test integration_test/core_test.dart -d "${{ steps.sim.outputs.udid }}"
```

참고:
- iOS 빌드는 `app/ios/Podfile`, `Podfile.lock`, `Flutter/*.xcconfig`, `project.pbxproj`를 러너에서 고친다. 그래서 이 작업에는 `git diff --exit-code`를 두지 않는다.
- 시뮬레이터는 앱 서명이 필요 없다(자동 테스트 설계 6절).

- [ ] **Step 6: 자동 테스트 설계를 고친다**

`docs/superpowers/specs/2026-10-05-test-automation-design.md`:

2절 표의 ③ 줄 "언제" 칸:
- 이전: `매 커밋. iOS는 main 브랜치와 매일 밤`
- 이후: `매 커밋 (iOS 포함. 저장소가 공개라 macOS 러너가 무료다)`

8절 표:
- 이전 줄 `| main 브랜치 push, 매일 밤 | iOS 시뮬레이터 통합 테스트(M1b부터) |` → 지운다.
- `| 매 push |` 줄의 작업 칸 끝에 `, iOS 시뮬레이터 통합 테스트(M1b부터)`를 더한다.

8절 표 아래 문단을 다음으로 바꾼다:

```markdown
저장소는 2026-10-05부터 공개다. 공개 저장소에서는 macOS를 포함한 기본 러너가 무료라서, iOS도 매 push마다 돈다. 여러 기기 시나리오는 시간이 오래 걸려서 매일 밤에만 돌린다.
```

- [ ] **Step 7: 확인하고 커밋한다**

Run: `cd /c/ChatProject && python tools/ci/check_workflows.py && (cd tools/ci && python -m unittest test_pick_simulator)`
Expected: `workflows ok`, `OK`

```bash
cd /c/ChatProject
git add app/ios/Runner.xcodeproj/project.pbxproj app/rust_builder/ios/chat_ffi.podspec app/rust/src/native.rs app/rust/src/global.rs .github/workflows/ci.yml tools/ci/pick_simulator.py tools/ci/test_pick_simulator.py docs/superpowers/specs/2026-10-05-test-automation-design.md
git commit -m "ci: iOS simulator integration tests; iOS bundle id dev.chatproject.chatapp, iOS 15"
git push
```

CI 확인:
- `ios` 작업이 녹색이다.
- 로그에 `picked iPhone ... on com.apple.CoreSimulator.SimRuntime.iOS-26-...` 줄이 있다.
- 로그에 `🎉 3 tests passed.`가 있다. GitHub Actions에서는 테스트 결과를 GitHub 형식으로 찍는다. 로컬에서는 같은 결과가 `+3: All tests passed!`로 나온다.
- 나머지 작업도 녹색이다.

### Task 4: `chat_nse` 크레이트와 측정용 그룹 상태

**Files:**
- Create: `core/chat_nse/Cargo.toml`, `core/chat_nse/src/lib.rs`, `core/chat_nse/src/state.rs`
- Create: `tools/nse_fixture/Cargo.toml`, `tools/nse_fixture/src/lib.rs`, `tools/nse_fixture/src/main.rs`, `tools/nse_fixture/tests/decrypt.rs`
- Create (생성됨): `app/ios/NotificationService/Fixture/nse_state.bin`, `app/ios/NotificationService/Fixture/ciphertexts.txt`
- Modify: `Cargo.toml` (members), `.gitattributes`

**Interfaces:**
- Consumes: 스파이크 B의 rev3 그룹 구성(`spikes/b-mls-memory/RESULTS.md`의 Method).
- Produces:
  - `chat_nse::probe_decrypt(state_path: String, ciphertext: Vec<u8>) -> Result<ProbeOutcome, NseError>`. UniFFI export다. Swift에서는 `probeDecrypt(statePath:ciphertext:) throws -> ProbeOutcome`가 된다.
  - `chat_nse::ProbeOutcome { plaintext_len: u32, preview: String, load_micros: u64, decrypt_micros: u64 }`
  - `chat_nse::NseError::{BadState(String), GroupNotFound, BadCiphertext(String), Decrypt(String), NotApplication}`. `flat_error`라서 Swift에서는 메시지 문자열로 보인다.
  - `chat_nse::state::{Map, encode(group_id: &[u8], map: &Map) -> Vec<u8>, read(&mut impl Read) -> io::Result<(Vec<u8>, Map)>}`
  - `nse_fixture::{Fixture { state: Vec<u8>, ciphertexts: Vec<Vec<u8>> }, build(n: usize, k: usize, credential_bytes: usize) -> Fixture}`
  - 측정 고정 파일 두 개:
    - `Fixture/nse_state.bin`: N = 200, 자격 증명 200 B, 약 1.35 MB.
    - `Fixture/ciphertexts.txt`: 8줄, 한 줄이 base64 암호문 하나. i번째 줄이 평문 `M1b probe message {i-1} ...`(1 KiB)의 암호문이다.

NSE는 복호하면서 바뀐 상태를 메모리에서만 쓰고 버린다(설계 12.3). 그래서 같은 상태 파일로 어느 암호문이든, 몇 번이든 다시 복호할 수 있다.

- [ ] **Step 1: 상태 파일 형식 시험을 쓴다**

`core/chat_nse/Cargo.toml`:

```toml
[package]
name = "chat_nse"
version = "0.1.0"
edition = "2024"
publish = false

[lib]
# staticlib: linked into the iOS notification extension. lib: used by tools/nse_fixture.
# No cdylib: `-lchat_nse` would pick a .dylib that sits next to the .a.
crate-type = ["staticlib", "lib"]

[dependencies]
openmls = "=0.9.0"
openmls_rust_crypto = "=0.6.0"
thiserror = "2"
uniffi = "=0.32.2"

[dev-dependencies]
tempfile = "3"
```

루트 `Cargo.toml`의 `members`를 바꾼다. `tools/nse_fixture`는 Step 5에서 더한다. 없는 폴더를 미리 넣으면 모든 cargo 명령이 실패한다.

```toml
members = ["core/chat_core", "core/chat_nse", "app/rust", "tools/uniffi-bindgen"]
```

`core/chat_nse/src/state.rs`에는 우선 시험만 둔다(맨 위 `use` 두 줄과 함께):

```rust
use std::collections::HashMap;
use std::io::{self, Read};

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Map {
        let mut map = Map::new();
        map.insert(b"k2".to_vec(), b"value two".to_vec());
        map.insert(b"k1".to_vec(), Vec::new());
        map
    }

    #[test]
    fn round_trip_keeps_group_id_and_map() {
        let bytes = encode(b"group", &sample());
        let (group_id, map) = read(&mut &bytes[..]).unwrap();
        assert_eq!(group_id, b"group");
        assert_eq!(map, sample());
    }

    #[test]
    fn encoding_is_deterministic() {
        assert_eq!(encode(b"g", &sample()), encode(b"g", &sample()));
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut bytes = encode(b"g", &sample());
        bytes[0] = b'X';
        assert_eq!(read(&mut &bytes[..]).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn truncated_file_is_rejected() {
        let bytes = encode(b"g", &sample());
        let cut = &bytes[..bytes.len() - 1];
        assert_eq!(read(&mut &cut[..]).unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn oversized_length_is_rejected_without_allocating() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&u32::MAX.to_be_bytes()); // group id length: 4 GiB
        assert_eq!(read(&mut &bytes[..]).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn oversized_entry_count_is_rejected_without_allocating() {
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.push(b'g');
        bytes.extend_from_slice(&u32::MAX.to_be_bytes()); // entry count
        assert_eq!(read(&mut &bytes[..]).unwrap_err().kind(), io::ErrorKind::InvalidData);
    }
}
```

`core/chat_nse/src/lib.rs`에는 우선 `pub mod state;` 한 줄만 둔다.

Run: `cd /c/ChatProject && cargo test -p chat_nse state`
Expected: 컴파일 오류. `cannot find type 'Map'`, `cannot find function 'encode'`, `cannot find value 'MAGIC'`

- [ ] **Step 2: 상태 파일 형식을 구현한다**

`core/chat_nse/src/state.rs` 맨 위의 `use` 두 줄을 지운다. 그 자리, 곧 시험 모듈 위에 다음 블록을 둔다. 블록에 `use`가 이미 들어 있다.

```rust
//! Snapshot of one receiver's OpenMLS `MemoryStorage` map plus the group id.
//!
//! Format, integers big-endian u32:
//! `b"CNS1" | gid_len | gid | n | n x (key_len | value_len | key | value)`, keys sorted.

use std::collections::HashMap;
use std::io::{self, Read};

const MAGIC: &[u8; 4] = b"CNS1";
/// No single field is larger than this. A corrupt length must not make the NSE allocate gigabytes.
const MAX_FIELD: usize = 64 << 20;
/// Upper bound on entries for the same reason.
const MAX_ENTRIES: usize = 1 << 20;

/// The `MemoryStorage::values` map.
pub type Map = HashMap<Vec<u8>, Vec<u8>>;

pub fn encode(group_id: &[u8], map: &Map) -> Vec<u8> {
    let mut entries: Vec<_> = map.iter().collect();
    entries.sort();
    let mut out = MAGIC.to_vec();
    out.extend_from_slice(&(group_id.len() as u32).to_be_bytes());
    out.extend_from_slice(group_id);
    out.extend_from_slice(&(entries.len() as u32).to_be_bytes());
    for (key, value) in entries {
        out.extend_from_slice(&(key.len() as u32).to_be_bytes());
        out.extend_from_slice(&(value.len() as u32).to_be_bytes());
        out.extend_from_slice(key);
        out.extend_from_slice(value);
    }
    out
}

/// Streams the snapshot, so the file is never held in memory next to the map.
pub fn read(r: &mut impl Read) -> io::Result<(Vec<u8>, Map)> {
    if bytes(r, 4)? != MAGIC {
        return Err(invalid("bad magic"));
    }
    let len = u32(r)?;
    let group_id = bytes(r, len)?;
    let n = u32(r)?;
    if n > MAX_ENTRIES {
        return Err(invalid("too many entries"));
    }
    let mut map = HashMap::with_capacity(n);
    for _ in 0..n {
        let (key_len, value_len) = (u32(r)?, u32(r)?);
        let key = bytes(r, key_len)?;
        map.insert(key, bytes(r, value_len)?);
    }
    Ok((group_id, map))
}

fn invalid(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, what.to_owned())
}

fn u32(r: &mut impl Read) -> io::Result<usize> {
    let mut b = [0; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_be_bytes(b) as usize)
}

fn bytes(r: &mut impl Read, len: usize) -> io::Result<Vec<u8>> {
    if len > MAX_FIELD {
        return Err(invalid("field too large"));
    }
    let mut v = vec![0; len];
    r.read_exact(&mut v)?;
    Ok(v)
}
```

Run: `cd /c/ChatProject && cargo test -p chat_nse state`
Expected: `6 passed`

- [ ] **Step 3: 복호 함수의 오류 경로 시험을 쓴다**

`core/chat_nse/src/lib.rs`:

```rust
pub mod state;

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &tempfile::TempDir, bytes: &[u8]) -> String {
        let path = dir.path().join("nse_state.bin");
        std::fs::write(&path, bytes).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn missing_state_file_is_bad_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("missing.bin").to_string_lossy().into_owned();
        let err = probe_decrypt(path, vec![1, 2, 3]).unwrap_err();
        assert!(matches!(err, NseError::BadState(_)), "{err:?}");
    }

    #[test]
    fn state_with_bad_magic_is_bad_state() {
        let dir = tempfile::tempdir().unwrap();
        let err = probe_decrypt(write(&dir, b"NOPE"), Vec::new()).unwrap_err();
        assert!(matches!(err, NseError::BadState(_)), "{err:?}");
    }

    #[test]
    fn state_without_the_group_is_group_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let path = write(&dir, &state::encode(b"gid", &state::Map::new()));
        let err = probe_decrypt(path, Vec::new()).unwrap_err();
        assert!(matches!(err, NseError::GroupNotFound), "{err:?}");
    }
}
```

Run: `cd /c/ChatProject && cargo test -p chat_nse`
Expected: 컴파일 오류. `cannot find function 'probe_decrypt'`, `cannot find type 'NseError'`

- [ ] **Step 4: 복호 함수를 구현한다**

`core/chat_nse/src/lib.rs`의 첫 줄 `pub mod state;`를 다음으로 바꾼다. 시험 모듈은 그대로 둔다.

```rust
//! MLS decrypt for the iOS notification service extension (design 12.3). MLS only.
//!
//! The persisted state is never written: OpenMLS's writes during decrypt (it re-saves
//! the message secrets, spike B) land in an in-memory copy that is dropped on return.

use std::fs::File;
use std::io::BufReader;
use std::time::Instant;

use openmls::prelude::tls_codec::Deserialize as _;
use openmls::prelude::*;
use openmls_rust_crypto::OpenMlsRustCrypto;

pub mod state;

uniffi::setup_scaffolding!();

#[derive(Debug, uniffi::Record)]
pub struct ProbeOutcome {
    pub plaintext_len: u32,
    /// Up to 48 bytes of the plaintext, lossy UTF-8, for the notification body.
    pub preview: String,
    /// Read + decode the state file + `MlsGroup::load`.
    pub load_micros: u64,
    /// TLS decode + `process_message`.
    pub decrypt_micros: u64,
}

#[derive(Debug, thiserror::Error, uniffi::Error)]
#[uniffi(flat_error)]
pub enum NseError {
    #[error("bad state: {0}")]
    BadState(String),
    #[error("group not in state")]
    GroupNotFound,
    #[error("bad ciphertext: {0}")]
    BadCiphertext(String),
    #[error("decrypt failed: {0}")]
    Decrypt(String),
    #[error("not an application message")]
    NotApplication,
}

/// Reads the state file (see [`state`]) into a fresh provider, loads its group and
/// decrypts one `MlsMessageOut` wire encoding of a `PrivateMessage`. Rust streams the
/// file itself, so Swift never holds the state or copies it across the FFI.
#[uniffi::export]
pub fn probe_decrypt(state_path: String, ciphertext: Vec<u8>) -> Result<ProbeOutcome, NseError> {
    let start = Instant::now();
    let (group_id, map) = File::open(&state_path)
        .and_then(|f| state::read(&mut BufReader::new(f)))
        .map_err(|e| NseError::BadState(e.to_string()))?;
    let provider = OpenMlsRustCrypto::default();
    *provider.storage().values.write().unwrap() = map;
    let mut group = MlsGroup::load(provider.storage(), &GroupId::from_slice(&group_id))
        .map_err(|e| NseError::BadState(e.to_string()))?
        .ok_or(NseError::GroupNotFound)?;
    let loaded = Instant::now();

    let message = MlsMessageIn::tls_deserialize_exact(&ciphertext)
        .map_err(|e| NseError::BadCiphertext(e.to_string()))?
        .try_into_protocol_message()
        .map_err(|e| NseError::BadCiphertext(e.to_string()))?;
    let processed = group
        .process_message(&provider, message)
        .map_err(|e| NseError::Decrypt(e.to_string()))?;
    let ProcessedMessageContent::ApplicationMessage(app) = processed.into_content() else {
        return Err(NseError::NotApplication);
    };
    let plaintext = app.into_bytes();
    let done = Instant::now();

    Ok(ProbeOutcome {
        plaintext_len: plaintext.len() as u32,
        preview: String::from_utf8_lossy(&plaintext[..plaintext.len().min(48)]).into_owned(),
        load_micros: (loaded - start).as_micros() as u64,
        decrypt_micros: (done - loaded).as_micros() as u64,
    })
}
```

Run: `cd /c/ChatProject && cargo test -p chat_nse`
Expected: `9 passed`. 상태 형식 6개와 오류 경로 3개다.

- [ ] **Step 5: 측정 고정 파일 생성기의 시험을 쓴다**

`tools/nse_fixture/Cargo.toml`:

```toml
[package]
name = "nse_fixture"
version = "0.1.0"
edition = "2024"
publish = false

[dependencies]
base64 = "0.22"
chat_nse = { path = "../../core/chat_nse" }
openmls = "=0.9.0"
openmls_basic_credential = "=0.6.0"
openmls_rust_crypto = "=0.6.0"

[dev-dependencies]
tempfile = "3"
```

루트 `Cargo.toml`의 `members`에 `tools/nse_fixture`를 더한다:

```toml
members = ["core/chat_core", "core/chat_nse", "app/rust", "tools/uniffi-bindgen", "tools/nse_fixture"]
```

`tools/nse_fixture/tests/decrypt.rs`:

```rust
use std::fs;
use std::path::Path;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chat_nse::{NseError, probe_decrypt};
use nse_fixture::build;

fn write_state(dir: &tempfile::TempDir, state: &[u8]) -> String {
    let path = dir.path().join("nse_state.bin");
    fs::write(&path, state).unwrap();
    path.to_string_lossy().into_owned()
}

#[test]
fn every_ciphertext_decrypts_against_the_same_state() {
    let fixture = build(10, 3, 4);
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(&dir, &fixture.state);
    for (seq, ct) in fixture.ciphertexts.iter().enumerate() {
        let outcome = probe_decrypt(path.clone(), ct.clone()).unwrap();
        assert_eq!(outcome.plaintext_len, 1024);
        assert!(outcome.preview.starts_with(&format!("M1b probe message {seq} ")), "{}", outcome.preview);
    }
}

#[test]
fn decrypting_twice_works_and_leaves_the_state_file_unchanged() {
    let fixture = build(10, 1, 4);
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(&dir, &fixture.state);
    probe_decrypt(path.clone(), fixture.ciphertexts[0].clone()).unwrap();
    probe_decrypt(path.clone(), fixture.ciphertexts[0].clone()).unwrap();
    assert_eq!(fs::read(&path).unwrap(), fixture.state);
}

#[test]
fn corrupted_ciphertext_is_an_error_not_a_panic() {
    let fixture = build(10, 1, 4);
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(&dir, &fixture.state);
    let mut ct = fixture.ciphertexts[0].clone();
    let last = ct.len() - 1;
    ct[last] ^= 0xFF; // inside the AEAD tag
    let err = probe_decrypt(path, ct).unwrap_err();
    assert!(matches!(err, NseError::Decrypt(_) | NseError::BadCiphertext(_)), "{err:?}");
}

#[test]
fn empty_ciphertext_is_bad_ciphertext() {
    let fixture = build(10, 1, 4);
    let dir = tempfile::tempdir().unwrap();
    let path = write_state(&dir, &fixture.state);
    let err = probe_decrypt(path, Vec::new()).unwrap_err();
    assert!(matches!(err, NseError::BadCiphertext(_)), "{err:?}");
}

#[test]
fn committed_fixture_decrypts() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/ios/NotificationService/Fixture");
    let path = dir.join("nse_state.bin").to_string_lossy().into_owned();
    let lines = fs::read_to_string(dir.join("ciphertexts.txt")).unwrap();
    let cts: Vec<Vec<u8>> = lines.lines().map(|l| STANDARD.decode(l).unwrap()).collect();
    assert_eq!(cts.len(), 8);
    for seq in [0, 7] {
        let outcome = probe_decrypt(path.clone(), cts[seq].clone()).unwrap();
        assert!(outcome.preview.starts_with(&format!("M1b probe message {seq} ")), "{}", outcome.preview);
    }
}
```

`tools/nse_fixture/src/lib.rs`에는 우선 다음만 둔다:

```rust
pub struct Fixture {
    pub state: Vec<u8>,
    pub ciphertexts: Vec<Vec<u8>>,
}
```

Run: `cd /c/ChatProject && cargo test -p nse_fixture`
Expected: 컴파일 오류. `unresolved import 'nse_fixture::build'`

- [ ] **Step 6: 생성기를 구현한다**

`tools/nse_fixture/src/lib.rs` 전체:

```rust
//! Builds spike B's rev3 group and exports receiver R's state plus K new ciphertexts from S.
//! The ciphertexts are at generations after R's stored ratchet, so each one decrypts
//! against the same state (the NSE never persists its writes).

use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;

const CS: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

pub struct Fixture {
    /// `chat_nse::state` snapshot of R.
    pub state: Vec<u8>,
    /// K `MlsMessageOut` wire encodings; index i holds `plaintext(i)`.
    pub ciphertexts: Vec<Vec<u8>>,
}

struct Client {
    provider: OpenMlsRustCrypto,
    signer: SignatureKeyPair,
    credential: CredentialWithKey,
}

impl Client {
    /// `credential_bytes` pads the BasicCredential identity (an InstallationCert is about 200 B).
    fn new(name: &str, credential_bytes: usize) -> Self {
        let provider = OpenMlsRustCrypto::default();
        let signer = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
        signer.store(provider.storage()).unwrap();
        let mut identity = name.as_bytes().to_vec();
        identity.resize(credential_bytes.max(identity.len()), b'.');
        let credential = CredentialWithKey {
            credential: BasicCredential::new(identity).into(),
            signature_key: signer.public().into(),
        };
        Self { provider, signer, credential }
    }

    fn key_package(&self) -> KeyPackage {
        let bundle = KeyPackage::builder()
            .build(CS, &self.provider, &self.signer, self.credential.clone())
            .unwrap();
        bundle.key_package().clone()
    }

    fn send(&self, group: &mut MlsGroup, plaintext: &[u8]) -> Vec<u8> {
        let message = group.create_message(&self.provider, &self.signer, plaintext).unwrap();
        message.tls_serialize_detached().unwrap()
    }
}

fn process(group: &mut MlsGroup, provider: &OpenMlsRustCrypto, wire: &[u8]) -> ProcessedMessageContent {
    let message = MlsMessageIn::tls_deserialize_exact(wire).unwrap();
    group
        .process_message(provider, message.try_into_protocol_message().unwrap())
        .unwrap()
        .into_content()
}

fn receive_commit(group: &mut MlsGroup, provider: &OpenMlsRustCrypto, commit: &MlsMessageOut) {
    let ProcessedMessageContent::StagedCommitMessage(staged) =
        process(group, provider, &commit.tls_serialize_detached().unwrap())
    else {
        panic!("expected a commit")
    };
    group.merge_staged_commit(provider, *staged).unwrap();
}

/// 1 KiB of readable text, so the notification preview shows which message was decrypted.
pub fn plaintext(seq: usize) -> Vec<u8> {
    let mut p = format!("M1b probe message {seq} ").into_bytes();
    p.resize(1024, b'.');
    p
}

/// N members (S, R and N-2 that never process anything), rev3 config, spike B traffic.
pub fn build(n: usize, k: usize, credential_bytes: usize) -> Fixture {
    // Spike B rev3: max_past_epochs 5, sender ratchet (out_of_order 200, forward 1000).
    let create_config = MlsGroupCreateConfig::builder()
        .ciphersuite(CS)
        .max_past_epochs(5)
        .sender_ratchet_configuration(SenderRatchetConfiguration::new(200, 1000))
        .build();
    let join_config = create_config.join_config().clone();

    // S adds R and the others by value, 50 per commit (spike B).
    let s = Client::new("S", credential_bytes);
    let r = Client::new("R", credential_bytes);
    let mut s_group = MlsGroup::new(&s.provider, &s.signer, &create_config, s.credential.clone()).unwrap();
    let mut key_packages = vec![r.key_package()];
    key_packages.extend((2..n).map(|i| Client::new(&format!("M{i}"), credential_bytes).key_package()));
    let mut r_group: Option<MlsGroup> = None;
    for batch in key_packages.chunks(50) {
        let (commit, welcome, _) = s_group.add_members(&s.provider, &s.signer, batch).unwrap();
        s_group.merge_pending_commit(&s.provider).unwrap();
        match &mut r_group {
            Some(group) => receive_commit(group, &r.provider, &commit),
            None => {
                let welcome = MlsMessageIn::tls_deserialize_exact(welcome.tls_serialize_detached().unwrap()).unwrap();
                let MlsMessageBodyIn::Welcome(welcome) = welcome.extract() else {
                    panic!("expected a welcome")
                };
                let tree = s_group.export_ratchet_tree().into();
                let staged = StagedWelcome::new_from_welcome(&r.provider, &join_config, welcome, Some(tree)).unwrap();
                r_group = Some(staged.into_group(&r.provider).unwrap());
            }
        }
    }
    let mut r_group = r_group.unwrap();

    // Spike B traffic: 6 epochs R processes, then 200 messages of which R decrypts the last.
    for _ in 0..6 {
        let bundle = s_group.self_update(&s.provider, &s.signer, LeafNodeParameters::default()).unwrap();
        s_group.merge_pending_commit(&s.provider).unwrap();
        receive_commit(&mut r_group, &r.provider, bundle.commit());
    }
    let mut last = Vec::new();
    for _ in 0..200 {
        last = s.send(&mut s_group, &plaintext(usize::MAX));
    }
    assert!(matches!(
        process(&mut r_group, &r.provider, &last),
        ProcessedMessageContent::ApplicationMessage(_)
    ));

    // Snapshot R, then K messages that R's snapshot has not seen.
    let snapshot = r.provider.storage().values.read().unwrap().clone();
    let state = chat_nse::state::encode(s_group.group_id().as_slice(), &snapshot);
    let ciphertexts = (0..k).map(|seq| s.send(&mut s_group, &plaintext(seq))).collect();
    Fixture { state, ciphertexts }
}
```

`tools/nse_fixture/src/main.rs`:

```rust
//! Usage: nse_fixture <out_dir> [k = 8] [credential_bytes = 200]
//! Writes nse_state.bin (N = 200) and ciphertexts.txt (one base64 per line, line i = seq i - 1).

use std::{fs, path::PathBuf};

use base64::{Engine as _, engine::general_purpose::STANDARD};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = PathBuf::from(args.get(1).expect("usage: nse_fixture <out_dir> [k] [credential_bytes]"));
    let k: usize = args.get(2).map_or(8, |a| a.parse().unwrap());
    let credential_bytes: usize = args.get(3).map_or(200, |a| a.parse().unwrap());

    let fixture = nse_fixture::build(200, k, credential_bytes);
    fs::create_dir_all(&out).unwrap();
    let state_path = out.join("nse_state.bin");
    fs::write(&state_path, &fixture.state).unwrap();

    // Self-check through the same entry point the NSE calls.
    for (seq, ct) in fixture.ciphertexts.iter().enumerate() {
        let outcome = chat_nse::probe_decrypt(state_path.to_string_lossy().into_owned(), ct.clone()).unwrap();
        assert_eq!(outcome.plaintext_len, 1024);
        println!(
            "seq {seq}: ciphertext {} B, base64 {} B, load {} us, decrypt {} us",
            ct.len(),
            STANDARD.encode(ct).len(),
            outcome.load_micros,
            outcome.decrypt_micros,
        );
    }
    let lines: Vec<String> = fixture.ciphertexts.iter().map(|ct| STANDARD.encode(ct)).collect();
    fs::write(out.join("ciphertexts.txt"), lines.join("\n") + "\n").unwrap();
    println!("state {} B", fixture.state.len());
}
```

Run: `cd /c/ChatProject && cargo test -p nse_fixture -- --skip committed_fixture`
Expected: `4 passed`. `committed_fixture_decrypts`는 아직 파일이 없어서 건너뛴다.

- [ ] **Step 7: 측정 고정 파일을 만들고 시험한다**

```bash
cd /c/ChatProject
cargo run --release -p nse_fixture -- app/ios/NotificationService/Fixture 8 200
cargo test -p nse_fixture
```

Expected:
- 생성기가 `seq 0` … `seq 7` 줄을 출력한다. 줄마다 `ciphertext 1170 B, base64 1560 B` 근처 값이다.
- 마지막 줄은 `state 13xxxxx B`(약 1.35 MB)다.
- 시험은 `5 passed`다.

`.gitattributes` 끝에 더한다:

```gitattributes
# Binary fixtures: never convert line endings.
*.bin binary
```

- [ ] **Step 8: 정적 검사와 커밋**

Run: `cd /c/ChatProject && cargo test --workspace && cargo clippy --workspace --all-targets -- -D warnings`
Expected: 모두 통과, 경고 없음.

```bash
cd /c/ChatProject
git add Cargo.toml Cargo.lock .gitattributes core/chat_nse tools/nse_fixture app/ios/NotificationService/Fixture
git commit -m "feat(nse): chat_nse MLS decrypt for the iOS extension and a 200-leaf probe fixture"
git push
```

CI 확인: `rust` 작업의 `cargo test --workspace`가 `chat_nse` 9개와 `nse_fixture` 5개를 포함해 녹색이다.

### Task 5: 바인딩 재생성 스크립트와 CI 검사

**Files:**
- Create: `tools/gen_bindings.sh`
- Create (생성됨): `app/ios/NotificationService/Generated/chat_nse.swift`, `chat_nseFFI.h`, `chat_nseFFI.modulemap`
- Modify: `.github/workflows/ci.yml` (작업 `bindings`)

**Interfaces:**
- Consumes:
  - Task 4의 `chat_nse`.
  - M1a의 `tools/uniffi-bindgen`과 `app/flutter_rust_bridge.yaml`.
- Produces:
  - `bash tools/gen_bindings.sh`는 커밋된 바인딩 셋을 모두 다시 만든다.
    - flutter_rust_bridge: `app/lib/src/rust/**`, `app/rust/src/frb_generated.rs`, freezed 결과.
    - UniFFI Kotlin: `chat_ffi.kt`.
    - UniFFI Swift: `Generated/chat_nse*`.
  - CI 작업 `bindings`는 스크립트를 돌린 뒤 작업 트리가 커밋과 다르면 실패한다.

UniFFI Swift는 호출할 때마다 체크섬을 검사한다. 커밋된 Swift 바인딩이 다시 빌드한 `libchat_nse.a`와 어긋나면 NSE가 `fatalError`로 죽는다. 이 검사가 그 어긋남을 PR에서 잡는다.

- [ ] **Step 1: 스크립트를 쓴다**

`tools/gen_bindings.sh`:

```bash
#!/usr/bin/env bash
# Regenerates every committed binding from the Rust sources:
#   flutter_rust_bridge: app/lib/src/rust/**, app/rust/src/frb_generated.rs (+ freezed)
#   UniFFI Kotlin (chat_ffi): app/android/app/src/main/kotlin/uniffi/chat_ffi/chat_ffi.kt
#   UniFFI Swift (chat_nse):  app/ios/NotificationService/Generated/
# The `bindings` CI job runs this and fails if anything differs from the commit.
set -euo pipefail
cd "$(dirname "$0")/.."

case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) ffi_lib=target/debug/chat_ffi.dll nse_lib=target/debug/chat_nse.lib ;;
  Darwin) ffi_lib=target/debug/libchat_ffi.dylib nse_lib=target/debug/libchat_nse.a ;;
  *) ffi_lib=target/debug/libchat_ffi.so nse_lib=target/debug/libchat_nse.a ;;
esac

version="$(flutter_rust_bridge_codegen --version)"
case "$version" in
  *2.11.1*) ;;
  *) echo "flutter_rust_bridge_codegen 2.11.1 is required, found: $version" >&2; exit 1 ;;
esac

(cd app && flutter_rust_bridge_codegen generate && dart run build_runner build)
cargo build -p chat_ffi -p chat_nse
cargo run -q -p uniffi-bindgen -- generate --library "$ffi_lib" --language kotlin \
  --out-dir app/android/app/src/main/kotlin --no-format
cargo run -q -p uniffi-bindgen -- generate --library "$nse_lib" --language swift \
  --out-dir app/ios/NotificationService/Generated --no-format
```

- [ ] **Step 2: 이 PC에서 돌려 Swift 바인딩을 만든다**

Run: `cd /c/ChatProject && bash tools/gen_bindings.sh && git diff --stat && git status --porcelain | grep '^??'`
Expected:
- `git diff --stat`는 아무것도 출력하지 않는다(줄바꿈 경고 줄만 나올 수 있다). 기존 FRB와 Kotlin 파일의 내용이 그대로라는 뜻이다.
- 이 PC는 `core.autocrlf=true`라서, `git status`에는 내용이 같은 파일이 ` M`으로 뜰 수 있다. 무시한다.
- `??` 줄은 `app/ios/NotificationService/Generated/` 하나다.
- `git diff --stat`에 파일이 나오면 멈추고 보고한다. 커밋된 바인딩이 낡았거나 생성 결과가 실행마다 다르다는 뜻이다.

Run: `grep -n "public func probeDecrypt" app/ios/NotificationService/Generated/chat_nse.swift`
Expected: `public func probeDecrypt(statePath: String, ciphertext: Data)throws  -> ProbeOutcome` 꼴의 한 줄. 공백은 UniFFI 템플릿 그대로다.

- [ ] **Step 3: 검사가 어긋남을 잡는지 본다**

```bash
cd /c/ChatProject
echo "// drift" >> app/ios/NotificationService/Generated/chat_nse.swift
bash tools/gen_bindings.sh && git status --porcelain app/ios/NotificationService/Generated
```

Expected:
- 다시 만든 뒤에는 `// drift`가 사라진다.
- `git status`는 여전히 새 파일 셋만 보인다. 즉 스크립트가 손으로 고친 내용을 덮어쓴다. 그래서 CI에서는 그 차이가 `git status`에 드러난다.

- [ ] **Step 4: CI 작업을 더한다**

`.github/workflows/ci.yml`의 `rust` 작업 아래에 더한다:

```yaml
  bindings:
    runs-on: ubuntu-latest
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false
      - uses: dtolnay/rust-toolchain@7e38f4b43b4db5c8dd498af069a4f6196df1d067 # master 2026-10-01
        with:
          toolchain: ${{ env.RUST_TOOLCHAIN }}
      # Also caches ~/.cargo/bin, so the codegen below is built once.
      - uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6 # v2.9.2
      - uses: subosito/flutter-action@1a449444c387b1966244ae4d4f8c696479add0b2 # v2.23.0
        with:
          flutter-version: ${{ env.FLUTTER_VERSION }}
          channel: stable
      - run: cargo install flutter_rust_bridge_codegen --version 2.11.1 --locked
      - name: Regenerate
        run: |
          (cd app && flutter pub get)
          bash tools/gen_bindings.sh
      - name: Generated files match the commit
        run: |
          changes="$(git status --porcelain)"
          if [ -n "$changes" ]; then
            echo "::error::Generated bindings differ from the commit. Run tools/gen_bindings.sh and commit the result."
            echo "$changes"
            git --no-pager diff --stat
            exit 1
          fi
```

- [ ] **Step 5: 커밋하고 CI를 본다**

Run: `cd /c/ChatProject && python tools/ci/check_workflows.py`
Expected: `workflows ok`

```bash
cd /c/ChatProject
git add tools/gen_bindings.sh app/ios/NotificationService/Generated .github/workflows/ci.yml
git commit -m "build: one script regenerates the FRB, Kotlin and Swift bindings; CI fails on drift"
git push
```

CI 확인: `bindings`가 녹색이다. Linux에서 다시 만든 결과가 Windows에서 만든 커밋과 같다는 뜻이다.
- 빨갛다면 로그의 `git diff --stat`를 본다.
- 운영체제마다 결과가 다른 파일이 있으면 그 파일만 Linux에서 만든 것으로 바꿔 커밋하고, 원인을 보고서에 적는다.

### Task 6: 알림 확장(NSE) 타깃

**Files:**
- Create: `app/ios/NotificationService/NotificationService.swift`, `Info.plist`, `NotificationService.entitlements`, `NotificationService-Bridging-Header.h`
- Create: `app/ios/Runner/Runner.entitlements`
- Create: `app/ios/scripts/add_notification_service.rb`
- Modify: `.github/workflows/ci.yml` (작업 `ios`)
- Modify: `docs/superpowers/specs/2026-10-05-test-automation-design.md` (6, 9, 10절)

**Interfaces:**
- Consumes:
  - Task 4의 `Fixture/nse_state.bin`.
  - Task 5의 `Generated/chat_nse.swift`(`probeDecrypt`, `ProbeOutcome`).
  - Task 3의 `ios` 작업.
- Produces:
  - `ruby app/ios/scripts/add_notification_service.rb <Runner.xcodeproj>`는 NotificationService 타깃을 프로젝트에 더한다.
    - 바꾼 것이 있으면 `updated ...`, 없으면 `... already up to date`를 출력한다.
    - Task 8이 이 스크립트에 서명 설정을 더한다.
  - NSE 기록 형식. App Group 루트의 `nse_memory.jsonl`에 알림마다 두 줄을 쓴다. Task 7이 읽는다.
    - 시작할 때: `{"phase":"start","seq":N,"sent_at":ms,"received_at":ms,"pid":P,"footprint":bytes}`
    - 끝날 때, 성공: `{"phase":"end","seq":N,"pid":P,"ok":true,"plaintext_len":L,"load_us":U,"decrypt_us":U,"footprint":bytes,"peak":bytes,"available":bytes}`
    - 끝날 때, 실패: `{"phase":"end","seq":N,"pid":P,"ok":false,"error":"...","footprint":bytes,"peak":bytes,"available":bytes}`
    - `peak`를 읽지 못하면 `-1`이다.
    - 페이로드에 `e`가 없으면 `error`가 `noCiphertext`다. 이 기록이 복호 없는 기준값이다.

NSE의 피크(`ledger_phys_footprint_peak`)는 프로세스가 살아 있는 동안의 최댓값이다. iOS는 NSE 프로세스를 다시 쓰기도 하므로 기록에 `pid`를 남긴다.

- [ ] **Step 1: NSE 소스 파일을 쓴다**

`app/ios/NotificationService/Info.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>$(DEVELOPMENT_LANGUAGE)</string>
	<key>CFBundleDisplayName</key>
	<string>NotificationService</string>
	<key>CFBundleExecutable</key>
	<string>$(EXECUTABLE_NAME)</string>
	<key>CFBundleIdentifier</key>
	<string>$(PRODUCT_BUNDLE_IDENTIFIER)</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>$(PRODUCT_NAME)</string>
	<key>CFBundlePackageType</key>
	<string>$(PRODUCT_BUNDLE_PACKAGE_TYPE)</string>
	<key>CFBundleShortVersionString</key>
	<string>$(FLUTTER_BUILD_NAME)</string>
	<key>CFBundleVersion</key>
	<string>$(FLUTTER_BUILD_NUMBER)</string>
	<key>NSExtension</key>
	<dict>
		<key>NSExtensionPointIdentifier</key>
		<string>com.apple.usernotifications.service</string>
		<key>NSExtensionPrincipalClass</key>
		<string>$(PRODUCT_MODULE_NAME).NotificationService</string>
	</dict>
</dict>
</plist>
```

`app/ios/NotificationService/NotificationService.entitlements`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>com.apple.security.application-groups</key>
	<array>
		<string>group.dev.chatproject.chatapp</string>
	</array>
</dict>
</plist>
```

`app/ios/Runner/Runner.entitlements`. 배포 프로파일로 서명하면 Xcode가 `aps-environment`를 `production`으로 바꾼다.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>aps-environment</key>
	<string>development</string>
	<key>com.apple.security.application-groups</key>
	<array>
		<string>group.dev.chatproject.chatapp</string>
	</array>
</dict>
</plist>
```

`app/ios/NotificationService/NotificationService-Bridging-Header.h`:

```c
// Exposes the UniFFI C declarations of chat_nse to Swift (generated, see tools/gen_bindings.sh).
#include "Generated/chat_nseFFI.h"
// os_proc_available_memory
#include <os/proc.h>
```

`app/ios/NotificationService/NotificationService.swift`:

```swift
import Foundation
import UserNotifications

/// M1b probe (design 12.3, roadmap M1b): decrypts the push's MLS ciphertext `e` against the
/// bundled 200-leaf group state and appends its own memory use to the App Group file
/// `nse_memory.jsonl`. M7 replaces it with the real extension.
final class NotificationService: UNNotificationServiceExtension {
    private static let appGroup = "group.dev.chatproject.chatapp"
    private static let logName = "nse_memory.jsonl"

    private let lock = NSLock()
    private var contentHandler: ((UNNotificationContent) -> Void)?
    private var original: UNNotificationContent?

    override func didReceive(_ request: UNNotificationRequest,
                             withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void) {
        lock.lock()
        self.contentHandler = contentHandler
        original = request.content
        lock.unlock()

        let info = request.content.userInfo
        let seq = (info["seq"] as? NSNumber)?.intValue ?? -1
        let pid = Int(getpid())
        // Written before any work, so a record exists even if iOS kills the extension.
        Self.append([
            "phase": "start",
            "seq": seq,
            "sent_at": (info["sent_at"] as? NSNumber)?.int64Value ?? -1,
            "received_at": Int64(Date().timeIntervalSince1970 * 1000),
            "pid": pid,
            "footprint": Memory.sample()?.footprint ?? 0,
        ])

        guard let content = request.content.mutableCopy() as? UNMutableNotificationContent else {
            return deliver(request.content)
        }
        var end: [String: Any] = ["phase": "end", "seq": seq, "pid": pid]
        do {
            guard let e = info["e"] as? String, let ciphertext = Data(base64Encoded: e) else {
                throw ProbeError.noCiphertext
            }
            guard let state = Bundle.main.path(forResource: "nse_state", ofType: "bin") else {
                throw ProbeError.noState
            }
            let outcome = try probeDecrypt(statePath: state, ciphertext: ciphertext)
            end["ok"] = true
            end["plaintext_len"] = outcome.plaintextLen
            end["load_us"] = outcome.loadMicros
            end["decrypt_us"] = outcome.decryptMicros
            content.body = "#\(seq) \(outcome.preview)"
        } catch {
            end["ok"] = false
            end["error"] = "\(error)"
            content.body = "#\(seq) 복호 실패: \(error)"
        }
        let sample = Memory.sample()
        end["footprint"] = sample?.footprint ?? 0
        end["peak"] = sample?.peak ?? -1
        end["available"] = os_proc_available_memory() // 0 when iOS does not report it for extensions
        Self.append(end)
        if let peak = sample?.peak {
            content.title = String(format: "NSE 최대 %.1f MB", Double(peak) / 1_048_576)
        }
        deliver(content)
    }

    override func serviceExtensionTimeWillExpire() {
        lock.lock()
        let content = original
        lock.unlock()
        if let content { deliver(content) }
    }

    /// Calls the handler at most once, whichever path gets here first.
    private func deliver(_ content: UNNotificationContent) {
        lock.lock()
        let handler = contentHandler
        contentHandler = nil
        lock.unlock()
        handler?(content)
    }

    /// Appends one JSON line with O_APPEND, so lines from concurrent notifications do not interleave.
    private static func append(_ record: [String: Any]) {
        guard let dir = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: appGroup),
              var line = try? JSONSerialization.data(withJSONObject: record, options: [.sortedKeys]) else { return }
        line.append(0x0A)
        let fd = open(dir.appendingPathComponent(logName).path, O_WRONLY | O_CREAT | O_APPEND, 0o644)
        guard fd >= 0 else { return }
        defer { close(fd) }
        _ = line.withUnsafeBytes { write(fd, $0.baseAddress, $0.count) }
    }
}

private enum ProbeError: Error { case noCiphertext, noState }

private enum Memory {
    struct Sample { let footprint: UInt64; let peak: Int64? }

    /// TASK_VM_INFO: phys_footprint now, and the lifetime peak (rev 3, iOS 13+).
    static func sample() -> Sample? {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size)
        let kr = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
            }
        }
        guard kr == KERN_SUCCESS else { return nil }
        // The kernel reports how many words it filled; the peak field needs revision 3.
        let peakEnd = (MemoryLayout<task_vm_info_data_t>.offset(of: \task_vm_info_data_t.ledger_phys_footprint_peak)!
                       + MemoryLayout<Int64>.size) / MemoryLayout<natural_t>.size
        return Sample(footprint: info.phys_footprint,
                      peak: Int(count) >= peakEnd ? info.ledger_phys_footprint_peak : nil)
    }
}
```

- [ ] **Step 2: 타깃을 더하는 Ruby 스크립트를 쓴다**

`app/ios/scripts/add_notification_service.rb`:

```ruby
# frozen_string_literal: true

# Adds the NotificationService app extension target to Runner.xcodeproj (M1b).
# There is no Mac, so this script is the source of truth: CI runs it before every iOS build.
# Running it twice changes nothing ("already up to date").
#
#   gem install xcodeproj -v 1.28.1 --no-document
#   ruby app/ios/scripts/add_notification_service.rb app/ios/Runner.xcodeproj
gem 'xcodeproj', '1.28.1'
require 'xcodeproj'

APP_ID = 'dev.chatproject.chatapp'
NSE = 'NotificationService'
CONFIGS = %w[Debug Release Profile].freeze

project_path = ARGV.fetch(0)
project = Xcodeproj::Project.open(project_path)
deployment_target = project.build_configuration_list['Release'].build_settings['IPHONEOS_DEPLOYMENT_TARGET']
changed = false

# Compare in the form xcodeproj writes (arrays joined by spaces).
set = lambda do |config, key, value|
  flat = ->(v) { v.is_a?(Array) ? v.join(' ') : v }
  next if flat.(config.build_settings[key]) == flat.(value)

  config.build_settings[key] = value
  changed = true
end
file_ref = ->(group, path) { group.files.find { |f| f.path == path } || group.new_file(path) }

runner = project.native_targets.find { |t| t.name == 'Runner' } or abort 'no Runner target'

# Files of app/ios/NotificationService.
group = project.main_group[NSE] || project.main_group.new_group(NSE, NSE)
sources = ["#{NSE}.swift", 'Generated/chat_nse.swift'].map { |p| file_ref.(group, p) }
%W[Info.plist #{NSE}.entitlements #{NSE}-Bridging-Header.h Generated/chat_nseFFI.h].each { |p| file_ref.(group, p) }
state = file_ref.(group, 'Fixture/nse_state.bin')

# The extension target.
nse = project.native_targets.find { |t| t.name == NSE } ||
      project.new_target(:app_extension, NSE, :ios, deployment_target, nil, :swift)
CONFIGS.each { |name| nse.add_build_configuration(name, name == 'Debug' ? :debug : :release) }
nse.add_file_references(sources)
nse.add_resources([state])

# Build settings. Runner's xcconfig supplies FLUTTER_BUILD_NAME/NUMBER for Info.plist.
CONFIGS.each do |name|
  config = nse.build_configuration_list[name]
  base = runner.build_configuration_list[name].base_configuration_reference
  if config.base_configuration_reference != base
    config.base_configuration_reference = base
    changed = true
  end
  {
    'PRODUCT_NAME' => '$(TARGET_NAME)',
    'PRODUCT_BUNDLE_IDENTIFIER' => "#{APP_ID}.#{NSE}",
    'INFOPLIST_FILE' => "#{NSE}/Info.plist",
    'CODE_SIGN_ENTITLEMENTS' => "#{NSE}/#{NSE}.entitlements",
    'SWIFT_VERSION' => '5.0',
    'TARGETED_DEVICE_FAMILY' => '1,2',
    'IPHONEOS_DEPLOYMENT_TARGET' => deployment_target,
    'SKIP_INSTALL' => 'YES',
    'SWIFT_OBJC_BRIDGING_HEADER' => "#{NSE}/#{NSE}-Bridging-Header.h",
    'LIBRARY_SEARCH_PATHS[sdk=iphoneos*]' => '$(SRCROOT)/../../target/aarch64-apple-ios/release',
    'LIBRARY_SEARCH_PATHS[sdk=iphonesimulator*]' => '$(SRCROOT)/../../target/aarch64-apple-ios-sim/release',
    'OTHER_LDFLAGS' => ['$(inherited)', '-lchat_nse'],
    'EXCLUDED_ARCHS[sdk=iphonesimulator*]' => '$(inherited) x86_64' # the Rust simulator library is arm64 only
  }.each { |key, value| set.(config, key, value) }
end

# Embed the extension in Runner and build it first.
embed = runner.copy_files_build_phases.find { |p| p.name == 'Embed Foundation Extensions' } ||
        runner.new_copy_files_build_phase('Embed Foundation Extensions')
embed.symbol_dst_subfolder_spec = :plug_ins # 13
embed.dst_path = ''
embed.add_file_reference(nse.product_reference, true).settings = { 'ATTRIBUTES' => ['RemoveHeadersOnCopy'] }
runner.add_dependency(nse)
# Flutter's "Cycle inside Runner" fix: Embed Foundation Extensions must come before Run Script.
phases = runner.build_phases
run_script = phases.find { |p| p.is_a?(Xcodeproj::Project::Object::PBXShellScriptBuildPhase) && p.name == 'Run Script' } or
  abort 'no Run Script phase'
phases.move(embed, phases.index(run_script)) if phases.index(embed) > phases.index(run_script)

# Runner: App Group + push entitlements.
file_ref.(project.main_group['Runner'], 'Runner.entitlements')
runner.build_configurations.each do |config|
  set.(config, 'PRODUCT_BUNDLE_IDENTIFIER', APP_ID)
  set.(config, 'CODE_SIGN_ENTITLEMENTS', 'Runner/Runner.entitlements')
end

if changed || project.dirty?
  project.save
  puts "updated #{project_path}"
else
  puts "#{project_path} already up to date"
end
```

- [ ] **Step 3: `ios` 작업에 NSE 빌드를 더한다**

`.github/workflows/ci.yml`의 `ios` 작업에서 `Flutter packages` 단계 다음에 두 단계를 넣는다:

```yaml
      - name: Rust library for the notification extension
        run: cargo build -p chat_nse --release --target aarch64-apple-ios-sim
      - name: Add the notification extension target
        run: |
          gem install xcodeproj -v 1.28.1 --no-document
          ruby app/ios/scripts/add_notification_service.rb app/ios/Runner.xcodeproj
          # A second run must change nothing.
          ruby app/ios/scripts/add_notification_service.rb app/ios/Runner.xcodeproj | grep -q "already up to date"
```

`Integration tests on the simulator` 단계 다음에 더한다:

```yaml
      - name: The extension is embedded in the test build
        run: |
          appex="$(find app/build/ios/iphonesimulator -maxdepth 3 -name NotificationService.appex -print -quit)"
          test -n "$appex" || { echo "::error::NotificationService.appex is not embedded"; exit 1; }
          /usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$appex/Info.plist"
          test -f "$appex/nse_state.bin" || { echo "::error::nse_state.bin is not in the extension"; exit 1; }
```

- [ ] **Step 4: 자동 테스트 설계에 `simctl push`의 답을 적는다**

`docs/superpowers/specs/2026-10-05-test-automation-design.md` 6절의 두 번째 줄을 다음으로 바꾼다.
- 이전: `- \`xcrun simctl push\`로 알림 확장 처리 경로까지 시험할 수 있는지는 M1b에서 확인한다.`
- 이후:

```markdown
- `xcrun simctl push`는 알림 확장(NSE)을 실행하지 않는다. Xcode 11.4 릴리스 노트의 Known Issues(55822721)와 Xcode 14 릴리스 노트에 적혀 있고, Xcode 26.x까지 바뀌지 않았다. GitHub 러너의 시뮬레이터는 실제 APNs 토큰도 받지 못한다. 그래서 CI는 NSE가 빌드되어 앱 안(`Runner.app/PlugIns/NotificationService.appex`)에 들어갔는지만 확인한다. 복호와 메모리는 TestFlight 빌드로 잰다.
```

9절 M1b 줄:
- 이전: `| M1b | iOS 시뮬레이터 CI 작업, \`simctl push\` 확인 |`
- 이후: `| M1b | iOS 시뮬레이터 CI 작업, NSE 빌드·내장 확인 |`

10절 첫 줄:
- 이전: `- \`xcrun simctl push\`가 알림 확장을 실행하는가 (M1b에서 확인).`
- 이후: `- (M1b에서 답함) \`xcrun simctl push\`는 알림 확장을 실행하지 않는다. 6절.`

- [ ] **Step 5: 커밋하고 CI를 본다**

```bash
cd /c/ChatProject
git add app/ios/NotificationService app/ios/Runner/Runner.entitlements app/ios/scripts/add_notification_service.rb .github/workflows/ci.yml docs/superpowers/specs/2026-10-05-test-automation-design.md
git commit -m "feat(ios): notification service extension that decrypts the probe ciphertext and records its memory"
git push
```

CI 확인 (`ios` 작업):
- 스크립트 첫 실행이 `updated app/ios/Runner.xcodeproj`를 출력한다. 두 번째 실행의 grep 검사도 통과한다.
- 통합 테스트가 `🎉 3 tests passed.`다. NSE가 들어간 앱을 빌드해서 돈 결과다.
- `The extension is embedded` 단계가 `dev.chatproject.chatapp.NotificationService`를 출력한다.

빨갛다면 첫 오류를 고친다. 흔한 경우는 다음과 같다.
- `Cycle inside Runner`: 단계 순서가 틀렸다. 스크립트의 `phases.move`를 확인한다.
- `library 'chat_nse' not found`: Rust 라이브러리 단계가 빠졌거나 경로가 틀렸다.
- Swift 컴파일 오류: 생성된 바인딩과 브리징 헤더 경로를 확인한다.
- 시뮬레이터 서명 오류: 오류 원문을 보고서에 적고, `ios` 작업의 `env`에 `FLUTTER_XCODE_CODE_SIGNING_ALLOWED: 'NO'`를 더한다.

### Task 7: 앱의 "알림 측정" 화면

**Files:**
- Create: `app/lib/probe/nse_record.dart`, `app/lib/probe/probe_host.dart`, `app/lib/probe/probe_screen.dart`
- Create: `app/test/nse_record_test.dart`, `app/test/probe_screen_test.dart`
- Modify: `app/lib/home/home_screen.dart`, `app/lib/main.dart`, `app/test/home_screen_test.dart`
- Modify: `app/ios/Runner/AppDelegate.swift`
- Modify: `app/pubspec.yaml`, `app/pubspec.lock` (`path_provider_foundation`을 직접 의존성으로)

**Interfaces:**
- Consumes: Task 6의 NSE 기록 형식과 App Group `group.dev.chatproject.chatapp`.
- Produces:
  - `ProbeHost`: `requestPush() → Future<bool>`, `deviceToken() → Future<String?>`, `readNseLog() → Future<String?>`, `clearNseLog() → Future<void>`. 구현은 `IosProbeHost`다.
  - `parseNseLog(String) → List<NseRecord>`
  - `HomeScreen({required CoreClient core, ProbeHost? probe})`. `probe`가 있을 때만 측정 단추가 보인다.
  - iOS MethodChannel `chat_app/ios_probe`: `requestPush`(bool), `deviceToken`(String?)

- [ ] **Step 1: 기록 해석 시험을 쓴다**

`app/test/nse_record_test.dart`:

```dart
import 'package:chat_app/probe/nse_record.dart';
import 'package:flutter_test/flutter_test.dart';

const _okPair = '''
{"footprint":3000000,"phase":"start","pid":7,"received_at":1000500,"sent_at":1000000,"seq":1}
{"available":0,"decrypt_us":1700,"footprint":5000000,"load_us":3700,"ok":true,"peak":6291456,"phase":"end","pid":7,"plaintext_len":1024,"seq":1}
''';

void main() {
  test('pairs start and end lines by pid and seq', () {
    final records = parseNseLog(_okPair);
    expect(records, hasLength(1));
    final r = records.single;
    expect(r.seq, 1);
    expect(r.ok, isTrue);
    expect(r.peakBytes, 6291456);
    expect(r.decryptMicros, 1700);
    expect(r.latencyMs, 500);
    expect(r.stopped, isFalse);
  });

  test('start_without_end_is_reported_as_stopped', () {
    final records = parseNseLog('{"phase":"start","pid":9,"seq":4,"sent_at":1,"received_at":2,"footprint":1}\n');
    expect(records.single.stopped, isTrue);
    expect(records.single.ok, isNull);
  });

  test('garbage_lines_are_skipped', () {
    final records = parseNseLog('not json\n[1,2]\n{"phase":"start"}\n{"phase":"end","pid":"x","seq":1}\n$_okPair{"phase":"st');
    expect(records, hasLength(1));
    expect(records.single.seq, 1);
  });

  test('a push without ciphertext is a baseline record', () {
    const log = '{"phase":"start","pid":3,"seq":2,"sent_at":10,"received_at":20,"footprint":1}\n'
        '{"phase":"end","pid":3,"seq":2,"ok":false,"error":"noCiphertext","footprint":2,"peak":4194304,"available":0}\n';
    final r = parseNseLog(log).single;
    expect(r.baseline, isTrue);
    expect(r.ok, isFalse);
  });

  test('an unknown peak (-1) and a missing sent_at give nulls', () {
    const log = '{"phase":"start","pid":3,"seq":5,"sent_at":-1,"received_at":20,"footprint":1}\n'
        '{"phase":"end","pid":3,"seq":5,"ok":true,"footprint":2,"peak":-1,"available":0}\n';
    final r = parseNseLog(log).single;
    expect(r.peakBytes, isNull);
    expect(r.latencyMs, isNull);
  });
}
```

Run: `cd /c/ChatProject/app && flutter test test/nse_record_test.dart`
Expected: 컴파일 오류. `Error when reading 'lib/probe/nse_record.dart'`, `Method not found: 'parseNseLog'`

- [ ] **Step 2: 기록 해석을 구현한다**

`app/lib/probe/nse_record.dart`:

```dart
import 'dart:convert';

/// One notification as the M1b NSE probe recorded it in `nse_memory.jsonl` (App Group):
/// a `start` line, then an `end` line unless iOS stopped the extension first.
class NseRecord {
  const NseRecord({
    required this.seq,
    required this.pid,
    required this.sentAtMs,
    required this.receivedAtMs,
    this.ok,
    this.error,
    this.peakBytes,
    this.loadMicros,
    this.decryptMicros,
  });

  final int seq;
  final int pid;
  final int sentAtMs;
  final int receivedAtMs;

  /// Null when there is no `end` line: the extension stopped before it finished.
  final bool? ok;
  final String? error;
  final int? peakBytes;
  final int? loadMicros;
  final int? decryptMicros;

  bool get stopped => ok == null;

  /// A push without ciphertext: the extension's memory without MLS work.
  bool get baseline => error == 'noCiphertext';

  int? get latencyMs => sentAtMs > 0 && receivedAtMs > 0 ? receivedAtMs - sentAtMs : null;
}

/// Pairs `start` and `end` lines by (pid, seq), in the order the starts appear.
/// Lines that are not JSON objects with integer `pid` and `seq` are skipped.
List<NseRecord> parseNseLog(String text) {
  final starts = <(int, int), Map<String, dynamic>>{};
  final ends = <(int, int), Map<String, dynamic>>{};
  final order = <(int, int)>[];
  for (final line in const LineSplitter().convert(text)) {
    Object? decoded;
    try {
      decoded = jsonDecode(line);
    } on FormatException {
      continue;
    }
    if (decoded is! Map<String, dynamic>) continue;
    final pid = decoded['pid'];
    final seq = decoded['seq'];
    if (pid is! int || seq is! int) continue;
    final key = (pid, seq);
    switch (decoded['phase']) {
      case 'start':
        if (!starts.containsKey(key)) order.add(key);
        starts[key] = decoded;
      case 'end':
        ends[key] = decoded;
    }
  }
  return [for (final key in order) _record(starts[key]!, ends[key])];
}

NseRecord _record(Map<String, dynamic> start, Map<String, dynamic>? end) => NseRecord(
      seq: start['seq'] as int,
      pid: start['pid'] as int,
      sentAtMs: _int(start['sent_at']) ?? -1,
      receivedAtMs: _int(start['received_at']) ?? -1,
      ok: end == null ? null : end['ok'] == true,
      error: end?['error'] is String ? end!['error'] as String : null,
      peakBytes: _positive(end?['peak']),
      loadMicros: _int(end?['load_us']),
      decryptMicros: _int(end?['decrypt_us']),
    );

int? _int(Object? value) => value is int ? value : null;

int? _positive(Object? value) => value is int && value > 0 ? value : null;
```

Run: `cd /c/ChatProject/app && flutter test test/nse_record_test.dart`
Expected: `+5: All tests passed!`

- [ ] **Step 3: 측정 화면 시험을 쓴다**

`app/test/probe_screen_test.dart`:

```dart
import 'package:chat_app/probe/probe_host.dart';
import 'package:chat_app/probe/probe_screen.dart';
import 'package:chat_app/theme/app_theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

class FakeProbeHost implements ProbeHost {
  FakeProbeHost({this.log, this.allow = true});

  String? token;
  String? log;
  final bool allow;
  bool cleared = false;

  @override
  Future<bool> requestPush() async {
    if (allow) token = 'a1b2c3';
    return allow;
  }

  @override
  Future<String?> deviceToken() async => token;

  @override
  Future<String?> readNseLog() async => log;

  @override
  Future<void> clearNseLog() async {
    cleared = true;
    log = null;
  }
}

const _log = '''
{"phase":"start","pid":7,"seq":1,"sent_at":1000,"received_at":1400,"footprint":1}
{"phase":"end","pid":7,"seq":1,"ok":false,"error":"noCiphertext","footprint":1,"peak":4194304,"available":0}
{"phase":"start","pid":7,"seq":2,"sent_at":2000,"received_at":2300,"footprint":1}
{"phase":"end","pid":7,"seq":2,"ok":true,"plaintext_len":1024,"load_us":3700,"decrypt_us":1700,"footprint":1,"peak":6291456,"available":0}
{"phase":"start","pid":8,"seq":3,"sent_at":3000,"received_at":3100,"footprint":1}
''';

Future<void> pumpProbe(WidgetTester tester, FakeProbeHost host) async {
  await tester.pumpWidget(MaterialApp(theme: buildAppTheme(), home: ProbeScreen(host: host)));
  await tester.pumpAndSettle();
}

void main() {
  testWidgets('shows empty state without records', (tester) async {
    await pumpProbe(tester, FakeProbeHost());
    expect(find.text('아직 기록이 없어요'), findsOneWidget);
    expect(find.text('알림 허용하고 토큰 받기'), findsOneWidget);
  });

  testWidgets('allowing notifications shows the token', (tester) async {
    await pumpProbe(tester, FakeProbeHost());
    await tester.tap(find.text('알림 허용하고 토큰 받기'));
    await tester.pumpAndSettle();
    expect(find.text('a1b2c3'), findsOneWidget);
    expect(find.text('토큰 복사'), findsOneWidget);
  });

  testWidgets('a denied permission says how to turn it on', (tester) async {
    await pumpProbe(tester, FakeProbeHost(allow: false));
    await tester.tap(find.text('알림 허용하고 토큰 받기'));
    await tester.pumpAndSettle();
    expect(find.text('알림이 꺼져 있어요. 설정 앱에서 허용해 주세요.'), findsOneWidget);
  });

  testWidgets('shows the largest peak and one line per notification', (tester) async {
    await pumpProbe(tester, FakeProbeHost(log: _log));
    expect(find.text('가장 큰 피크 6.0 MB · 기준 15 MB 미만'), findsOneWidget);
    expect(find.text('#1 · 기준(복호 없음) · 피크 4.0 MB · 지연 400 ms'), findsOneWidget);
    expect(find.text('#2 · 복호 성공 · 피크 6.0 MB · 복호 1.7 ms · 지연 300 ms'), findsOneWidget);
    expect(find.text('#3 · 끝 기록 없음 (중단됐을 수 있어요)'), findsOneWidget);
  });

  testWidgets('clearing the log empties the list', (tester) async {
    final host = FakeProbeHost(log: _log);
    await pumpProbe(tester, host);
    await tester.tap(find.text('기록 지우기'));
    await tester.pumpAndSettle();
    expect(host.cleared, isTrue);
    expect(find.text('아직 기록이 없어요'), findsOneWidget);
  });
}
```

`app/test/home_screen_test.dart`의 `main()` 끝에 시험 하나를 더한다. 맨 위 import에 `import 'package:chat_app/probe/probe_host.dart';`와 `import 'probe_screen_test.dart' show FakeProbeHost;`를 더한다.

```dart
  testWidgets('the probe button appears only with a probe host and opens the probe screen', (tester) async {
    await pumpHome(tester);
    expect(find.byTooltip('알림 측정'), findsNothing);

    final ProbeHost probe = FakeProbeHost();
    await tester.pumpWidget(MaterialApp(
      theme: buildAppTheme(),
      home: HomeScreen(core: FakeCoreClient(), probe: probe),
    ));
    await tester.pump();
    await tester.tap(find.byTooltip('알림 측정'));
    await tester.pumpAndSettle();
    expect(find.text('알림 측정'), findsOneWidget);
  });
```

Run: `cd /c/ChatProject/app && flutter test test/probe_screen_test.dart test/home_screen_test.dart`
Expected: 컴파일 오류. `probe_host.dart`와 `probe_screen.dart`가 없고, `HomeScreen`에 `probe` 매개변수가 없다.

- [ ] **Step 4: 화면과 플랫폼 연결을 구현한다**

`app/pubspec.yaml`의 `dependencies`에 `path_provider_foundation`을 직접 의존성으로 더한다. App Group 폴더를 읽는 `getContainerPath`가 이 패키지에만 있다.

```bash
cd /c/ChatProject/app && flutter pub add path_provider_foundation:^2.6.0
```

`app/lib/probe/probe_host.dart`:

```dart
import 'dart:io';

import 'package:flutter/services.dart';
import 'package:path_provider_foundation/path_provider_foundation.dart';

/// What the M1b probe screen needs from the platform. iOS only; widget tests use a fake.
abstract class ProbeHost {
  /// Asks for notification permission, then registers for a device token.
  /// Returns whether notifications are allowed.
  Future<bool> requestPush();

  /// Hex APNs device token, or null until iOS has delivered one.
  Future<String?> deviceToken();

  /// The NSE log (`nse_memory.jsonl`), or null when it does not exist yet.
  Future<String?> readNseLog();

  Future<void> clearNseLog();
}

class IosProbeHost implements ProbeHost {
  static const appGroup = 'group.dev.chatproject.chatapp';
  static const _channel = MethodChannel('chat_app/ios_probe');

  @override
  Future<bool> requestPush() async => await _channel.invokeMethod<bool>('requestPush') ?? false;

  @override
  Future<String?> deviceToken() => _channel.invokeMethod<String>('deviceToken');

  Future<File?> _log() async {
    final dir = await PathProviderFoundation().getContainerPath(appGroupIdentifier: appGroup);
    return dir == null ? null : File('$dir/nse_memory.jsonl');
  }

  @override
  Future<String?> readNseLog() async {
    final file = await _log();
    if (file == null || !await file.exists()) return null;
    return file.readAsString();
  }

  @override
  Future<void> clearNseLog() async {
    final file = await _log();
    if (file != null && await file.exists()) await file.delete();
  }
}
```

`app/lib/probe/probe_screen.dart`:

```dart
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../theme/tokens.dart';
import 'nse_record.dart';
import 'probe_host.dart';

/// M1b device measurements: the APNs token to send to, and what the NSE recorded.
/// Lives only until M7 replaces the probe with the real extension.
class ProbeScreen extends StatefulWidget {
  const ProbeScreen({super.key, required this.host});

  final ProbeHost host;

  @override
  State<ProbeScreen> createState() => _ProbeScreenState();
}

String _mb(int bytes) => (bytes / 1048576).toStringAsFixed(1);

/// One line per notification.
String describeRecord(NseRecord r) {
  final latency = r.latencyMs == null ? '' : ' · 지연 ${r.latencyMs} ms';
  final peak = r.peakBytes == null ? '' : ' · 피크 ${_mb(r.peakBytes!)} MB';
  if (r.stopped) return '#${r.seq} · 끝 기록 없음 (중단됐을 수 있어요)';
  if (r.baseline) return '#${r.seq} · 기준(복호 없음)$peak$latency';
  if (r.ok == true) {
    final decrypt = r.decryptMicros == null ? '' : ' · 복호 ${(r.decryptMicros! / 1000).toStringAsFixed(1)} ms';
    return '#${r.seq} · 복호 성공$peak$decrypt$latency';
  }
  return '#${r.seq} · 복호 실패 · ${r.error ?? '이유 없음'}';
}

class _ProbeScreenState extends State<ProbeScreen> {
  String? _token;
  bool? _allowed;
  List<NseRecord> _records = const [];

  @override
  void initState() {
    super.initState();
    _refresh();
  }

  Future<void> _refresh() async {
    final token = await widget.host.deviceToken();
    final log = await widget.host.readNseLog();
    if (!mounted) return;
    setState(() {
      _token = token;
      _records = log == null ? const [] : parseNseLog(log);
    });
  }

  Future<void> _requestPush() async {
    final allowed = await widget.host.requestPush();
    if (!mounted) return;
    setState(() => _allowed = allowed);
    await _refresh();
  }

  Future<void> _clear() async {
    await widget.host.clearNseLog();
    await _refresh();
  }

  @override
  Widget build(BuildContext context) {
    final peaks = [for (final r in _records) if (r.peakBytes != null) r.peakBytes!];
    final token = _token;
    return Scaffold(
      appBar: AppBar(title: const Text('알림 측정')),
      body: ListView(
        padding: const EdgeInsets.fromLTRB(22, 16, 22, 16),
        children: [
          const Text('알림 토큰', style: TextStyle(fontSize: 18, fontWeight: FontWeight.w600, color: AppColors.text)),
          const SizedBox(height: 8),
          if (token == null)
            FilledButton(onPressed: _requestPush, child: const Text('알림 허용하고 토큰 받기'))
          else ...[
            SelectableText(token, style: const TextStyle(fontSize: 13, color: AppColors.textSecondary)),
            TextButton(
              onPressed: () => Clipboard.setData(ClipboardData(text: token)),
              child: const Text('토큰 복사'),
            ),
          ],
          if (_allowed == false)
            const Text('알림이 꺼져 있어요. 설정 앱에서 허용해 주세요.', style: TextStyle(color: AppColors.textSecondary)),
          const SizedBox(height: 24),
          const Text('NSE 기록', style: TextStyle(fontSize: 18, fontWeight: FontWeight.w600, color: AppColors.text)),
          Row(children: [
            TextButton(onPressed: _refresh, child: const Text('새로고침')),
            TextButton(onPressed: _clear, child: const Text('기록 지우기')),
          ]),
          if (_records.isEmpty)
            const Text('아직 기록이 없어요', style: TextStyle(color: AppColors.textSecondary))
          else ...[
            if (peaks.isNotEmpty)
              Text(
                '가장 큰 피크 ${_mb(peaks.reduce((a, b) => a > b ? a : b))} MB · 기준 15 MB 미만',
                style: const TextStyle(fontWeight: FontWeight.w600, color: AppColors.text),
              ),
            const SizedBox(height: 8),
            for (final r in _records)
              Padding(
                padding: const EdgeInsets.symmetric(vertical: 4),
                child: Text(describeRecord(r), style: const TextStyle(fontSize: 14, color: AppColors.textSecondary)),
              ),
          ],
        ],
      ),
    );
  }
}
```

`app/lib/home/home_screen.dart`:
- import에 `import '../probe/probe_host.dart';`와 `import '../probe/probe_screen.dart';`를 더한다.
- 생성자와 필드:

```dart
  const HomeScreen({super.key, required this.core, this.probe});

  final CoreClient core;

  /// M1b device probe (iOS only). Null hides the probe button.
  final ProbeHost? probe;
```

- `build`에서 `'대화'` 제목 `Text` 위젯을 다음 `Row`로 감싼다:

```dart
              Row(
                children: [
                  const Text(
                    '대화',
                    style: TextStyle(fontSize: 30, fontWeight: FontWeight.w700, color: AppColors.text),
                  ),
                  const Spacer(),
                  if (widget.probe != null)
                    IconButton(
                      tooltip: '알림 측정',
                      icon: const Icon(Icons.science_outlined, color: AppColors.textSecondary),
                      onPressed: () => Navigator.of(context).push(
                        MaterialPageRoute<void>(builder: (_) => ProbeScreen(host: widget.probe!)),
                      ),
                    ),
                ],
              ),
```

`app/lib/main.dart`:
- import에 `import 'dart:io';`와 `import 'probe/probe_host.dart';`를 더한다.
- `runApp(ChatApp(core: FrbCoreClient()));` → `runApp(ChatApp(core: FrbCoreClient(), probe: Platform.isIOS ? IosProbeHost() : null));`
- `ChatApp`:

```dart
class ChatApp extends StatelessWidget {
  const ChatApp({super.key, required this.core, this.probe});

  final CoreClient core;
  final ProbeHost? probe;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'ChatProject',
      theme: buildAppTheme(),
      home: HomeScreen(core: core, probe: probe),
    );
  }
}
```

`app/ios/Runner/AppDelegate.swift` 전체:

```swift
import Flutter
import UIKit
import UserNotifications

@main
@objc class AppDelegate: FlutterAppDelegate, FlutterImplicitEngineDelegate {
  /// Hex APNs device token for the M1b probe screen, set once iOS registers the app.
  private var apnsToken: String?

  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    return super.application(application, didFinishLaunchingWithOptions: launchOptions)
  }

  func didInitializeImplicitFlutterEngine(_ engineBridge: FlutterImplicitEngineBridge) {
    GeneratedPluginRegistrant.register(with: engineBridge.pluginRegistry)
    guard let registrar = engineBridge.pluginRegistry.registrar(forPlugin: "ChatProbe") else { return }
    let channel = FlutterMethodChannel(name: "chat_app/ios_probe", binaryMessenger: registrar.messenger())
    channel.setMethodCallHandler { [weak self] call, result in
      switch call.method {
      case "requestPush":
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) { granted, _ in
          DispatchQueue.main.async {
            if granted { UIApplication.shared.registerForRemoteNotifications() }
            result(granted)
          }
        }
      case "deviceToken":
        result(self?.apnsToken)
      default:
        result(FlutterMethodNotImplemented)
      }
    }
  }

  override func application(
    _ application: UIApplication,
    didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
  ) {
    apnsToken = deviceToken.map { String(format: "%02x", $0) }.joined()
    super.application(application, didRegisterForRemoteNotificationsWithDeviceToken: deviceToken)
  }
}
```

알림 권한은 설계 8.6에 따라 M7부터 복구 문구 확인 직후에 묻는다. M1b에서는 측정 화면의 단추로만 묻는다.

- [ ] **Step 5: 시험과 분석**

Run: `cd /c/ChatProject/app && flutter test && flutter analyze`
Expected:
- `flutter test`: 위젯 시험 전체가 통과한다. M1a의 6개 + 기록 5개 + 화면 5개 + 홈 1개 = 17개(`+17: All tests passed!`).
- `flutter analyze`: `No issues found!`

- [ ] **Step 6: 커밋하고 CI를 본다**

```bash
cd /c/ChatProject
git add app/lib app/test app/ios/Runner/AppDelegate.swift app/pubspec.yaml app/pubspec.lock
git commit -m "feat(app): probe screen for the APNs token and the NSE memory records"
git push
```

CI 확인:
- `flutter` 작업에서 17개가 통과한다.
- `ios` 작업이 녹색이다. `AppDelegate.swift`가 컴파일되고 시뮬레이터 통합 테스트도 통과한다.

### Task 8: 서명과 TestFlight 업로드

**Files:**
- Modify: `app/ios/scripts/add_notification_service.rb` (서명 설정)
- Create: `.github/workflows/testflight.yml`
- Modify: `tools/ci/check_workflows.py`
- Create: `tools/ci/test_check_workflows.py`

**Interfaces:**
- Consumes: Task 6의 스크립트와 NSE, Task 1의 Environment `testflight`, Task 2의 검사 스크립트.
- Produces:
  - `testflight` 워크플로. `workflow_dispatch`, 또는 `testflight-*` 태그 push로 시작한다.
  - 빌드 번호는 `github.run_number`다.
  - 성공하면 App Store Connect에 빌드 하나가 올라간다.

- [ ] **Step 1: Apple 쪽 준비 (사용자)**

developer.apple.com → Certificates, Identifiers & Profiles:
1. **App ID 2개.** Identifiers → + → App IDs → App → Explicit.
   - `dev.chatproject.chatapp`: Push Notifications, App Groups를 켠다.
   - `dev.chatproject.chatapp.NotificationService`: App Groups만 켠다.
2. **App Group.** Identifiers → + → App Groups → `group.dev.chatproject.chatapp`. 위의 두 App ID에서 App Groups → Configure로 이 그룹을 고른다.
3. **프로파일 2개.** Profiles → + → Distribution → App Store Connect.
   - 각 App ID와 기존 Apple Distribution 인증서를 고른다.
   - 이름은 정확히 `ChatProject App Store`, `ChatProject NSE App Store`로 짓는다.
   - 1·2를 끝낸 뒤에 만든다. 그 전에 만든 프로파일에는 그룹이 빠진다.
4. **APNs 키.** Keys → + → Apple Push Notifications service.
   - 환경은 **Production**, 범위는 팀 전체(Team Scoped)다.
   - `.p8`을 한 번만 내려받을 수 있다. 받은 파일은 `spikes/c-push/secrets/`에 둔다(무시되는 폴더).
   - Key ID를 적어 둔다.

App Store Connect:

5. **앱 레코드.** 앱 → + → 신규 앱.
   - 플랫폼 iOS, 번들 ID `dev.chatproject.chatapp`. SKU는 아무 값이나 넣는다(예: `chatproject-1`).
   - 이름은 App Store 전체에서 겹치면 안 된다. 겹치면 임시 이름을 쓰고, 첫 심사 전에 바꾼다.
6. **API 키.** 사용자 및 액세스 → 통합 → App Store Connect API → 팀 키를 만든다.
   - 역할은 **App Manager**다.
   - `.p8`을 내려받고 Issuer ID와 Key ID를 적는다.
   - BeanProfile에서 만든 같은 팀의 키가 있으면 그대로 써도 된다.

GitHub Environment `testflight`에 시크릿 7개를 넣는다(Git Bash). 값은 화면에 나오지 않는다.

```bash
R=HyunwookYoo/chatproject
base64 -w0 /c/path/to/distribution.p12 | gh secret set APPSTORE_CERTIFICATES_FILE_BASE64 --env testflight -R $R
gh secret set APPSTORE_CERTIFICATES_PASSWORD --env testflight -R $R          # 프롬프트에 붙여 넣는다
base64 -w0 "/c/path/to/ChatProject_App_Store.mobileprovision" | gh secret set APPSTORE_PROFILE_APP_BASE64 --env testflight -R $R
base64 -w0 "/c/path/to/ChatProject_NSE_App_Store.mobileprovision" | gh secret set APPSTORE_PROFILE_NSE_BASE64 --env testflight -R $R
gh secret set APPSTORE_ISSUER_ID --env testflight -R $R
gh secret set APPSTORE_API_KEY_ID --env testflight -R $R
gh secret set APPSTORE_API_PRIVATE_KEY --env testflight -R $R < /c/path/to/AuthKey_XXXXXXXXXX.p8
gh secret list --env testflight -R $R    # 이름 7개만 보인다
```

`--body` 옵션은 쓰지 않는다. 값이 셸 기록에 남기 때문이다.

- [ ] **Step 2: 워크플로 검사에 TestFlight 규칙을 더하고 시험을 쓴다**

`tools/ci/test_check_workflows.py`:

```python
import pathlib
import unittest

from check_workflows import check

PINNED = "apple-actions/upload-testflight-build@14df18fee5a4ff3b76971dd382f033494a0194e9"


def testflight(**overrides):
    doc = {
        "on": {"workflow_dispatch": None, "push": {"tags": ["testflight-*"]}},
        "permissions": {"contents": "read"},
        "env": {"RUST_TOOLCHAIN": "1.92.0", "CARGOKIT_TOOLCHAIN": "1.92.0"},
        "jobs": {"testflight": {
            "environment": "testflight",
            "steps": [
                {"name": "Refuse re-runs", "run": 'test "$GITHUB_RUN_ATTEMPT" = 1'},
                {"name": "Install provisioning profiles", "run": "ProvisionedDevices application-groups aps-environment"},
                {"name": "Upload to TestFlight", "uses": PINNED},
            ],
        }},
    }
    doc.update(overrides)
    return doc


class CheckTest(unittest.TestCase):
    path = pathlib.Path("testflight.yml")

    def test_a_good_testflight_workflow_passes(self):
        self.assertEqual(check(self.path, testflight()), [])

    def test_pull_request_trigger_is_rejected(self):
        problems = check(self.path, testflight(on={"pull_request": None}))
        self.assertTrue(any("triggers" in p for p in problems), problems)

    def test_missing_environment_is_rejected(self):
        doc = testflight()
        del doc["jobs"]["testflight"]["environment"]
        self.assertTrue(any("environment" in p for p in check(self.path, doc)))

    def test_missing_rerun_guard_is_rejected(self):
        doc = testflight()
        doc["jobs"]["testflight"]["steps"].pop(0)
        self.assertTrue(any("Refuse re-runs" in p for p in check(self.path, doc)))

    def test_artifact_upload_is_rejected(self):
        doc = testflight()
        doc["jobs"]["testflight"]["steps"].append(
            {"uses": "actions/upload-artifact@0123456789012345678901234567890123456789"})
        self.assertTrue(any("upload-artifact" in p for p in check(self.path, doc)))

    def test_unpinned_action_is_rejected(self):
        doc = testflight()
        doc["jobs"]["testflight"]["steps"][2]["uses"] = "apple-actions/upload-testflight-build@v5"
        self.assertTrue(any("not pinned" in p for p in check(self.path, doc)))


if __name__ == "__main__":
    unittest.main()
```

Run: `cd /c/ChatProject/tools/ci && python -m unittest test_check_workflows -v`
Expected: `test_a_good_testflight_workflow_passes`와 `test_unpinned_action_is_rejected`는 통과하고, 나머지 넷은 실패한다(아직 TestFlight 규칙이 없다).

`tools/ci/check_workflows.py`의 `check()`에서 `return problems` 바로 위에 더한다:

```python
    if name == "testflight.yml":
        problems += check_testflight(name, doc)
```

`check()` 아래에 함수를 더한다:

```python
def check_testflight(name, doc):
    """The release job holds the App Store signing secrets (Review Focus 5)."""
    problems = []
    if set(triggers(doc)) - {"workflow_dispatch", "push"}:
        problems.append(f"{name}: triggers must be workflow_dispatch and push (tags) only")
    push = triggers(doc).get("push") or {}
    if push and (set(push) != {"tags"} or push["tags"] != ["testflight-*"]):
        problems.append(f"{name}: push may only be for tags testflight-*")
    for job_id, job in (doc.get("jobs") or {}).items():
        if job.get("environment") != "testflight":
            problems.append(f"{name}: job {job_id} must use environment: testflight")
        step_list = job.get("steps") or []
        names = [step.get("name", "") for step in step_list]
        if not names or names[0] != "Refuse re-runs":
            problems.append(f"{name}: job {job_id} must start with the 'Refuse re-runs' step")
        profiles = next((s for s in step_list if s.get("name") == "Install provisioning profiles"), None)
        if profiles is None or not all(k in profiles.get("run", "") for k in
                                        ("ProvisionedDevices", "application-groups", "aps-environment")):
            problems.append(f"{name}: job {job_id} must validate the profiles before signing")
        for step in step_list:
            if "upload-artifact" in step.get("uses", ""):
                problems.append(f"{name}: job {job_id} must not upload-artifact (IPA and profiles stay private)")
    return problems
```

Run: `cd /c/ChatProject/tools/ci && python -m unittest test_check_workflows -v`
Expected: `Ran 6 tests ... OK`

- [ ] **Step 3: 스크립트에 서명 설정을 더한다**

`app/ios/scripts/add_notification_service.rb`에서 `# Runner: App Group + push entitlements.` 블록 다음, `if changed || project.dirty?` 위에 더한다:

```ruby
# Signing (M1b plan, decision 3). Debug and Profile sign automatically (simulator builds need
# no profile). Release signs manually with the App Store profiles that the testflight workflow
# installs; without Manual, archive falls back to automatic signing and fails with "No Accounts".
TEAM = '9J2FNH63M2'
PROFILES = { 'Runner' => 'ChatProject App Store', NSE => 'ChatProject NSE App Store' }.freeze
[runner, nse].each do |target|
  CONFIGS.each do |name|
    config = target.build_configuration_list[name]
    set.(config, 'DEVELOPMENT_TEAM', TEAM)
    if name == 'Release'
      set.(config, 'CODE_SIGN_STYLE', 'Manual')
      set.(config, 'CODE_SIGN_IDENTITY', 'Apple Distribution')
      set.(config, 'CODE_SIGN_IDENTITY[sdk=iphoneos*]', 'Apple Distribution')
      set.(config, 'PROVISIONING_PROFILE_SPECIFIER', PROFILES.fetch(target.name))
    else
      set.(config, 'CODE_SIGN_STYLE', 'Automatic')
    end
  end
end
```

- [ ] **Step 4: TestFlight 워크플로를 쓴다**

`.github/workflows/testflight.yml`:

```yaml
name: testflight

# Signed build -> App Store Connect. Start it with a `testflight-<n>` tag on a pushed commit,
# or from the Actions tab once the workflow is on main. The secrets live in the `testflight`
# Environment, which needs the owner's approval for every run.
on:
  workflow_dispatch:
  push:
    tags: ['testflight-*']

permissions:
  contents: read

concurrency:
  group: testflight
  cancel-in-progress: false

env:
  FLUTTER_VERSION: 3.44.6
  RUST_TOOLCHAIN: 1.92.0
  # cargokit (app/rust_builder) builds with this toolchain. Must equal RUST_TOOLCHAIN.
  CARGOKIT_TOOLCHAIN: 1.92.0
  XCODE: /Applications/Xcode_26.6.app
  APP_ID: dev.chatproject.chatapp
  NSE_ID: dev.chatproject.chatapp.NotificationService
  APP_GROUP: group.dev.chatproject.chatapp
  # Must equal PROVISIONING_PROFILE_SPECIFIER in app/ios/scripts/add_notification_service.rb.
  APP_PROFILE_NAME: ChatProject App Store
  NSE_PROFILE_NAME: ChatProject NSE App Store

jobs:
  testflight:
    runs-on: macos-26
    environment: testflight
    timeout-minutes: 90
    steps:
      - name: Refuse re-runs
        run: |
          if [ "$GITHUB_RUN_ATTEMPT" != 1 ]; then
            echo "::error::A re-run reuses build number $GITHUB_RUN_NUMBER, which App Store Connect rejects. Start a new run instead."
            exit 1
          fi

      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
        with:
          persist-credentials: false

      - name: Pin Xcode
        run: |
          sudo xcode-select -s "$XCODE/Contents/Developer"
          xcodebuild -version

      - uses: dtolnay/rust-toolchain@7e38f4b43b4db5c8dd498af069a4f6196df1d067 # master 2026-10-01
        with:
          toolchain: ${{ env.RUST_TOOLCHAIN }}
          targets: aarch64-apple-ios

      - uses: subosito/flutter-action@1a449444c387b1966244ae4d4f8c696479add0b2 # v2.23.0
        with:
          flutter-version: ${{ env.FLUTTER_VERSION }}
          channel: stable

      - name: Rust library for the notification extension
        run: cargo build -p chat_nse --release --target aarch64-apple-ios

      - name: Add the notification extension target
        run: |
          gem install xcodeproj -v 1.28.1 --no-document
          ruby app/ios/scripts/add_notification_service.rb app/ios/Runner.xcodeproj

      - name: Import distribution certificate
        uses: apple-actions/import-codesign-certs@5142e029c445c10ffc7149d172e540235a065466 # v7.0.0
        with:
          p12-file-base64: ${{ secrets.APPSTORE_CERTIFICATES_FILE_BASE64 }}
          p12-password: ${{ secrets.APPSTORE_CERTIFICATES_PASSWORD }}

      - name: Keychain timeout and identity check
        run: |
          security set-keychain-settings -lut 21600 signing_temp.keychain
          ids="$(security find-identity -v -p codesigning)"; echo "$ids"
          case "$ids" in
            *"Apple Distribution"*) ;;
            *) echo "::error::no usable Apple Distribution identity (is the private key inside the .p12?)"; exit 1 ;;
          esac

      - name: Install provisioning profiles
        env:
          APP_PROFILE_B64: ${{ secrets.APPSTORE_PROFILE_APP_BASE64 }}
          NSE_PROFILE_B64: ${{ secrets.APPSTORE_PROFILE_NSE_BASE64 }}
        run: |
          set -euo pipefail
          legacy="$HOME/Library/MobileDevice/Provisioning Profiles"
          xcode16="$HOME/Library/Developer/Xcode/UserData/Provisioning Profiles"
          mkdir -p "$legacy" "$xcode16"
          pb=/usr/libexec/PlistBuddy

          install_profile() { # label base64 bundle-id expected-name env-prefix
            local f="$RUNNER_TEMP/$1.mobileprovision" p="$RUNNER_TEMP/$1.plist"
            local uuid name team appid groups
            printf '%s' "$2" | base64 --decode > "$f"
            security cms -D -i "$f" > "$p"
            uuid=$($pb -c 'Print :UUID' "$p")
            name=$($pb -c 'Print :Name' "$p")
            team=$($pb -c 'Print :TeamIdentifier:0' "$p")
            appid=$($pb -c 'Print :Entitlements:application-identifier' "$p")
            groups=$($pb -c 'Print :Entitlements:com.apple.security.application-groups' "$p" 2>/dev/null || true)
            [ "$appid" = "$team.$3" ] || { echo "::error::$1 profile is for $appid, expected $team.$3"; exit 1; }
            [ "$name" = "$4" ] || { echo "::error::$1 profile is named '$name', expected '$4'"; exit 1; }
            if $pb -c 'Print :ProvisionedDevices' "$p" >/dev/null 2>&1; then
              echo "::error::$1 is not an App Store profile"; exit 1
            fi
            case "$groups" in
              *"$APP_GROUP"*) ;;
              *) echo "::error::$1 profile lacks $APP_GROUP; enable App Groups on the App ID, then regenerate it"; exit 1 ;;
            esac
            cp "$f" "$legacy/$uuid.mobileprovision"
            cp "$f" "$xcode16/$uuid.mobileprovision"
            echo "$5_PROFILE_UUID=$uuid" >> "$GITHUB_ENV"
            echo "TEAM_ID=$team" >> "$GITHUB_ENV"
          }

          install_profile app "$APP_PROFILE_B64" "$APP_ID" "$APP_PROFILE_NAME" APP
          install_profile nse "$NSE_PROFILE_B64" "$NSE_ID" "$NSE_PROFILE_NAME" NSE
          [ "$($pb -c 'Print :Entitlements:aps-environment' "$RUNNER_TEMP/app.plist")" = production ] \
            || { echo "::error::app profile has no production aps-environment"; exit 1; }

      - name: Write ExportOptions.plist
        run: |
          cat > "$RUNNER_TEMP/ExportOptions.plist" <<EOF
          <?xml version="1.0" encoding="UTF-8"?>
          <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
          <plist version="1.0">
          <dict>
            <key>method</key><string>app-store-connect</string>
            <key>destination</key><string>export</string>
            <key>signingStyle</key><string>manual</string>
            <key>signingCertificate</key><string>Apple Distribution</string>
            <key>teamID</key><string>${TEAM_ID}</string>
            <key>provisioningProfiles</key>
            <dict>
              <key>${APP_ID}</key><string>${APP_PROFILE_UUID}</string>
              <key>${NSE_ID}</key><string>${NSE_PROFILE_UUID}</string>
            </dict>
            <key>manageAppVersionAndBuildNumber</key><false/>
          </dict>
          </plist>
          EOF
          plutil -lint "$RUNNER_TEMP/ExportOptions.plist"

      - name: Build IPA
        working-directory: app
        run: |
          flutter pub get
          flutter build ipa --release \
            --build-number=${{ github.run_number }} \
            --export-options-plist="$RUNNER_TEMP/ExportOptions.plist"

      # flutter build ipa exits 0 even when `xcodebuild -exportArchive` fails, so check the IPA.
      - name: Locate IPA and check its contents
        id: ipa
        run: |
          set -euo pipefail
          ipa="$(find app/build/ios/ipa -maxdepth 1 -name '*.ipa' -print -quit)"
          test -n "$ipa" || { echo "::error::no IPA was exported"; exit 1; }
          unzip -q "$ipa" -d "$RUNNER_TEMP/ipa"
          app_dir="$(find "$RUNNER_TEMP/ipa/Payload" -maxdepth 1 -name '*.app' -print -quit)"
          got="$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$app_dir"/PlugIns/*.appex/Info.plist)"
          [ "$got" = "$NSE_ID" ] || { echo "::error::embedded extension is '$got'"; exit 1; }
          nm -gU "$app_dir/Frameworks/chat_ffi.framework/chat_ffi" | grep -q frb_get_rust_content_hash \
            || { echo "::error::chat_ffi lost its flutter_rust_bridge symbols in the release build"; exit 1; }
          echo "path=$ipa" >> "$GITHUB_OUTPUT"

      - name: Upload to TestFlight
        uses: apple-actions/upload-testflight-build@14df18fee5a4ff3b76971dd382f033494a0194e9 # v5.5.0
        with:
          app-path: ${{ steps.ipa.outputs.path }}
          app-type: ios
          issuer-id: ${{ secrets.APPSTORE_ISSUER_ID }}
          api-key-id: ${{ secrets.APPSTORE_API_KEY_ID }}
          api-private-key: ${{ secrets.APPSTORE_API_PRIVATE_KEY }}
```

Run: `cd /c/ChatProject && python tools/ci/check_workflows.py && (cd tools/ci && python -m unittest discover -p 'test_*.py')`
Expected: `workflows ok`, `OK`

- [ ] **Step 5: 커밋하고 push한다**

```bash
cd /c/ChatProject
git add app/ios/scripts/add_notification_service.rb .github/workflows/testflight.yml tools/ci/check_workflows.py tools/ci/test_check_workflows.py
git commit -m "ci: signed TestFlight upload with the app and the notification extension"
git push
```

CI 확인:
- `workflows`가 녹색이다(검사 시험 포함).
- `ios`도 녹색이다. 서명 설정이 시뮬레이터 Debug 빌드를 막지 않는다는 뜻이다.

- [ ] **Step 6: 첫 TestFlight 실행 (사용자와 함께)**

```bash
cd /c/ChatProject
git tag testflight-1 && git push origin testflight-1
```

1. 사용자가 Actions 탭에서 `testflight` 실행의 배포 승인(Review deployments → Approve)을 누른다.
2. `gh run watch "$(gh run list --workflow testflight --limit 1 --json databaseId --jq '.[0].databaseId')" --exit-status`로 끝까지 본다.
3. 실패하면 로그의 첫 `::error::`를 고친다.
   - 프로파일 문제면 사용자가 다시 만들고 시크릿을 바꾼다.
   - 코드 문제면 커밋한다.
   - 그다음 **새 태그**(`testflight-2` …)로 다시 시작한다. 재실행은 `Refuse re-runs`가 막는다.
4. 성공하면 사용자가 App Store Connect → TestFlight에서 그 빌드를 연다.
   - "Missing Compliance"가 보이면 수출 규정 질문에 답한다. 답하기 전에는 내부 테스터도 설치할 수 없다.
   - 이 앱은 표준 암호(MLS: X25519, AES-128-GCM, Ed25519)를 쓴다. 답은 사용자가 Apple의 안내를 보고 정한다.
5. iPhone의 TestFlight 앱에서 설치하고 실행한다. 첫 화면(대화, 연결 없음, 코어 버전)과 오른쪽 위 측정 단추가 보이면 이 태스크의 완료 기준을 채운 것이다.

### Task 9: APNs 발송 도구 `apns-probe`

**Files:**
- Create: `spikes/c-push/apns-probe/Cargo.toml`, `spikes/c-push/apns-probe/src/main.rs`, `spikes/c-push/apns-probe/Cargo.lock`(생성됨)
- Modify: 루트 `Cargo.toml`의 `exclude`

**Interfaces:**
- Consumes:
  - Task 4의 `Fixture/ciphertexts.txt`.
  - Task 8 Step 1의 APNs 키(`spikes/c-push/secrets/AuthKey_<KEYID>.p8`).
- Produces: 이 PC에서 도는 명령 하나.

```
apns-probe send --key <p8> --key-id <KEYID> --team-id 9J2FNH63M2 --topic dev.chatproject.chatapp
                --token <hex> [--count N] [--interval-ms M] [--start-seq S]
                [--ciphertexts <ciphertexts.txt>] [--expiration-secs E] [--sandbox]
```

- 알림 `seq`번은 `ciphertexts.txt`의 `((seq-1) mod 줄 수)`번째 줄을 `e`로 싣는다.
- `--ciphertexts`가 없으면 `e` 없이 보낸다. NSE는 이것을 기준값으로 기록한다.
- 결과는 알림마다 한 줄이다: `seq=… bytes=… status=… apns-id=… rtt_ms=… <reason>`. 실패가 하나라도 있으면 종료 코드 1로 끝난다.

워크스페이스 밖에 두는 이유: 다른 스파이크처럼 자기 `Cargo.lock`을 갖고, reqwest·jsonwebtoken 의존성이 앱 빌드에 섞이지 않게 한다.

- [ ] **Step 1: 시험을 먼저 쓴다**

루트 `Cargo.toml`:

```toml
exclude = ["spikes/a-connectivity", "spikes/b-mls-memory", "spikes/c-push/apns-probe"]
```

`spikes/c-push/apns-probe/Cargo.toml`:

```toml
[package]
name = "apns-probe"
version = "0.1.0"
edition = "2024"
rust-version = "1.92"
publish = false

[dependencies]
anyhow = "1"
base64 = "0.23"
clap = { version = "4.6", features = ["derive"] }
# jsonwebtoken >= 10 has no default crypto backend: pick exactly one of rust_crypto / aws_lc_rs.
jsonwebtoken = { version = "11", features = ["rust_crypto"] }
# reqwest 0.13: `rustls-tls` was renamed `rustls`.
reqwest = { version = "0.13", default-features = false, features = ["http2", "rustls"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "time"] }
uuid = { version = "1", features = ["v4"] }

[dev-dependencies]
# Throwaway P-256 key pair for the JWT tests; no secret is committed.
p256 = { version = "0.13", features = ["pem"] }
```

`spikes/c-push/apns-probe/src/main.rs`에 우선 시험 모듈만 둔다:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use p256::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};

    fn throwaway_keypair() -> (String, String) {
        let sk = p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng);
        let private = sk.to_pkcs8_pem(LineEnding::LF).unwrap().to_string();
        let public = sk.public_key().to_public_key_pem(LineEnding::LF).unwrap();
        (private, public)
    }

    #[test]
    fn jwt_header_and_claims_match_apple_doc() {
        let (private, public) = throwaway_keypair();
        let key = EncodingKey::from_ec_pem(private.as_bytes()).unwrap();
        let jwt = make_jwt(&key, "ABC123DEFG", "DEF123GHIJ", 1_700_000_000).unwrap();

        let header = jsonwebtoken::decode_header(&jwt).unwrap();
        assert_eq!(header.alg, Algorithm::ES256);
        assert_eq!(header.kid.as_deref(), Some("ABC123DEFG"));
        assert!(header.typ.is_none());

        let parts: Vec<&str> = jwt.split('.').collect();
        let claims: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
        assert_eq!(claims, json!({ "iss": "DEF123GHIJ", "iat": 1_700_000_000u64 }));

        let mut validation = jsonwebtoken::Validation::new(Algorithm::ES256);
        validation.required_spec_claims.clear();
        validation.validate_exp = false;
        let decoding = jsonwebtoken::DecodingKey::from_ec_pem(public.as_bytes()).unwrap();
        jsonwebtoken::decode::<Value>(&jwt, &decoding, &validation).unwrap();
    }

    #[test]
    fn payload_shape() {
        let p: Value = serde_json::from_slice(&build_payload(7, 1_700_000_000_123, Some(&[1, 2, 3])).unwrap()).unwrap();
        assert_eq!(p["aps"]["mutable-content"], 1);
        assert_eq!(p["aps"]["alert"]["title"], "측정");
        assert_eq!(p["aps"]["alert"]["body"], "알림 7번");
        assert_eq!((p["seq"].as_u64(), p["sent_at"].as_u64()), (Some(7), Some(1_700_000_000_123)));
        assert_eq!(STANDARD.decode(p["e"].as_str().unwrap()).unwrap(), [1, 2, 3]);
        assert!(p["aps"].get("seq").is_none(), "custom keys must be peers of aps");
        let without: Value = serde_json::from_slice(&build_payload(1, 1, None).unwrap()).unwrap();
        assert!(without.get("e").is_none());
    }

    #[test]
    fn payload_size_boundary_is_4096() {
        let fits = |n: usize| build_payload(1, 1_700_000_000_000, Some(&vec![0xAB; n])).map(|b| b.len());
        let max = (0..4096).rev().find(|&n| fits(n).is_ok()).unwrap();
        assert!(fits(max).unwrap() <= MAX_PAYLOAD);
        assert!(fits(max + 1).is_err());
        assert!((2900..3100).contains(&max), "largest raw ciphertext was {max}");
    }

    #[test]
    fn ciphertext_for_seq_cycles_through_lines() {
        let lines = vec![vec![1u8], vec![2u8], vec![3u8]];
        assert_eq!(ciphertext_for(&lines, 1), Some(&[1u8][..]));
        assert_eq!(ciphertext_for(&lines, 3), Some(&[3u8][..]));
        assert_eq!(ciphertext_for(&lines, 4), Some(&[1u8][..]));
        assert_eq!(ciphertext_for(&[], 1), None);
    }

    #[test]
    fn empty_ciphertext_file_is_rejected() {
        let dir = std::env::temp_dir().join(format!("apns_probe_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty.txt");
        std::fs::write(&path, "\n\n").unwrap();
        assert!(read_ciphertexts(&path).is_err());
        std::fs::write(&path, "AQID\nBAUG\n").unwrap();
        assert_eq!(read_ciphertexts(&path).unwrap(), vec![vec![1, 2, 3], vec![4, 5, 6]]);
    }

    #[test]
    fn production_is_the_default_endpoint() {
        assert_eq!(host(false), "https://api.push.apple.com");
        assert_eq!(host(true), "https://api.sandbox.push.apple.com");
    }
}
```

Run: `cd /c/ChatProject/spikes/c-push/apns-probe && cargo test`
Expected: 컴파일 오류. `cannot find function 'make_jwt'`, `build_payload`, `ciphertext_for`, `read_ciphertexts`, `host`

- [ ] **Step 2: 발송 도구를 구현한다**

`spikes/c-push/apns-probe/src/main.rs`의 시험 모듈 위:

```rust
//! apns-probe: sends N alert pushes (mutable-content) to one device token over APNs HTTP/2.
//! M1b device measurements (roadmap M1b, spike B device run and spike C iOS run).

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use clap::{Args, Parser, Subcommand};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use reqwest::header::HeaderValue;
use serde::Serialize;
use serde_json::{Value, json};

/// APNs limit for everything except VoIP (5120).
const MAX_PAYLOAD: usize = 4096;
/// Apple: mint a new token no more than once per 20 minutes, and keep none longer than 60.
const JWT_REFRESH_AFTER: Duration = Duration::from_secs(45 * 60);

#[derive(Parser)]
#[command(name = "apns-probe", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Send --count pushes, one every --interval-ms.
    Send(SendArgs),
}

#[derive(Args)]
struct SendArgs {
    /// AuthKey_XXXXXXXXXX.p8 (PKCS#8 PEM). Keep it in spikes/c-push/secrets/.
    #[arg(long)]
    key: PathBuf,
    /// 10-character APNs key id.
    #[arg(long)]
    key_id: String,
    /// 10-character Apple team id.
    #[arg(long)]
    team_id: String,
    /// apns-topic: the app's bundle id, never the extension's.
    #[arg(long)]
    topic: String,
    /// Device token as hex (the probe screen's "토큰 복사").
    #[arg(long)]
    token: String,
    #[arg(long, default_value_t = 1)]
    count: u32,
    #[arg(long, default_value_t = 1000)]
    interval_ms: u64,
    /// Number of the first push; continue a series across runs.
    #[arg(long, default_value_t = 1)]
    start_seq: u32,
    /// Base64 ciphertexts, one per line (Fixture/ciphertexts.txt). Without it, no `e` is sent.
    #[arg(long)]
    ciphertexts: Option<PathBuf>,
    /// apns-expiration = now + this many seconds. 0 means "attempt once, do not store".
    #[arg(long, default_value_t = 3600)]
    expiration_secs: u64,
    /// api.sandbox.push.apple.com (Xcode debug builds only). TestFlight needs production.
    #[arg(long)]
    sandbox: bool,
}

#[derive(Serialize)]
struct Claims<'a> {
    iss: &'a str,
    iat: u64,
}

fn host(sandbox: bool) -> &'static str {
    if sandbox { "https://api.sandbox.push.apple.com" } else { "https://api.push.apple.com" }
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("clock before 1970").as_millis() as u64
}

fn make_jwt(key: &EncodingKey, key_id: &str, team_id: &str, iat_secs: u64) -> Result<String> {
    let mut header = Header::new(Algorithm::ES256);
    header.typ = None; // Apple documents only `alg` and `kid`
    header.kid = Some(key_id.to_owned());
    Ok(encode(&header, &Claims { iss: team_id, iat: iat_secs }, key)?)
}

/// Custom keys are peers of `aps`; Apple ignores custom keys inside `aps`.
fn build_payload(seq: u32, sent_at_ms: u64, ciphertext: Option<&[u8]>) -> Result<Vec<u8>> {
    let mut v = json!({
        "aps": {
            "alert": { "title": "측정", "body": format!("알림 {seq}번") },
            "mutable-content": 1
        },
        "seq": seq,
        "sent_at": sent_at_ms
    });
    if let Some(ct) = ciphertext {
        v["e"] = Value::String(STANDARD.encode(ct));
    }
    let bytes = serde_json::to_vec(&v)?;
    if bytes.len() > MAX_PAYLOAD {
        bail!("payload is {} bytes, APNs limit is {MAX_PAYLOAD}", bytes.len());
    }
    Ok(bytes)
}

fn read_ciphertexts(path: &Path) -> Result<Vec<Vec<u8>>> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let lines = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| STANDARD.decode(l.trim()).context("ciphertext line is not base64"))
        .collect::<Result<Vec<_>>>()?;
    if lines.is_empty() {
        bail!("{} has no ciphertext lines", path.display());
    }
    Ok(lines)
}

/// Push `seq` (1-based) carries line `(seq - 1) mod n`.
fn ciphertext_for(lines: &[Vec<u8>], seq: u32) -> Option<&[u8]> {
    if lines.is_empty() {
        return None;
    }
    Some(&lines[(seq as usize - 1) % lines.len()])
}

#[tokio::main]
async fn main() -> Result<()> {
    let Cmd::Send(a) = Cli::parse().cmd;
    let pem = std::fs::read(&a.key).with_context(|| format!("read {}", a.key.display()))?;
    let key = EncodingKey::from_ec_pem(&pem).context("not a PKCS#8 EC private key (.p8)")?;
    let lines = match &a.ciphertexts {
        Some(path) => read_ciphertexts(path)?,
        None => Vec::new(),
    };
    let url = format!("{}/3/device/{}", host(a.sandbox), a.token);
    let client = reqwest::Client::builder()
        .http2_prior_knowledge() // APNs speaks HTTP/2 only
        .timeout(Duration::from_secs(30))
        .build()?;

    let mut minted = Instant::now();
    let mut jwt = make_jwt(&key, &a.key_id, &a.team_id, now_ms() / 1000)?;
    let (mut ok, mut failed) = (0u32, 0u32);
    for seq in a.start_seq..a.start_seq + a.count {
        if minted.elapsed() >= JWT_REFRESH_AFTER {
            jwt = make_jwt(&key, &a.key_id, &a.team_id, now_ms() / 1000)?;
            minted = Instant::now();
        }
        let sent_at = now_ms();
        let body = build_payload(seq, sent_at, ciphertext_for(&lines, seq))?;
        let expiration = if a.expiration_secs == 0 { 0 } else { sent_at / 1000 + a.expiration_secs };
        let mut auth = HeaderValue::from_str(&format!("bearer {jwt}"))?;
        auth.set_sensitive(true);

        let started = Instant::now();
        let response = client
            .post(&url)
            .header("authorization", auth)
            .header("apns-topic", &a.topic)
            .header("apns-push-type", "alert")
            .header("apns-priority", "10")
            .header("apns-expiration", expiration.to_string())
            .header("apns-id", uuid::Uuid::new_v4().to_string())
            .body(body.clone())
            .send()
            .await;
        let rtt = started.elapsed().as_millis();
        match response {
            Err(e) => {
                failed += 1;
                println!("seq={seq} bytes={} NETWORK-ERROR rtt_ms={rtt} {e}", body.len());
            }
            Ok(r) => {
                let status = r.status();
                let apns_id = r.headers().get("apns-id").and_then(|v| v.to_str().ok()).unwrap_or("-").to_owned();
                // Empty on 200; otherwise {"reason": ..., "timestamp"?: ...}.
                let text = r.text().await.unwrap_or_default();
                if status.is_success() { ok += 1 } else { failed += 1 }
                println!("seq={seq} bytes={} status={} apns-id={apns_id} rtt_ms={rtt} {text}", body.len(), status.as_u16());
            }
        }
        if seq + 1 < a.start_seq + a.count {
            tokio::time::sleep(Duration::from_millis(a.interval_ms)).await;
        }
    }
    println!("done: ok={ok} failed={failed}");
    if failed > 0 {
        std::process::exit(1);
    }
    Ok(())
}
```

Run: `cd /c/ChatProject/spikes/c-push/apns-probe && cargo test && cargo clippy --all-targets -- -D warnings`
Expected: `6 passed`, 경고 없음.

- [ ] **Step 3: 커밋**

```bash
cd /c/ChatProject
git add Cargo.toml spikes/c-push/apns-probe/Cargo.toml spikes/c-push/apns-probe/Cargo.lock spikes/c-push/apns-probe/src/main.rs
git status --short | grep -iE "\.p8|secrets" ; echo "secret check exit=$?"   # exit=1 이어야 한다
git commit -m "feat(spike-c): apns-probe sends measurement pushes to a TestFlight build"
git push
```

### Task 10: 기기 실측과 기록 (사용자와 함께)

**Files:**
- Modify: `spikes/b-mls-memory/RESULTS.md` ("Device run (iPhone, TestFlight)" 절)
- Modify: `spikes/c-push/RESULTS.md` ("iOS (APNs)" 절)
- Modify: `docs/superpowers/specs/2026-10-04-p2p-chat-design.md` (8.2, 14, 17절 #1·#12)
- Modify: `docs/superpowers/plans/2026-10-04-p2p-chat-roadmap.md` (M1b 상태, §7 entitlement 신청일)

준비: Task 8의 TestFlight 빌드가 iPhone에 깔려 있다. 이 PC에 `apns-probe`가 빌드돼 있다. `.p8`은 `spikes/c-push/secrets/`에 있다. 명령은 모두 `spikes/c-push/apns-probe`에서 실행한다.

```bash
P="cargo run --release -q -- send --key ../secrets/AuthKey_<KEYID>.p8 --key-id <KEYID> --team-id 9J2FNH63M2 --topic dev.chatproject.chatapp --token <TOKEN>"
```

- [ ] **Step 1: 토큰을 받는다**

1. iPhone에서 앱 → 오른쪽 위 측정 단추 → "알림 허용하고 토큰 받기" → 허용.
2. "토큰 복사"를 누른다.
3. 토큰을 메모나 메신저로 PC에 옮겨 `<TOKEN>`에 넣는다.

- [ ] **Step 2: 스파이크 B 기기판 — NSE 메모리**

1. 앱을 완전히 닫고(앱 전환기에서 위로 밀기) iPhone을 잠근다.
2. 기준값: `$P --count 1 --start-seq 1`. 복호 없이 Swift 런타임과 라이브러리만 올라간 NSE의 메모리다.
3. 1분 뒤 복호를 측정한다: `$P --count 8 --start-seq 2 --interval-ms 5000 --ciphertexts ../../../app/ios/NotificationService/Fixture/ciphertexts.txt`
4. 잠금 화면 알림을 확인한다.
   - 제목: `NSE 최대 x.x MB`
   - 본문: `#N M1b probe message …`
5. 앱 → 측정 → 새로고침. 다음 값을 적는다.
   - 기준 피크, 복호 피크의 최댓값
   - 복호 시간(ms)
   - "끝 기록 없음" 줄의 수
6. 판정:
   - 복호 피크 최댓값이 **15 MB 미만**이면 통과다.
   - 넘으면 여기서 멈추고 사용자와 설계 17절 #1의 대안을 정한다. 대안은 알림을 "새 메시지"로만 보여 주는 것이다.

- [ ] **Step 3: 스파이크 C iOS판 — 꺼진 iPhone에 남는 개수와 지연**

1. 앱 → 측정 → "기록 지우기".
2. iPhone의 전원을 끈다.
3. `$P --count 5 --start-seq 101 --interval-ms 60000 --expiration-secs 86400`. 5분 동안 1분 간격으로 보낸다. 출력의 `status=200` 다섯 줄을 확인한다.
4. 마지막 발송 10분 뒤 iPhone을 켜고 잠금을 푼다.
5. 1분 기다린 뒤 다음을 적는다.
   - 알림 센터에 뜬 알림의 수와 `#`번호
   - 앱 → 측정 → 새로고침에 나온 각 줄의 `지연`
6. 같은 절차를 비행기 모드로 한 번 더 한다. 전원은 켠 채 3단계 대신 비행기 모드를 켜고, 4단계에서 끈다.
7. 설계의 가정은 앱당 1개(대개 최신)가 남는다는 것이다. 실제 수가 다르면 그대로 기록한다.

- [ ] **Step 4: 결과를 기록한다**

1. `spikes/b-mls-memory/RESULTS.md` 끝에 `## Device run (iPhone, TestFlight)` 절을 더한다.
   - 날짜, iPhone 모델과 iOS 버전, 빌드 번호
   - 기준 피크와 복호 피크(MB), 복호와 로드 시간
   - 고정 파일 크기(1.35 MB, 자격 증명 200 B)
   - 판정
   - 상태 저장이 JSON이라 실제(M5 바이너리 저장)보다 크게 나온다는 주의
2. `spikes/c-push/RESULTS.md` 끝에 `## iOS (APNs)` 절을 더한다.
   - 전원 꺼짐과 비행기 모드 각각에 대해 보낸 수, 남은 수, 남은 번호, 지연
   - `apns-expiration`은 86400
3. 설계 문서를 고친다.
   - 8.2절: "앱당 1개만 보관" 문장 뒤에 `(2026-10 실측: <결과>)`를 더한다.
   - 14절 표: "iOS NSE 메모리" 줄에 실측 피크를 적는다.
   - 17절 #1: 상태를 실측 결과로 바꾼다. 예: "기기 실측 피크 x.x MB로 통과(2026-10-xx)".
4. 로드맵 M1b를 고친다.
   - `상태: 완료 (2026-10-xx)` 줄을 더한다.
   - 범위 줄의 "App Store Connect API 키로 클라우드 서명"을 "수동 서명(배포 인증서 + App Store 프로파일 2개), 업로드는 App Store Connect API 키"로 바꾼다.
   - 사용자 준비의 "APNs 인증 키(.p8) … GitHub 저장소 시크릿에"를 "APNs 키는 이 PC에만 둔다(서버 시크릿은 M7)"로 바꾼다.
   - 자동 시험 줄의 "main 브랜치 push와 매일 밤"을 "매 push"로 바꾼다.

- [ ] **Step 5: entitlement 신청 (사용자)**

설계 17절 #12와 로드맵 §7을 따른다. 승인 전에는 지금 방식(폴백)을 그대로 쓴다.

1. 알림 필터링: https://developer.apple.com/contact/request/notification-service
   - 대상 번들 ID는 `dev.chatproject.chatapp.NotificationService`다.
   - 용도: 차단한 대화의 알림 숨기기, E2E 암호화 메신저.
2. multicast: https://developer.apple.com/contact/request/networking-multicast
   - 용도: 같은 LAN의 메일박스 발견, iroh mDNS(224.0.0.251:5353).
   - Bonjour만으로 부족한 이유도 적는다.
3. 신청한 날짜를 로드맵 §7 첫 줄에 적는다.
4. 승인되면 다음 순서로 진행한다.
   - App ID에서 capability를 켠다.
   - 프로파일을 다시 만들고 시크릿을 바꾼다.
   - entitlement 파일에 키를 더한다.
   - 이 단계는 M7 계획으로 넘긴다.

- [ ] **Step 5b: 기록을 커밋한다**

```bash
cd /c/ChatProject
git add spikes/b-mls-memory/RESULTS.md spikes/c-push/RESULTS.md docs/superpowers/specs/2026-10-04-p2p-chat-design.md docs/superpowers/plans/2026-10-04-p2p-chat-roadmap.md
git commit -m "docs: M1b device measurements (NSE memory, APNs storage on iPhone)"
git push
```

---

## 마무리

- 마지막 브랜치 리뷰 전에, main ruleset의 필수 검사에 이번에 생긴 작업 `workflows`, `bindings`, `ios`를 더한다.
  - `$SCRUB/ruleset.json`의 `required_status_checks`에 세 항목을 더한다. 형식은 `{ "context": "ios", "integration_id": 15368 }`.
  - `gh api -X PUT repos/HyunwookYoo/chatproject/rulesets/$RULESET_ID --input ruleset.json --silent`
- PR을 merge commit이나 fast-forward로 병합한다(진행 방식).

## 완료 기준 (로드맵 M1b)

- [ ] 저장소가 공개이고, 이력에 집 IP·폰 IP·폰 시리얼·계정 경로가 없다. Task 1의 두 번째 스캔이 증거다.
- [ ] CI가 모두 녹색이다: `workflows`, `rust`, `bindings`, `flutter`, `windows`, `android`, `ios`.
  - `ios`는 시뮬레이터에서 `core_test` 3개를 돌리고, NSE가 앱에 들어갔는지 확인한다.
  - `android`는 cargokit이 1.92.0으로 빌드했는지 확인한다.
- [ ] TestFlight 빌드가 iPhone에 설치되고 실행된다(Task 8 Step 6).
- [ ] NSE 피크 메모리가 15 MB 미만이다(Task 10 Step 2). 넘으면 설계 17절 #1의 대안을 사용자와 정한다.
- [ ] APNs 실측값이 설계 8.2절과 스파이크 C 결과에 적혀 있다(Task 10 Step 3·4).
- [ ] M1a가 넘긴 일 M1b 절의 8개 항목이 모두 끝났다(위 표).
- [ ] 알림 필터링·multicast entitlement 신청일이 로드맵 §7에 적혀 있다.
