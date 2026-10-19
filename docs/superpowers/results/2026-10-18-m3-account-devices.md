# M3 account and devices — implementation and validation results

Status: **C4 source/local validation complete, pending task and whole-branch review; M3 §10 is not complete**. Source base for C4: `116ef3b83f24b967098056d73c2c06cc5143fcbf`; whole M3 review base: `aadd2db`. The approved scope is the [M3 plan](../plans/2026-10-18-m3-account-devices.md) and [design §10](../specs/2026-10-18-m3-account-devices-design.md). Local checks ran on Windows x86_64 with Rust 1.92.0, Flutter 3.44.6/Dart 3.12.2, FRB 2.11.1 and the existing pinned Android SDK/NDK setup. There is no Apple build/runtime or signed iPhone evidence on this host.

Historical task reports and current C4 executions are separate below. All source, failure logs, review reports and scratch probes remain preserved for controller review. No push, merge, signed upload, registry change, public DHT/relay test or physical-device run occurred in C4.

## Accepted source chronology and provenance

The [prior evidence appendix](2026-10-18-m3-account-devices-prior-evidence.md) retains task-reported commands, actual failures, correction rounds and platform limits, including their original commit attribution. The [ruling appendix](2026-10-18-m3-account-devices-rulings.md) retains every controller `Ruling:` paragraph from the A/B/C ledgers in ledger order, including rationale and cost if wrong. These historical reports do not imply that their artifacts were rebuilt at C4 HEAD.

| Task / commits | Implemented boundary and evidence attribution |
|---|---|
| Plan `6050108`, contract clarification `231f6a9` | Approved design/plans; documentation changes, no runtime proof. |
| A1 `4a7413a`, correction `ddf0357` | BIP39 entropy/HKDF/IDs/errors and debug-only FakeClock. Golden fixture tests, compile-fail secret ownership, generated bindings and strict cfg correction are recorded under A1. |
| A2 `96c23f9`, correction `56109b2` | Encrypted atomic snapshots and canonical single writer; Windows real child-process crash/ownership tests; persistent suspension ownership correction. Initial Android portability assumption was disproved by C4 below. |
| A3 `4bec589`, correction `5159604` | Actual Windows DPAPI and Android Keystore/JNI plus explicit restart phases; iOS Keychain source/buffer cleanup only. Android native-provider proof did not exercise the Rust Store lock. |
| B1 `048b4ed` (overlapped A work) | Strict account/certificate/record encoding and bounded signed packet/DHT adapter. Local codec/backend and target compilation evidence; no public DHT success. |
| A4 `be9de78`, correction `d197fd4` | Stable Candidate transport, account-owned gate, synchronous lifecycle closure and bounded quiescence; authorized rekey/token-publication race correction. Native iOS finite-lock execution remains open. |
| A5 `1575dd3` | Create/confirm/recovery state machine, same PeerId/new installation and refusal to overwrite established state. Local fake-record/storage tests, not physical recovery. |
| B2 `5ed4259`, correction `a664725` | Durable publication/high-water/revocation/conflict state; future observation hold correction. Lookup unavailable vs completed absence remains distinct. |
| B3 `a02d1ce`, lint `9a61c9b`, correction `185445c` | Channel-bound account proofs, authority revocation, public-only record exchange and post-transition restrictions. Existing accepted local security tests and strict gates; no transport-connected ⇒ auth-ready inference. |
| B4 `eedcf6f` | Identity invites/contacts/safety confirmation. Initial RED was missing-API compilation; executable reject-all negative control came later. Historical process Minor remains explicit. |
| B5 `678eaa4`, correction `fe29a9a` | Noise pairing, both consent roles/explicit Existing approval, QR cleanup and authority-time recheck. Synthetic ciphertext gate evidence is distinct from B6 durable transfer. Snow internal-memory erasure remains a residual. |
| B6 `dd14e57`, correction `2384601` | Durable disclosure/import/receipt/membership activation, crash boundaries, exact resume and discard. Correction covers post-store authority, finite polling, contact ownership, reached-boundary evidence and cancelled New resume. |
| C1 `adad1c1`, correction `5450338` | Typed bridge and cached public projection, stream ownership, exact revision correction. Actual Windows Flutter/DPAPI write/read/final-read and injected Locked/Missing; generated 11-file stability and fixture exclusion. |
| C2 `2f5edb9`, correction `33e81d6` | Account/contact UI, sensitive display ownership and safety-request race correction. Widget semantics/large text/keyboard evidence; OS accessibility execution is separate. |
| C3 `8292367`, corrections `2b9b884`, `116ef3b` | Pair/device UI, scanner, native privacy and public invite intake. Original 125 Flutter tests/native builds/Release artifacts belong to `8292367`; later corrections have 30/31 affected tests plus analysis. C4 final artifacts below rebuild amended source. Actual Windows Peek remained blocked, and Apple/physical OS gates remain open. |
| C4 (this commit; base `116ef3b`) | Same-actor debug local UI actions, shared owned signed-record service, redaction, owned-app restart scenario, CI/release boundaries, Android production writer-lock portability fix and current validation below. |

## C4 implementation and fixture boundaries

The two participants are real native apps using the real platform key protector and the same `AccountHandle` used by Flutter. Controller port 47000 owns a bounded `/records` WebSocket service. It transports public signed packet bytes in memory; the core still signs and verifies packet signatures, key identity, time, high-water and membership. The configuration is literal loopback with fixed port/path, installed before account opening, immutable thereafter and retained after an opening failure. No production DHT fallback is permitted. This is a local real-time fixture with `SystemClock`, not public pkarr evidence or a deterministic fake-clock test.

The harness invokes explicit debug local UI actions for create/confirm/invite/safety/pair/approval/revoke/recover/refresh. Pair routing uses the existing Local-only transport target fixture and the owning app's public endpoint ticket. It never modifies the QR, PSK, consent, signature or approval gate. Publication pause retains actual signed old membership; normal account commands must refuse PendingImport. After publication resumes, normal refresh must observe exact signed installation/transport membership before normal New-side refresh can activate the durable import.

Android restarts use only framework-owned `chat_e2e_1`/`chat_e2e_2` participants and dedicated debug-only canonical `filesDir/e2e/<run>/<participant>/store-N` roots. The harness does not reset or delete earlier roots during the run; the existing owned-AVD clean-snapshot/no-snapshot-save policy is unchanged. Restart assertions require a changed process/core instance and unchanged public identity, installation and endpoint for a retained store. M2 remains the default scenario, with its different anonymous endpoint after restart assertion unchanged. Three unrelated user AVDs and the original `C:/ChatProject` checkout were not modified.

Controller logging classifies frames before any file write. Account arguments/results, errors, malformed JSON and late results are redacted; diagnostics use error codes and public projections. Generated recovery words/QR remain in memory. New Rust-owned JSON/typed secret copies and serialization scratch use drop cleanup. Opaque tungstenite/serde internal copies and Dart immutable strings have no guaranteed RAM-erasure claim; Snow's retained internal-copy limitation from B5 also remains.

## C4 observed failures and fixes

| Retained attempt / artifact | Actual observation and disposition |
|---|---|
| `task-4-red-dart.log` → `task-4-green-redaction.log` | Five new public-marker redaction failures plus 41 existing passes, then 46/46. Success/error/malformed/late/timeout and safe exception formatting covered before implementation. |
| `task-4-red-python.log` → `task-4-green-python-1.log` | 96 tests with three expected new marker/welcome-readiness failures and one existing skip; corrected actual welcome expectations, then 96 OK/1 skip. |
| `task-4-red-rust.log` → `task-4-green-rust-1.log` | Config parser behavioral rejection before local record configuration; three focused hook tests passed after implementation. |
| Android M3 runs `1792334513469045`, `1792334822547976` | Both failed before account creation: snapshots unavailable, no AccountChanged. Owned fresh root contained only `.account.lock`; native UI reported StorageLocked. Original failure logs and six secondary cleanup no-device errors from premature emulator interruption remain retained. |
| `task-4-cleanup-red2.log` → `task-4-cleanup-green.log` | Two actual acquisition/dependency-order failures, then two passes. Only acquired owned emulators are deferred until activity/reverse callbacks finish; other process interruption and primary/secondary failure precedence remain. Initial callback type error is retained separately, not counted as behavioral RED. |
| Native account run `1792335042713557/28928.stdout.log` | Before production fix: expected Unconfigured, actual Locked. Wrapper log is empty because the framework captures child stdout separately. |
| Android writer fix / native run `1792335301051374/16660.stdout.log` | Android-only borrowed-FD `libc::flock(LOCK_EX\|LOCK_NB)` replaces unsupported Rust 1.92 `File::try_lock`; `WouldBlock→WriterBusy`, all other failures StorageLocked. Native account creation 1/1 passed. Cargo.lock adds only the existing libc dependency edge; pin 0.2.189, no version upgrade. Windows/iOS paths and persistent/finite policies unchanged. |
| Native writer probe `task-4-writer-probe-run.log` | Fresh production Android x86_64 rlib: independent open WriterBusy, spawned process contender WriterBusy, drop owner then reopen/load empty store, protector tripwire untouched; exit0. Dedicated owned `/data/local/tmp/chatproject-writer-probe-1792335478748965`; no account/key creation. First direct compile missed host proc-macro search path (E0463); second with target+host deps passed. Exact source/compiler args/rlib hash/run output retained. |
| Probe cleanup | Verified `chat_e2e_1`, stopped that owned emulator with `adb emu kill`; subsequent adb inventory empty. `devices down` conservatively retained an uncertain stale ownership record without signaling a stale PID. Probe artifacts/root were preserved. |
| M3 run `1792335553333150` | Two actual Active creations, invites/safety, fresh Candidate and stable Candidate restart. NewDisplays scan returned AuthFailed; exact internal branch was not established and is not retroactively attributed to routing. Separate inspection found omitted Local target routing, independently covered by route RED→GREEN. Cleanup retained a real 30s Android-2 force-stop timeout; owned processes/reverses closed and activity record retained. |
| `puppet_account` integration | Actual first fixture install after an already opened account rejects without changing Local profile; Store WriterBusy→release→retry retains fixture and subsequently makes actual local signed-record requests; hook and public AccountHandle share Candidate state. Duplicate config rejects. Route unknown-operation behavioral RED then GREEN. Public zero-entropy fixture only. |
| M3 run `1792336057228362` | Numeric public diagnostic: expiry 1792336563, owner/scanner seconds 1792336263; no clock-policy change. Real pairing/approval reached PendingImport and refused ordinary commands, then timed out waiting automatic import activation after publication failure closed its temporary channel. Harness now uses the existing explicit refresh contract. Real cleanup timeout retained. |
| M3 run `1792336494026710` | Both pairing directions, actual observed membership and New Active, plus retained Active restart. Revocation observation timed out: publication ACK is not an observed record and the actor's periodic refresh is 60s. Harness now explicitly refreshes after create/revoke/recover; no timeout increase or core publication-policy change. |
| Dart broad attempt `task-4-dart-full.log` | 186 passes, one existing Windows SIGINT skip, three existing devices-CLI failures while the real owned-AVD scenario was concurrently using their ports. This scheduling error is retained; affected tests passed 3/3 after real AVD cleanup (`task-4-devices-serial.log`). `Emulators._requireFreePort` binds the actual 5554/5556 loopback ports. No timeout weakening or existing-test source change. |
| Analyzer/tooling attempts | Early analyzer exit0 still contained informational brace lints; corrected status and used `--fatal-infos`. Incidental formatting churn was removed. A temporary editing-script embedded-string substitution caused parse errors, corrected before execution. Wrong-cwd and CP949 editing/extraction failures were retained as authoring errors, not behavioral RED. |

Rust portability source: [Rust 1.92 Unix File lock implementation](https://github.com/rust-lang/rust/blob/1.92.0/library/std/src/sys/fs/unix.rs#L1277-L1330) excludes Android; [AOSP Bionic flock declaration](https://raw.githubusercontent.com/aosp-mirror/platform_bionic/master/libc/include/sys/file.h) supplies the native operation. The actual native RED/GREEN and cross-process probe above substantiate the fix, not source inspection alone.

## Current final checks

Completed current-source checks are listed here; final native/Release/generation commands and hashes are appended below.

| Command (repository root unless noted) | Observed result / retained log |
|---|---|
| `cargo test --workspace --locked` | Exit0; task-4-workspace.log |
| `cargo test --locked -p chat_core --features test-hooks` | Exit0, 262 passed across unit/integration/doc summaries; task-4-core-hooks.log |
| `cargo test --locked -p chat_ffi --features test-hooks` | Exit0; task-4-ffi-hooks.log; explicit Windows phase helper ignore is covered by native fresh-process phases below |
| `cargo test --locked -p chat_node --features harness` | Exit0, 5 tests; task-4-node-harness.log |
| `cargo clippy --workspace --all-targets -- -D warnings` | Exit0; task-4-clippy-workspace.log |
| `cargo clippy --locked -p chat_core --all-targets --features test-hooks -- -D warnings` | Exit0; task-4-clippy-hooks-green.log, prior 8-argument lint retained and two function-level allowances parent-approved |
| `cargo test --manifest-path vendor/iroh-dns/Cargo.toml --locked --lib dns::ios::tests --features tls-ring` | 5 passed; host simulation only, not native iOS execution |
| `flutter analyze`; `flutter test` (app) | No issues; 134/134 passed; task-4-flutter-analyze.log, task-4-flutter-full.log. Expected inherited Home/network error-test traces remain |
| `dart analyze --fatal-infos` (tools/e2e) | No issues; task-4-analyze-clean-2.log |
| `dart test` (tools/e2e) + affected serial rerun | 186 passes/1 Windows SIGINT skip/3 existing port-conflicted failures; exact affected 3/3 then passed serially. New scenario 9/9 after final refresh change. See retained history; no claim of one clean full Dart invocation |
| Python CI suite / workflow checker | 96 OK/1 existing platform skip; task-4-python-full.log. Workflow checker passes; after adding explicit Android integration `set -e`, task-4-workflows-final.log also passed |
| `flutter test integration_test/account_test.dart -d windows` | 1/1 passed with awaited model close; task-4-native-windows-account.log |
| `flutter test integration_test/account_storage_test.dart -d windows --dart-define=ACCOUNT_STORAGE_PHASE=<phase> --dart-define=ACCOUNT_STORAGE_ROOT=<owned-root>` | Five separate invocations write→read→locked→missing→read, each1/1; task-4-native-windows-{1-write,2-read,3-locked,4-missing,5-read}.log, status file all exit0. Root: build/e2e/m3-c4-windows-storage-1792337900. Public receipt proves identity/installation/transport persistence with different core instance. Locked/Missing are injected statuses around real DPAPI, not actual OS-user lock |
| `python tools/ci/check_release_hooks.py app/build/app/outputs/flutter-apk/app-debug.apk` | Positive exclusion control: expected exit1 for actual M3 Debug controller marker; task-4-debug-marker-control.log. Release absence/runtime checks passed separately below |

## Remaining external/platform gates and concrete future run package

No permission is requested or external run executed by this result. The following reviewable scope must precede any later authorization. Each package uses disposable test accounts; generated phrases and QR are never included in logs, screen recordings or uploads. Public identity/installation/endpoint/instance IDs, state transitions, commands, artifact hashes and numeric outcomes are sufficient evidence.

| Gate / exact target scope | Required sequence and artifact package |
|---|---|
| Windows↔physical Android, two-way QR | Identify one owned Windows build and one explicitly selected physical Android serial/app, preserve production stores, use dedicated test stores. Record `physical/windows-android/build-manifest.json` (commit, toolchain, APK/DLL hashes, OS/device model), `public-events.jsonl` and `checklist.md`. Create two accounts; invite/contact and explicit safety confirmation; scan both pair directions with camera, compare SAS on both, deny/mismatch first, then approve Existing; verify observed membership/Active. Restart Candidate and Active, assert stable endpoint/installation and new instance. Exercise offline publish pending, revoke and same-PeerId recovery with fresh installation; record public before/after membership only. |
| Physical OS key/backup/privacy boundary | On the explicitly selected owned devices, measure Android Keystore protection level (software vs TEE/StrongBox) without promoting emulator software results. Verify backup/data-extraction exclusions with actual packaging/OS behavior; verify device-lock/key-missing behavior and restart preservation. Windows DPAPI other-user/other-machine cases require separately provisioned owned accounts/host, not the current user's key loss. Record `physical/storage/privacy-checklist.md`, safe error codes, native API results and a file/backup manifest excluding secret contents. Actual Windows Peek requires the user to resolve the already observed focus/occlusion condition before a new visual test; do not control their game or retry unchanged focus. Android FLAG_SECURE/OS preview and accessibility require actual observation. |
| Apple compile/simulator/native | On an available macOS/Xcode runner: pinned Rust iOS targets, generated bindings, Runner/NSE compile/link, existing `check_ios_app.sh` and NSE regressions; actual simulator app-container path for phased storage tests. Run account/privacy tests and explicit write→read→injected locked→injected missing→read in fresh processes preserving the same app data. Record `apple-simulator/build-manifest.json`, command logs, public phase receipts and lifecycle timing. Source CI entries here are pending execution, not a pass. Verify finite foreground Store lock and completed release before suspend; pending/timeout never counts as lock release. |
| Signed physical iPhone | Identify a permitted Apple team/provisioning profile and one explicitly authorized physical device; prepare the exact signed Runner/NSE artifact hashes and entitlements/access groups before any install/upload. Record `physical/iphone/signing-manifest.json`, safe Keychain accessibility/access-group evidence, first unlock after reboot, protected storage write/read/restart, scene shielding/QR camera flow, both pairing roles, revoke/recovery and same PeerId/new installation. Simulator results and prior M2 signed builds do not satisfy this M3 gate. No distribution/upload is implicit. |
| Separate public pkarr/relay run | Present exact commit/artifacts, disposable account public key, chosen public pkarr DHT/bootstrap/relay endpoints, permitted time window and traffic scope before authorization. Verify one signed publication→independent lookup→normal core signature/key/time/membership verification; record packet byte count/hash/public fields, lookup timestamps and safe error codes in `public-pkarr/manifest.json` and `observations.jsonl`. Never publish mnemonic, private seed, contact names, certificates or IP addresses in account records. Keep local fake-record results separate; no public run was made here. |
| Native public identity URI intake | Android VIEW/onNewIntent and iOS scene/AppDelegate intake source feeds the same parser and explicit add. Actual OS launch requires an owned public invalid/valid fixture run and receipts. Windows protocol registration and warm forwarding remain packaging/execution gaps; a future scoped registry/packaging proposal and reviewable installer diff are required before mutation. Cold argv handling alone is not protocol registration proof. |

Known residuals carried forward: B4 historical compile-RED chronology; expected inherited Home/network test stack traces and generator private-WindowsProtector parser notes; pinned scanner KGP future-compatibility warning and Android instrumentation Gradle-deprecated-features/Gradle10 compatibility notice; Windows Peek visual block; actual Android OS preview/TEE/backup proof; all Apple runtime/signing gates; public pkarr roundtrip; OS screen-reader/physical camera evidence. M2 latency/LAN/signed acceptance remains separately open and is not closed by M3 source or local results.

## C4 Android writer ownership probe: reproducible retained evidence

The source below links the exact production `chat_core` rlib built by the successful native Android account run. It exercises `Store::open`, its actual lock file and `File` lifetime. The only protector is a panic tripwire: no secret/account is created. This is a narrow scratch executable, not a production helper or a substitute for the native app lifecycle.

```rust
use chat_core::{account::{storage::{KeyProtector, Store}, Secret32, StoreId}, CoreError, ErrorCode};
use std::{path::Path, process::Command, sync::Arc};
struct Tripwire;
impl KeyProtector for Tripwire {
    fn protect(&self, _: StoreId, _: &Secret32) -> Result<Vec<u8>, CoreError> { panic!("unexpected secret access") }
    fn unprotect(&self, _: StoreId, _: &[u8]) -> Result<Secret32, CoreError> { panic!("unexpected secret access") }
    fn delete(&self, _: StoreId) -> Result<(), CoreError> { panic!("unexpected secret access") }
}
fn open(root: &Path) -> Result<Store, CoreError> { Store::open(root, Arc::new(Tripwire)) }
fn main() {
    let args: Vec<_> = std::env::args().collect();
    assert_eq!(args.len(), 3);
    let root = Path::new(&args[2]);
    let name = root.file_name().unwrap().to_str().unwrap();
    assert_eq!(root.parent(), Some(Path::new("/data/local/tmp")));
    assert!(name.strip_prefix("chatproject-writer-probe-").unwrap().chars().all(|c| c.is_ascii_digit()));
    if args[1] == "contender" {
        assert_eq!(open(root).err().unwrap().code, ErrorCode::WriterBusy);
        println!("cross-process contender denied: WriterBusy; no secret access");
        return;
    }
    assert_eq!(args[1], "owner");
    let first = open(root).unwrap();
    assert_eq!(open(root).err().unwrap().code, ErrorCode::WriterBusy);
    println!("separate open denied: WriterBusy; no secret access");
    let child = Command::new(std::env::current_exe().unwrap()).arg("contender").arg(root).status().unwrap();
    assert!(child.success());
    drop(first);
    let mut next = open(root).unwrap();
    assert!(next.load().unwrap().is_none());
    drop(next);
    println!("owner drop released lock; reopen succeeded; empty store; no secret access");
}

```

Exact `rustc` argument vector (one argument per line; invoked with `rustc @args` in PowerShell where `$args = Get-Content -Encoding utf8 <argument-file>`):

```text
+1.92.0
--edition=2024
--crate-name
writer_probe
--target
x86_64-linux-android
-C
linker=C:/Users/hyunw/AppData/Local/Android/Sdk/ndk/27.0.12077973/toolchains/llvm/prebuilt/windows-x86_64/bin/x86_64-linux-android28-clang.cmd
-L
dependency=C:\Users\hyunw\orca\workspaces\ChatProject\m2-iroh-transport\app\build\chat_ffi\build\x86_64-linux-android\debug\deps
--extern
chat_core=C:\Users\hyunw\orca\workspaces\ChatProject\m2-iroh-transport\app\build\chat_ffi\build\x86_64-linux-android\debug\deps\libchat_core-be65edd985431946.rlib
C:\Users\hyunw\orca\workspaces\ChatProject\m2-iroh-transport\.superpowers\sdd\2026-10-18-m3-c-app-validation/task-4-writer-probe.rs
-o
C:\Users\hyunw\orca\workspaces\ChatProject\m2-iroh-transport\.superpowers\sdd\2026-10-18-m3-c-app-validation/task-4-writer-probe
-L
native=C:\Users\hyunw\orca\workspaces\ChatProject\m2-iroh-transport\app\build\chat_ffi/build\x86_64-linux-android\debug\build\blake3-c22668e34fa65afe\out
-L
native=C:\Users\hyunw\orca\workspaces\ChatProject\m2-iroh-transport\app\build\chat_ffi/build\x86_64-linux-android\debug\build\blake3-c22668e34fa65afe\out
-L
native=C:\Users\hyunw\orca\workspaces\ChatProject\m2-iroh-transport\app\build\chat_ffi/build\x86_64-linux-android\debug\build\dart-sys-156373e2e232da46\out
-L
native=C:\Users\hyunw\orca\workspaces\ChatProject\m2-iroh-transport\app\build\chat_ffi/build\x86_64-linux-android\debug\build\ring-bdcba64e89bcc6cb\out
-L
native=C:\Users\hyunw\orca\workspaces\ChatProject\m2-iroh-transport\app\build\chat_ffi/build\x86_64-linux-android\debug\build\ring-fa167968d9dd300b\out
-L
dependency=C:\Users\hyunw\orca\workspaces\ChatProject\m2-iroh-transport\app\build\chat_ffi\build\debug\deps

```

Production rlib provenance:

```text

Algorithm : SHA256
Hash      : 629939E71F5D386070B640BAA44914BEA7E19D618249F214B0C083923A43B6B3
Path      : C:\Users\hyunw\orca\workspaces\ChatProject\m2-iroh-transport\app\build\chat_ffi\build\x86_64-linux-android\debug\deps\libchat_core-be65edd985431946.rlib


```

The first compile omitted the final host-dependency search path and failed E0463; it was retained. The second compile exited 0 with empty output. The owned framework `dart run bin/devices.dart up --count 1` (tools/e2e cwd) run was `1792335478748965`. After confirming `adb -s emulator-5554 emu avd name` was `chat_e2e_1`, the binary was pushed to `/data/local/tmp/chatproject-writer-probe-1792335478748965-bin`, `chmod 700` applied, and executed with arguments `owner /data/local/tmp/chatproject-writer-probe-1792335478748965`. It spawns the same binary as `contender` while the first lock is held.

```text
chat_e2e_1

OK

separate open denied: WriterBusy; no secret access
cross-process contender denied: WriterBusy; no secret access
owner drop released lock; reopen succeeded; empty store; no secret access
probe exit=0

```

The binary SHA256 is `803C63888A55726B714CFD7E796D9CC4AD15C195D55A80732B0F9108EFCA0247`; source SHA256 `EA3D7B17C8585C017D4297BEC27005B880EB47154F4CE1B16112088E1C38F315`. Full hash manifest is retained in `task-4-writer-probe-hashes.txt`. Owned emulator stop and subsequent empty adb inventory are separate from proof of lock release, which was asserted before stop. Stale ownership cleanup did not signal a PID it could not revalidate. Probe/root and all logs remain preserved.

## C4 actual Android scenario ④: run 1792337140795628

`dart run bin/run.dart --profile android --scenario m3 --repo-root C:/Users/hyunw/orca/workspaces/ChatProject/m2-iroh-transport` exited **0**, with `android M3 PASS`. Both owned emulator native apps used real platform protectors. All scenario assertions completed; final ownership JSON has empty process, activity and reverse arrays and adb inventory was empty. Unlike retained failed attempts, this successful run had no cleanup timeout.

The following public receipt is derived from `account-public-evidence.jsonl` (full artifact SHA256 `6c3e6c8450bb86f30138c34f7c8841d8124d5e4689a75af96ab9e571102fca27`). IDs below are hex encodings of the public byte arrays. Actual signed membership was asserted before each `membership_observed` row and New activation. `Active` is the actual public core projection, not inferred from a transport connection. Revocation and recovery issue normal own-peer Refresh; manager projection uses accepted record devices and only exposes Active for Observed plus local authorization.

| Stage | Core instance | Public PeerId | Installation | Endpoint / clock |
|---|---|---|---|---|
| created (Active) | `2594-18dfa9319c9f7580` | `3e7e551d7aa64bf1204621874062118b30c71263c81acb4c10f190663a792488` | `857ae72076284aef86e3bc8f8a8962b48afef0123d2b0d1ba21c2d00fcb03d6a` | `3e3a3433d928981d01e376f26e2095e8d1ea4f0a0f26822c5106540ade6c5877` |
| created (Active) | `3393-18dfa935b5ca0738` | `167e3ae5aab6b963849dc65091fe955d1fd04d72ec84c0bcb0cb9d8173fbf72f` | `9b3058c12caa1636312e0c707aee0c42682dc7c62f0e92218874225d47cd7016` | `6287bda1f1a1890150e0bb409c0e223701be61a2846b3bbe6d3f775c5620e7a9` |
| fresh_store (Unconfigured) | `4089-18dfa93a016a1328` | `—` | `—` | `—` |
| stable_restart (Candidate) | `4484-18dfa93d6ab81bd8` | `—` | `237ff506c2b67e490c79db927b6c8fc15555c1ca0b5d544506a808d7ea2bc530` | `923ad38d41559d9a3fb336c7bee1f98c8aec0d84126e08054fde2deca52e8043` |
| pair_clock NewDisplays | — | — | — | expires=1792337566; owner=1792337266; scanner=1792337265 |
| pending_import_never_normal_sync (PendingImport) | `4484-18dfa93d6ab81bd8` | `3e7e551d7aa64bf1204621874062118b30c71263c81acb4c10f190663a792488` | `237ff506c2b67e490c79db927b6c8fc15555c1ca0b5d544506a808d7ea2bc530` | `923ad38d41559d9a3fb336c7bee1f98c8aec0d84126e08054fde2deca52e8043` |
| membership_observed (Active) | `2594-18dfa9319c9f7580` | `3e7e551d7aa64bf1204621874062118b30c71263c81acb4c10f190663a792488` | `857ae72076284aef86e3bc8f8a8962b48afef0123d2b0d1ba21c2d00fcb03d6a` | `3e3a3433d928981d01e376f26e2095e8d1ea4f0a0f26822c5106540ade6c5877` |
| paired_NewDisplays (Active) | `4484-18dfa93d6ab81bd8` | `3e7e551d7aa64bf1204621874062118b30c71263c81acb4c10f190663a792488` | `237ff506c2b67e490c79db927b6c8fc15555c1ca0b5d544506a808d7ea2bc530` | `923ad38d41559d9a3fb336c7bee1f98c8aec0d84126e08054fde2deca52e8043` |
| stable_restart (Active) | `4820-18dfa9411bef0dc8` | `3e7e551d7aa64bf1204621874062118b30c71263c81acb4c10f190663a792488` | `237ff506c2b67e490c79db927b6c8fc15555c1ca0b5d544506a808d7ea2bc530` | `923ad38d41559d9a3fb336c7bee1f98c8aec0d84126e08054fde2deca52e8043` |
| fresh_store (Unconfigured) | `5085-18dfa942e8132758` | `—` | `—` | `—` |
| pair_clock ExistingDisplays | — | — | — | expires=1792337588; owner=1792337288; scanner=1792337289 |
| membership_observed (Active) | `2594-18dfa9319c9f7580` | `3e7e551d7aa64bf1204621874062118b30c71263c81acb4c10f190663a792488` | `857ae72076284aef86e3bc8f8a8962b48afef0123d2b0d1ba21c2d00fcb03d6a` | `3e3a3433d928981d01e376f26e2095e8d1ea4f0a0f26822c5106540ade6c5877` |
| paired_ExistingDisplays (Active) | `5085-18dfa942e8132758` | `3e7e551d7aa64bf1204621874062118b30c71263c81acb4c10f190663a792488` | `2255e41ca98d761a9f2a63ed6f9435fd91d4b6f529c93bb743c08e648a824302` | `a32121131c44b9c11d0fd36e2b82b980875e17e8b45b8b76e2993d1a0138d895` |
| revocation_observed (Active) | `2594-18dfa9319c9f7580` | `3e7e551d7aa64bf1204621874062118b30c71263c81acb4c10f190663a792488` | `857ae72076284aef86e3bc8f8a8962b48afef0123d2b0d1ba21c2d00fcb03d6a` | `3e3a3433d928981d01e376f26e2095e8d1ea4f0a0f26822c5106540ade6c5877` |
| fresh_store (Unconfigured) | `5681-18dfa94774079010` | `—` | `—` | `—` |
| same_peer_recovery (Active) | `5681-18dfa94774079010` | `3e7e551d7aa64bf1204621874062118b30c71263c81acb4c10f190663a792488` | `2c8104db4a00cb180834dbe3fa3848b365de56c8c737890ff73de2c8ad74a38e` | `de9d4bba56229b2a4e0a1c22d51ac0c0f5ccd10dc221285463ee5cc0da5294c8` |
| stable_restart (Active) | `5889-18dfa94a2a45f9c8` | `3e7e551d7aa64bf1204621874062118b30c71263c81acb4c10f190663a792488` | `2c8104db4a00cb180834dbe3fa3848b365de56c8c737890ff73de2c8ad74a38e` | `de9d4bba56229b2a4e0a1c22d51ac0c0f5ccd10dc221285463ee5cc0da5294c8` |
| recovery_restart (Active) | `5889-18dfa94a2a45f9c8` | `3e7e551d7aa64bf1204621874062118b30c71263c81acb4c10f190663a792488` | `2c8104db4a00cb180834dbe3fa3848b365de56c8c737890ff73de2c8ad74a38e` | `de9d4bba56229b2a4e0a1c22d51ac0c0f5ccd10dc221285463ee5cc0da5294c8` |

Wire-log classification audit: {'account_commands': 96, 'redacted_results': 96, 'redacted_events': 46}. Account commands had wholly redacted arguments; classified results/events remain redacted. Generated words/QR were not copied into this receipt. Public local fixture and native execution do not prove physical camera/OS preview, Apple, or public pkarr behavior.

## C4 final containment self-review correction

Canonicalizing both the requested path and the `filesDir/e2e` boundary allowed an `e2e` leaf symlink itself to escape. The corrected same-file internal helper canonicalizes the app parent and appends literal `e2e`; canonical requested paths must be strictly below that boundary. The normal debug caller retains its flag gate. No production root was modified, no dependency/API/FRB helper was added, and test fixture roots are fresh cache descendants.

On framework-owned `chat_e2e_1`, run `1792337816113976`, the exact native command from `app/android` was:

```powershell
$env:JAVA_HOME='C:/Program Files/Android/Android Studio/jbr'
$env:FLUTTER_ROOT='C:/tools/flutter'
$env:ANDROID_SERIAL='emulator-5554'
$env:CARGOKIT_TOOLCHAIN='1.92.0'
$env:CARGOKIT_VERBOSE='1'
./gradlew.bat :app:connectedDebugAndroidTest '-Pandroid.testInstrumentationRunnerArguments.class=dev.chatproject.chat_app.AccountKeyStoreTest#ownedTestPathAcceptsOnlyAbsoluteChildren,dev.chatproject.chat_app.AccountKeyStoreTest#ownedTestPathRejectsChildSymlinkEscape,dev.chatproject.chat_app.AccountKeyStoreTest#ownedTestPathRejectsRootSymlinkEscape' '-Ptarget-platform=android-x64' --console=plain
```

Before correction: **3 executed, 1 failed**, `ownedTestPathRejectsRootSymlinkEscape` raised `AssertionError: Escaping or non-child test directory accepted`; Gradle exit1 (`task-4-owned-path-red.log`). After correction: **3 passed**, Gradle exit0/19s (`task-4-owned-path-green.log`). Full result XML/protobuf directories were copied to retained `task-4-owned-path-{red,green}-results` before the next build. The other cases enforce ordinary absolute children and reject relative/exact-boundary/prefix-sibling/child-symlink escapes. The earlier complete M3 lifecycle pass is retained; this guard amendment is covered directly by native tests and the final Android artifacts.

The first current `devices up` invocation mistakenly supplied run.dart's `--repo-root`; it returned usage64 before acquiring anything. Correct `dart run bin/devices.dart up --count 1` resolves its checkout from the package and succeeded; both logs remain. This is an authoring command error, not behavioral RED.

## Final native retention correction and source-freeze amendment

Android native account test passed1/1 after awaited model.close. The first explicit write phase passed1/1, but the following read failed at account_storage_test.dart:115 because bridge-receipt.json did not exist, before any identity comparison. Actual `adb -s emulator-5554 shell run-as dev.chatproject.chat_app ls ...` then reported unknown package. Installed Flutter3.44.6 test.dart300–305 defaults `uninstall` to true; line489 passes it to debugging options; IntegrationTestTestDevice.kill147–150 uninstalls after each run. Android install itself uses -r. This is fixture invocation/CI teardown, not Keystore decryption failure or path migration.

The minimum correction is `--no-uninstall` on all Android/iOS storage-phase invocations. iOS account/privacy preceding simctl container discovery also retain the app so the container exists. No product/fixture API change, timeout, skip or fallback. The original write/read logs remain task-4-native-android-{1-write,2-read}.log, status exit0/1. A fresh retained root is `/data/user/0/dev.chatproject.chat_app/files/e2e/c4-native-retained-1792337816113976/storage`; the five fresh-process rerun passed all phases with exact order unchanged (details below). Parent approved this CI command-only freeze amendment.

`python tools/ci/check_workflows.py` and `python -m unittest discover -s tools/ci -p test_check_workflows.py -v` passed (30tests). An actual YAML parse verified Android and iOS each have one storage phase-loop command and both include --no-uninstall. Source-freeze-final.json differs from the first24-file manifest only in ci.yml. Existing Gradle/KGP warnings remain recorded; no dependency upgrade.

Optimized feature-enabled boundary tests: `cargo test --locked --release -p chat_core --features test-hooks --test puppet_disabled` passed1/1 (18.26s build); `cargo test --locked --release -p chat_ffi --features test-hooks release_storage_fixture_is_unavailable` passed1/1 (17.30s build). The latter asserts NotAvailable with CORE uninitialized. They demonstrate the debug-assertions half of the double gate even when feature is enabled. Current ordinary artifact/runtime proof follows separately.

### Corrected Android native phase result

All five fresh Flutter invocations with `--no-uninstall` completed **1/1, exit0** in order write→read→injected locked→injected missing→read (`task-4-native-android-retained-status.log` and per-phase logs). Exact command template from app: `flutter test integration_test/account_storage_test.dart -d emulator-5554 --no-uninstall --dart-define=ACCOUNT_STORAGE_PHASE=<phase> --dart-define=ACCOUNT_STORAGE_ROOT=/data/user/0/dev.chatproject.chat_app/files/e2e/c4-native-retained-1792337816113976/storage`. Each read asserted same public identity/installation/transport and a different core instance from write. Generated secrets were never read into diagnostics. Locked/Missing remain injected status tests around actual Android Keystore, not physical OS lock/key destruction.

The public receipt alone was copied via owned `adb shell run-as ... cat .../bridge-receipt.json` after final read; actual package retention succeeded. Receipt (public keys hex):

```json
{
  "peer": "e84dbcde375db101d89c15bf2040a03c86d2b2b3bca196ba6217814eac7f8b3c",
  "installation": "eb110a8348008cabc0c4f23eaa53b2d567649d708d9d9a278fc66a286d9acac7",
  "transport": "bae81368f9f35aa15904b30a271ccd6400997a9bf5746600caadaa5aa47f079c",
  "instance": "8107-18dfaa3858e990c0"
}
```

## Current-source generation and Release evidence

The source is frozen in `task-4-source-freeze-final.json` (24 source files). The CI retention flag was the only amendment from the first freeze. Current broad Rust/Flutter checks precede only that covered CI command amendment and the separately native-tested Kotlin containment correction; no generated API or account-policy change followed. Final artifact builds use this same frozen source.

`C:/Program Files/Git/bin/bash.exe tools/gen_bindings.sh` exited 0 with Flutter on PATH. All 11 generated files have identical before/after SHA256 in `task-4-bindings-before.json` and `task-4-bindings-after.json`; no generated file is staged. Existing WindowsProtector private-type parser/lifetime and FVM/package notices remain in `task-4-bindings.log`, not suppressed or claimed as new failures.

The executed Android x86_64 Debug library contains all three exclusion markers (`chat-test-hooks:v1`, `account_local_`, `Local record fixture unavailable`); expected scanner exit1 is retained. Windows Debug executable contains privacyTest/failRestore/failPartial/failBitmap plus production setSensitiveScreen/privacy_unavailable. `task-4-debug-fixture-marker-proof.json` records hashes and exact booleans. An initial diagnostic incorrectly asserted that every Debug APK ABI had hook markers; its ARM64 member had none. The corrected evidence is explicitly restricted to the executed x86_64 ABI; no ARM64 Debug execution or freshness claim follows from that control. This diagnostic assumption failure is not a product RED.

Windows ordinary Release (`flutter build windows --release --target lib/main.dart -v`) exited0, 48.9s. Its entire Release tree was copied to `build/e2e/m3-c4-artifacts/windows-ordinary` before building the contract entry. Windows contract (`flutter build windows --release --target integration_test/release_contract_test.dart --dart-define=RELEASE_CONTRACT_NONCE=m3c4local1792337140795628 -v`) exited0, 15.4s, and its complete tree was independently copied to `windows-contract`. Both scans passed across 5 native files. Both actual runtime smokes exited0. The ordinary checker asserts account welcome/readiness (inherited output text is `Release Home/native smoke PASS; network DNS and traffic remain physical-device validation`); the contract checker verifies the exact build nonce, hooks NotAvailable, network stopped and endpoint null. Neither is public DNS/traffic evidence. No focus/restore/input action was used.

Commands from repository root for each copied Windows directory:

```text
python tools/ci/check_release_hooks.py build/e2e/m3-c4-artifacts/<windows-ordinary|windows-contract>
dart --packages=tools/e2e/.dart_tool/package_config.json tools/ci/release_smoke.dart --platform windows --artifact build/e2e/m3-c4-artifacts/windows-ordinary
dart --packages=tools/e2e/.dart_tool/package_config.json tools/ci/release_smoke.dart --platform windows --artifact build/e2e/m3-c4-artifacts/windows-contract --contract-nonce m3c4local1792337140795628
```

The full copied Windows trees are hashed in `task-4-windows-artifacts.json`. Both Release executables lack all four debug privacy fixture markers while retaining both production privacy markers. Each native build uses `CARGOKIT_TOOLCHAIN=1.92.0`, `CARGOKIT_VERBOSE=1`; the ordinary verbose log records the actual pinned rustup cargo command. Exact logs: task-4-windows-release.log, task-4-windows-release-markers.log, task-4-windows-release-smoke.log, task-4-windows-contract-build.log, task-4-windows-contract-markers.log, task-4-windows-contract-smoke.log.

The first final Android ordinary build was interrupted across a host restart. Its preserved log `task-4-android-release-build.log` stops at the armv7 cargo command, timestamp 2026-10-19 00:55:16+09, without a completion/exit. On resumption at21:02, the retained exec session was unknown, host boot time was20:54:48, and the current cargo/rustc/rustup/dart/java/emulator/qemu inventory and adb inventory were empty (`task-4-resume-inventory.json`). No current ordinary APK snapshot existed; the app-output APK dated the prior day22:59:09 and is excluded from final proof. No stale PID was signaled. The same frozen-source build was relaunched once under a new `task-4-android-release-build-resumed.log`; this is interruption recovery, not a failed assertion retry or timeout change.

## Final Android Release environment failure and correction

Ordinary Release smoke run `1792411550997007` failed exit1 with `Release readiness deadline exceeded`; its ownership process/activity arrays were empty after cleanup. The retained current-window XML (`build/e2e/1792411550997007/release/4512.stdout.log`, plus later fresh XML copies) shows the Android system dialog **Bluetooth keeps stopping**, with `android:id/aerr_close`. `dumpsys bluetooth_manager` records six crashes/restarts and OFF; current focus was `Application Error: com.google.android.bluetooth`. This is evidence of an obscured readiness window, not a passed welcome assertion.

On the newly acquired framework-owned `chat_e2e_1` only, the controller approved `adb -s emulator-5554 shell svc bluetooth disable` followed by `shell am force-stop com.google.android.bluetooth`. Both exited0 but the already shown error dialog remained; no smoke retry occurred at that point. A second narrowly approved action required a fresh exact Bluetooth error focus, matching title, and unique enabled/clickable `android:id/aerr_close`/Close app. Fresh bounds `[70,1028][1010,1154]` yielded one `adb shell input tap 540 1091` (exit0). The immediate focus query still showed a transition; the subsequent XML contained neither the dialog title nor close control, and the settled current focus was NexusLauncher. There was no second tap, host focus/input action, user-AVD modification, source change or timeout increase. Exact before/after receipts: task-4-owned-bluetooth-repair.log, task-4-bluetooth-tap-before.xml, task-4-bluetooth-tap-receipt.txt, task-4-bluetooth-after.xml, task-4-bluetooth-after-settled-window.txt. An initial `adb shell uiautomator dump /dev/tty` diagnostic returned only the dump confirmation; the existing smoke's correct `exec-out` form supplied the XML. That parsing attempt is retained as a diagnostic command mistake, not a product failure.

The failed run's `owned-app.log` also reports **Invalid component registrar / NoSuchMethodException** for `com.google.mlkit.common.internal.CommonComponentRegistrar`, `com.google.mlkit.vision.barcode.internal.BarcodeRegistrar`, and `com.google.mlkit.vision.common.internal.VisionCommonRegistrar` constructors. At this point these warnings remained an unresolved Release scanner concern; the targeted regression and constructor fix below subsequently resolved the actual decoder failure. Welcome alone did not prove the scanner worked. They are not labelled harmless or retrospectively identified as the readiness timeout cause. No unrelated shrinker/dependency change was made here.

## Final local validation closure and artifact provenance

The scheduled C4 local checks completed, with the retained failures and limitations above. The concrete Release scanner warning subsequently triggered an additional functional regression investigation before finalizing C4. Task review and whole-M3 review remain controller-owned; this is not M3 §10 completion. The C4 commit is `test(account): validate cross-platform account lifecycle`, based on `116ef3b83f24b967098056d73c2c06cc5143fcbf`. All final artifacts below derive from the frozen C4 source, not older C3 artifacts.

Android ordinary Release build resumed once after verified host restart and completed **exit0 /107.007s**, scanning **24 native members**. Android contract build completed **exit0 /50.801s**, scanning **24 native members**. Both APKs contain armeabi-v7a, arm64-v8a and x86_64; the ordinary verbose log records all three actual `rustup run 1.92.0 cargo build` target commands. Build/scan evidence covers all bundled ABIs; runtime proof is x86_64 only. Exact commands from app:

```text
flutter build apk --release --target lib/main.dart -v
flutter build apk --release --target integration_test/release_contract_test.dart --dart-define=RELEASE_CONTRACT_NONCE=m3c4local1792337140795628 -v
```

Ordinary APK was copied before contract overwrote app output. Commands from repository root:

```text
python tools/ci/check_release_hooks.py build/e2e/m3-c4-artifacts/ordinary-release.apk
python tools/ci/check_release_hooks.py build/e2e/m3-c4-artifacts/contract-release.apk
dart --packages=tools/e2e/.dart_tool/package_config.json tools/ci/release_smoke.dart --platform android --artifact build/e2e/m3-c4-artifacts/ordinary-release.apk --serial emulator-5554
dart --packages=tools/e2e/.dart_tool/package_config.json tools/ci/release_smoke.dart --platform android --artifact build/e2e/m3-c4-artifacts/contract-release.apk --serial emulator-5554 --contract-nonce m3c4local1792337140795628
```

After the verified one-shot system-dialog repair, ordinary run `1792411778516479` passed exit0 (`task-4-android-release-smoke-after-dialog.log`). Contract run `1792411825883032` passed exit0 (`task-4-android-contract-smoke.log`), verifying explicit nonce/hook NotAvailable/network stopped/endpoint-null checks in AOT. Each final smoke ownership file has empty processes/activities. The framework acquisition `1792411519815927` was verified as chat_e2e_1; no app PID or adb reverse remained before `adb -s emulator-5554 emu kill` exit0. Final adb inventory is empty (`task-4-release-adb-final.log`). All ownership records, including older uncertain records, remain retained; no stale PID was signaled and no scratch was deleted. The framework retained its existing no-snapshot-save invocation; no new snapshot was saved.

Artifacts under `build/e2e/m3-c4-artifacts`:

| Copied artifact | Bytes | SHA256 |
|---|---:|---|
| `windows-ordinary/chat_app.exe` | 123904 | `26a260b22ad0d78b9e616d7f6e1693afa4b38271348c6e45c7f1ea84e883a74a` |
| `windows-ordinary/chat_ffi.dll` | 16079360 | `56882826e7190908bdfafb1fd42fdfc7c177b4a15ec987fd6d51ac3dea45a053` |
| `windows-contract/chat_app.exe` | 123904 | `26a260b22ad0d78b9e616d7f6e1693afa4b38271348c6e45c7f1ea84e883a74a` |
| `windows-contract/chat_ffi.dll` | 16079360 | `56882826e7190908bdfafb1fd42fdfc7c177b4a15ec987fd6d51ac3dea45a053` |
| `ordinary-release.apk` | 108971570 | `fc78da0cf973f56c48b38b40ac19793b75165089be964b6597240e22122a7c59` |
| `contract-release.apk` | 98991534 | `2d52019188b898f419c611cb75807e7f5fbb2bc93012f7cf777192af80a7a68a` |

Full Windows tree member hashes are retained in `task-4-windows-artifacts.json`; APK native-member hashes are in `task-4-android-ordinary-artifact.json` and `task-4-android-contract-artifact.json`. The current Debug control SHA and exact marker booleans are in `task-4-debug-fixture-marker-proof.json`. No artifact was signed for distribution, uploaded, pushed or released.

Final Rust summary totals are 149 passed/1 ignored for workspace;262 passed for core hooks;14 passed/1 ignored for FFI hooks;5 passed for node harness;5 passed for host iOS DNS simulation. Both strict workspace and hook clippy exited0, plus the two optimized double-gate tests each1/1. Flutter134/134 and analyze No issues; Dart analyze No issues. Full Dart's retained 186pass/1skip/3 port-conflicted failures plus exact serial3/3 correction and final scenario9/9 are reported honestly, not represented as one clean full invocation. Python96 OK/1 existing skip, workflow checker and post-retention workflow30/30 passed. Native Windows and Android account tests each1/1 with awaited cleanup; each platform's five separate storage phases each1/1 (Android corrected --no-uninstall; original missing-receipt failure retained). Kotlin containment native3/3 follows its actual1/3 failure. Actual two-Android M3 run1792337140795628 completed all assertions and cleanup. Generated11 files are byte-identical. No covered broad suite was repeated after the interruption because its source hashes remained unchanged.

### Release scanner concern retained for review

The same warnings also occur in the **passing ordinary Release** run, `build/e2e/1792411778516479/release/owned-app.log` (PID7275), not only in the Bluetooth-obscured run (PID3213):

```text
W/ComponentDiscovery: Invalid component registrar.
Caused by: java.lang.NoSuchMethodException: com.google.mlkit.common.internal.CommonComponentRegistrar.<init> []
Caused by: java.lang.NoSuchMethodException: com.google.mlkit.vision.barcode.internal.BarcodeRegistrar.<init> []
Caused by: java.lang.NoSuchMethodException: com.google.mlkit.vision.common.internal.VisionCommonRegistrar.<init> []
```

At that stage the warning had not been exercised through the decoder; the subsequent targeted regression below establishes and fixes the actual Release failure. It is a concrete final-review concern, separate from KGP/Gradle notices and from the verified welcome/Release-contract checks. Neither emulator protocol pairing nor welcome readiness proves optical camera decoding.

## Release QR regression: actual failure, minimal fix and final amended artifacts

The earlier registrar warnings were not left as an external-camera limitation. Controller review required an actual Release image-decode regression before finalizing C4. Pinned mobile_scanner7.1.3's `analyzeImage` directly loads an app-owned PNG into `InputImage` and uses the same native barcode-scanner factory, without starting a camera or requesting permission. The existing Release contract now generates the public payload `chatproject-public-release-qr-v1` using existing QrPainter/dart:ui with a white margin, calls `MobileScannerController(autoStart:false).analyzeImage(..., formats:[qrCode])`, requires exactly one barcode with exact payload equality, and finally disposes the controller/deletes its own PNG and empty temporary directory. The existing nonce success receipt is emitted only after this check and cleanup. Windows skips only this Android-specific native decoder branch; its existing contract remains executed. No new dependency, API, FRB or CI harness was added; existing Android contract CI now exercises this regression.

Actual merged R8 configuration used firebase-components16.1.0's class-only `-keep class * implements com.google.firebase.components.ComponentRegistrar`; the usage report explicitly listed `public void <init>()` removed for all three warned registrars. mobile_scanner's `com.google.mlkit.*` rule does not match nested packages. The minimal fix appends the explicit constructor keep to the existing app ProGuard rules, matching [Firebase's official constructor-preservation correction](https://github.com/firebase/firebase-android-sdk/pull/6044/files). The [pinned scanner consumer rules](https://raw.githubusercontent.com/juliansteenbakker/mobile_scanner/v7.1.3/android/proguard-rules.pro) remain unchanged. All other optimization, shrinking, versions and rules remain enabled.

```proguard
-keep class * implements com.google.firebase.components.ComponentRegistrar {
    void <init>();
}
```

Behavioral **RED**, before changing the rule: `flutter build apk --release --target integration_test/release_contract_test.dart --dart-define=RELEASE_CONTRACT_NONCE=m3c4qr1792411825883032 -v` built successfully19.823s; copied `qr-red-release.apk` SHA256 `f79e4ce25ff78fcbcd452bcb90707a493e24495fd241df29f60ea998cda3417f`. Actual owned run `1792412195338567` failed exit1 with `Owned app reported a runtime exception`. Its `release/owned-app.log` contains all three registrar NoSuchMethodException messages, scanner MethodChannel NullPointerException, then `Bad state: Release public QR image decode mismatch` at `_checkQrImage` line99; no success nonce. This was an executing Release functional failure, not a compile-only test or inferred warning.

Behavioral **GREEN**, with only the constructor rule changed: identical fixture and nonce build17.317s exit0; all24 native-member scanPASS; actual run `1792412267417858` contractPASS exit0, nonce emitted only after exact image decoding. Its owned-app log has none of the registrar, NullPointer or unhandled-exception lines. The new usage report no longer lists those three constructors removed. Exact pre-fix, RED and GREEN R8 configuration/usage/mapping outputs and hashes remain under task-4-r8-before, task-4-r8-red, task-4-r8-green and associated hash manifests. The scoped `flutter analyze integration_test/release_contract_test.dart` reports No issues (`task-4-qr-analyze.log`).

Covering amended-source artifacts: ordinary Android build21.792s exit0,24native scanPASS and actual welcome smoke run `1792412381461146` PASS. Its owned-app log also has none of the prior registrar/NullPointer/unhandled-exception lines. Windows amended contract build20.933s exit0,5native scanPASS and runtime run `1792412430246777` PASS. Exact logs use task-4-qr-green-{build,markers,smoke}, task-4-qr-ordinary-{build,markers-corrected,smoke}, task-4-qr-windows-contract-{build,markers,smoke}. One artifact-copy command mistakenly combined PowerShell Set-Location with .NET relative GetFullPath and targeted a nonexistent parent directory; Copy-Item and the following missing-artifact scan failed. No artifact was written outside the worktree. The already successful ordinary build was copied with the exact absolute owned destination and scanned/launched without rebuilding; this command-authoring failure is retained, not mislabelled as a product RED.

All ordinary and contract APKs listed in the preceding sections are **pre-fix historical artifacts**, superseded for final-source acceptance by these two amended APKs. The previous Windows contract is likewise superseded by the amended entry build. The preserved ordinary Windows production tree remains valid: neither the Android-only R8 rule nor the test-only contract entry changes its build inputs; the controller explicitly required only the amended Windows contract covering check.

| Final amended artifact | Bytes | SHA256 |
|---|---:|---|
| `qr-contract-release.apk` | 99286446 | `e0cf0f7c2db47e7bb2818d5bc82a81371e36cbe8d8bef588d2397144189dc31a` |
| `qr-ordinary-release.apk` | 108971570 | `265e9e53adc001894003a354c11a9aecd64ee243f735c042743cdd54a53962f6` |
| `windows-qr-contract/chat_app.exe` | 123904 | `26a260b22ad0d78b9e616d7f6e1693afa4b38271348c6e45c7f1ea84e883a74a` |
| `windows-qr-contract/chat_ffi.dll` | 16079360 | `56882826e7190908bdfafb1fd42fdfc7c177b4a15ec987fd6d51ac3dea45a053` |

Final native-member/full-tree manifests: `task-4-qr-apk-artifacts.json`, `task-4-qr-windows-artifact.json`; final **26-file** source manifest: `task-4-source-freeze-qr-final.json`. The original24 source files are unchanged; the two added files are `app/android/app/proguard-rules.pro` and `app/integration_test/release_contract_test.dart`. Generated11/protected3 hashes remain unchanged. No Rust/binding input changed, so their completed broad suites/generation were not repeated. The original24-file freeze and all prior artifact hashes remain chronological proof, not mislabeled as final26-file artifacts.

The final QR-validation acquisition `1792412100493402` owned chat_e2e_1 only. RED, GREEN, ordinary Android and Windows contract ownership arrays all ended empty. Before owned emulator stop there was no app PID or adb reverse; verified `chat_e2e_1` stopped via `adb emu kill` exit0 (`task-4-qr-owned-cleanup.log`). No additional Bluetooth repair was needed in this acquisition. Original records and snapshots were not deleted. The fix/causal lesson is saved with project prefix in agentmemory `mem_mvf8131v_d939da8183b3` (android-r8-componentregistrar, mobile-scanner-release, public-qr-image-regression).

This resolves the observed Release decoder/registrar failure. Physical camera capture, permission UX, optical QR reliability, OS privacy and actual-device/platform gates remain unexecuted as scoped above; public image decoding is not substituted for those gates. C4 source/local validation is complete pending controller task and whole-branch reviews, while M3 §10 remains incomplete.

## C4 review fix 1: pre-write wire secrecy (2026-10-19)

The first C4 task review of `116ef3b..7498beb` returned 0 Critical, 1 Important (I1), and 1 Minor (M1). I1 was a real logging-boundary defect: valid JSON in the wrong inbound direction (`cmd`/`config`) bypassed parsing before recording, and non-account result/event maps could carry unknown nested fields into `wire.jsonl`. Existing account-result/error and malformed-JSON redaction did not cover those cases. No generated recovery words were used in the reproduction; all injected values were explicit public markers.

Fix scope is only `tools/e2e/lib/src/controller.dart` and `tools/e2e/test/account_scenario_test.dart`. Logging now checks direction before projection; every inbound record passes the existing strict top-level parser. Known public network, peer, connection-state and probe payloads are rebuilt from typed scalar fields and recursively projected peer lists; unknown fields are excluded and malformed/unknown payload shapes are redacted. Outbound config/command diagnostics are separately constructed. No input map/list is copied into these public payload projections. Account payloads, error details and context-free late results remain redacted. Intentional public M2 probe text and timing metrics remain visible, and full in-memory RPC/event consumers receive the original values. The protocol parser, actor, app and native providers are unchanged.

Commands ran from `tools/e2e` with `C:/tools/flutter/bin` on PATH. All evidence below is retained under `.superpowers/sdd/2026-10-18-m3-c-app-validation/`:

| Phase | Exact command | Actual outcome | Retained output |
|---|---|---|---|
| Behavioral RED, before production change | `dart test test/account_scenario_test.dart --name prelog` | exit1; 3 passed, 8 failed on persisted PUBLIC marker leakage | `task-4-fix-1-red.log` |
| Formatting | `dart format lib/src/controller.dart test/account_scenario_test.dart` | exit0; 2 files formatted | `task-4-fix-1-format.log` |
| Focused GREEN | `dart test test/account_scenario_test.dart --name prelog` | exit0; 11 passed | `task-4-fix-1-green-focused.log` |
| Covering GREEN | `dart test test/account_scenario_test.dart test/controller_test.dart test/scenario_test.dart` | exit0; 91 passed, no skips/failures | `task-4-fix-1-covering.log` |
| Analyzer | `dart analyze --fatal-infos` | exit0; No issues found | `task-4-fix-1-analyze.log` |

The eleven new cases cover inbound config, ordinary cmd, sensitive account cmd with unknown top-level data, already-safe hello/result/event top-level rejection, and nested result/network/peer/probe/incorrect-metric-type payloads. Rejection cases assert both RPC/controller failure and marker-free diagnostics; valid payload cases assert unchanged complete in-memory values alongside projected public metrics/probe text in the log. Existing account success/error/malformed/timeout-late controls and M2 restart/reconnect/replay/sequence/cleanup consumers are included in the 91-test covering run. Output contains ordinary test progress and expected test titles, with no unexpected warnings or failures.

`task-4-fix-1-source-freeze.json` supersedes the final26 source manifest for the amended tool source: exactly the two tool files above differ from `task-4-source-freeze-qr-final.json`; all other24 source hashes match. `task-4-fix-1-provenance.json` records generated11/protected3 hashes unchanged. App, Rust, binding, dependency, workflow and Release build inputs are unchanged, so the final amended APK and Windows manifests above retain their app-artifact provenance. No app/native/Rust/generation/Release checks were repeated and no new binary proof is claimed. Ruling appendix source hashes were refreshed against all three current ledgers, preserving every ruling-bearing paragraph (audit: `task-4-fix-1-rulings-audit.json`).

Self-review checked every changed source line against I1, compared public field names/types/enums with actual `NetworkInfo`/`PeerInfo`/`CoreEvent` and verified that receive, sequencing, pending-operation and consumer behavior are untouched. No extra dependency, protocol framework or adjacent cleanup was added. The secrecy lesson is saved in agentmemory `mem_mvf8sqdk_ca25e5a201cc` with `[ChatProject / M3 C4]` scope. M1's existing toolchain/expected-output follow-up stays deferred; physical camera/OS, Apple, public-network and signed-artifact acceptance gates remain open exactly as above. Fix 1 awaits controller re-review and whole-M3 review; this is not an M3 acceptance claim.
