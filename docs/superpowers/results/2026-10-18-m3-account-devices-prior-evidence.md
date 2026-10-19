# M3 prior task evidence appendix

These are retained excerpts from the accepted A1–C3 implementer reports, including their failed attempts, correction rounds, commands, platform limits and per-commit provenance. They are historical reports, not current C4 executions. The [main result](2026-10-18-m3-account-devices.md) distinguishes current checks and remaining gates. Original ignored reports and logs remain preserved for controller review; no scratch cleanup was performed.

### A1 — retained task-reported evidence

Source: `.superpowers/sdd/2026-10-18-m3-a-keys-storage/task-1-report.md`. Historical evidence at the identified commits; not a C4 rerun.


Status: DONE.
Commit: `4a7413a` — `feat(account): add recovery and device key derivation`.
Fix round 1 commit: `ddf0357` — `fix(account): gate fake clock hooks to debug builds`.
Base: `6050108f08f8855000c8ed309bbb50172fe63765`.
Workspace: `C:/Users/hyunw/orca/workspaces/ChatProject/m2-iroh-transport`.


- Added `account/{mod,types,keys,clock}.rs`: 32-byte zeroizing secret ownership without Debug/Clone/Serialize; raw-byte ordered identifiers with lowercase unpadded RFC4648 base32 Display; DeviceKind; private AccountKeys/DeviceKeys fields; public identifier/public-key getters and crate-private secret borrowing for later core tasks.
- Recovery is exactly 32-byte BIP39 English entropy, not the PBKDF2 64-byte seed. Uses `Mnemonic::from_entropy_in`, `parse_in`, `to_entropy`; NFKD from bip39's own normalizer, ASCII lowercase, and split-whitespace parsing. Rejects non-24-word inputs, invalid word/checksum, and oversized raw input before normalizing. Error details are fixed safe text.
- Controller ruling approved MAX_RECOVERY_INPUT_BYTES=4096 (raw UTF-8) and trusted Rust Secret32 construction/borrowing. Canonical, fullwidth/NFKD, uppercase, repeated-whitespace, exact-4096 and over-limit tests included. Crate-private unused secret getters have local dead_code allowances because signing/storage consumers are future serial tasks.
- Fixed RFC5869 HKDF-SHA256 salts/info from approved section3; Ed25519 public keys become account/installation identifiers. Device seed ownership stays with DeviceKeys; OS randomness uses `getrandom::fill`.
- `Clock: Send+Sync`, production SystemClock uses SystemTime/Instant; test FakeClock is available only under unit-test cfg or the existing test-hooks + debug_assertions double gate and separately controls wall and monotonic time. See fix round 1 below.
- Added all sixteen required core/FFI error variants, complete conversion/test mapping, and regenerated bindings through existing script. No account command/UI or A2 storage implementation added.


#### TDD evidence

Before production implementation, added public-vector integration tests and dev serde_json dependency. Command:

```powershell
cargo test -p chat_core --test account_keys
```

RED exit1, relevant output:

```text
error[E0432]: unresolved import `chat_core::account`
  could not find `account` in `chat_core`
error[E0433]: failed to resolve: use of unresolved module or unlinked crate `ed25519_dalek`
error[E0599]: no variant or associated item named `InvalidRecovery` found for enum `ErrorCode`
error: could not compile `chat_core` (test "account_keys") due to 5 previous errors
```

The other two diagnostics were cascading type inference errors for ID-byte iterators, caused by the missing account API. These are the expected unimplemented-API failures explicitly permitted by the brief. The converter test's sixteen new mappings were also added before the enum/converter production changes.

Initial post-implementation run: 6 passed/1 failed. The failure was in the test's non-echo helper: for invalid single-space input, generic safe error text also contains a space. Corrected the test to apply full-phrase non-echo assertion only to nontrivial input; production remained unchanged.

GREEN, same command, exit0:

```text
running 7 tests
random_secret_produces_independent_recoverable_entropy ... ok
invalid_word_checksum_and_23_or_25_rejected ... ok
entropy_round_trip_24_words ... ok
ids_are_52_lowercase_base32 ... ok
device_keys_are_distinct ... ok
recovery_input_byte_limit_is_checked_before_normalization ... ok
fixed_hkdf_domains ... ok
test result: ok. 7 passed; 0 failed; 0 ignored
```

Additional private unit assertions freeze all three supplied HKDF seed values. Three compile-fail doctests check that Secret32 cannot be debug formatted, cloned, or serde serialized. The test-hooks integration case checks independently controlled wall/monotonic time.


#### Other verification

- `cargo check --locked -p chat_core`: exit0, Finished dev profile, 10.50s, no compiler warnings.
- `cargo +1.92.0 check --locked -p chat_core`: exit0, Finished dev profile, 16.54s, no compiler warnings (waited for binding build lock).
- `& 'C:/Program Files/Git/bin/bash.exe' tools/gen_bindings.sh`: exit0. FRB 2.11.1 generation printed Done; build_runner wrote1 output; chat_ffi/chat_nse built; Kotlin and Swift generation completed. Only committed generated FRB Rust and Dart ErrorCode output changed. Kotlin/Swift output is unchanged. Generator informational messages about .fvmrc/no fvm and optional lifetime support were emitted; Flutter printed newer-version availability, without upgrading anything.
- `cargo test --workspace --locked --features chat_core/test-hooks`: exit0, 109 passed / 0 failed / 0 ignored, no compiler warnings. Build36.31s. Breakdown: core units53 (includes exact HKDF seeds), account integration8 (includes FakeClock), puppet7, transport17, FFI7 (includes all new errors), NSE9, fixture decrypt5, core compile-fail doctests3. Transport group30.21s. Required-feature chat_node harness suite was not enabled; no device/public-network harness was run.
- `rg -n 'to_seed|pbkdf|seed_normalized' core/chat_core/src/account core/chat_core/tests/account_keys.rs`: no matches; no BIP39 PBKDF2 seed API calls.
- `git diff --check`: exit0; Git emitted only existing Windows LF/CRLF conversion notices.
- Selective rustfmt applied to six new Rust files only; existing adjacent code was not reformatted.


#### Retained correction rounds

## Fix round 1 — debug-only FakeClock hook gate

Controller found that the initial FakeClock gate accepted feature="test-hooks" without debug_assertions, contrary to the existing global double gate. Verified other test utility patterns before changing code. Correction changes exactly3 cfg lines in2 files:

- clock.rs re-export and implementation module now use `cfg(any(test, all(feature = "test-hooks", debug_assertions)))`.
- account_keys.rs FakeClock integration test now uses `cfg(all(feature = "test-hooks", debug_assertions))`.

No timing/secret/identifier behavior or other feature changes. No binding regeneration necessary; public FFI is unchanged.

### RED evidence

Wrote an ignored, downstream Rust metadata probe at `.superpowers/sdd/2026-10-18-m3-a-keys-storage/fake-clock-release-probe.rs`:

```rust
use chat_core::account::clock::FakeClock;
fn main() {
    let _ = FakeClock::new(1_800_000_000, 0);
}
```

Before the correction:

```powershell
cargo check --release --locked --features test-hooks -p chat_core
```

Exit0, release profile21.00s. Then the probe command:

```powershell
$clockMetadata = Get-ChildItem -LiteralPath 'target/release/deps' -Filter 'libchat_core-*.rmeta' | Sort-Object LastWriteTime -Descending | Select-Object -First 1
Write-Output "Release metadata: $($clockMetadata.Name)"
rustc --edition 2024 --crate-name fake_clock_release_probe --emit=metadata -L dependency=target/release/deps --extern "chat_core=$($clockMetadata.FullName)" --out-dir .superpowers/sdd/2026-10-18-m3-a-keys-storage .superpowers/sdd/2026-10-18-m3-a-keys-storage/fake-clock-release-probe.rs
$clockProbeExit = $LASTEXITCODE
Write-Output "Probe rustc exit: $clockProbeExit"
if ($clockProbeExit -eq 0) { Write-Error 'RED: release/test-hooks unexpectedly exposes FakeClock'; exit 1 }
exit $clockProbeExit
```

Relevant output:

```text
Release metadata: libchat_core-be11a5c5e5d65ae5.rmeta
Probe rustc exit: 0
Write-Error: RED: release/test-hooks unexpectedly exposes FakeClock
```

Wrapper exit1 is the expected RED regression: release import compiled, proving unintended visibility.

### GREEN evidence

After changing the three cfg lines:

1. `cargo test --locked -p chat_core --features test-hooks --test account_keys`: exit0,8 passed/0 failed, including fake_clock_controls_wall_and_monotonic_time_independently. Build7.75s; tests0.00s, no compiler warnings.
2. `cargo check --release --locked --features test-hooks -p chat_core`: exit0, release profile0.95s, no warnings.
3. Re-ran the identical rustc probe against the newly rebuilt release metadata. Expected rejection was validated with this wrapper:

```powershell
$clockMetadata = Get-ChildItem -LiteralPath 'target/release/deps' -Filter 'libchat_core-*.rmeta' | Sort-Object LastWriteTime -Descending | Select-Object -First 1
Write-Output "Release metadata: $($clockMetadata.Name)"
$clockProbeOutput = & rustc --edition 2024 --crate-name fake_clock_release_probe --emit=metadata -L dependency=target/release/deps --extern "chat_core=$($clockMetadata.FullName)" --out-dir .superpowers/sdd/2026-10-18-m3-a-keys-storage .superpowers/sdd/2026-10-18-m3-a-keys-storage/fake-clock-release-probe.rs 2>&1
$clockProbeExit = $LASTEXITCODE
$clockProbeOutput | Write-Output
Write-Output "Probe rustc exit: $clockProbeExit"
if ($clockProbeExit -ne 1 -or (($clockProbeOutput | Out-String) -notmatch 'no `FakeClock` in `account::clock`')) { Write-Error 'Unexpected release visibility probe result'; exit 1 }
Write-Output 'GREEN: release/test-hooks FakeClock import rejected as expected'
exit 0
```

Relevant output, wrapper exit0:

```text
error[E0432]: unresolved import `chat_core::account::clock::FakeClock`
no `FakeClock` in `account::clock`
note: found an item that was configured out
33 | #[cfg(any(test, all(feature = "test-hooks", debug_assertions)))]
34 | pub use fake::FakeClock;
Probe rustc exit: 1
GREEN: release/test-hooks FakeClock import rejected as expected
```

4. `cargo check --release --locked --features test-hooks -p chat_core --test account_keys`: exit0, release profile7.32s, no warnings. This verifies that the adjusted integration-test gate compiles when FakeClock is excluded from the dependency.
5. `git diff --check`: exit0 (only Windows LF/CRLF notices). Self-review confirms exactly the requested3 cfg changes; pre-existing M2 docs and ignored oracle tooling preserved. No full-suite rerun because only the focused gate changed; covering debug tests, release library/integration checks, and actual downstream visibility proof passed.

Fix commit: `ddf0357` — `fix(account): gate fake clock hooks to debug builds`. Exactly2 files,3 insertions/3 deletions. Post-commit check confirms only the three pre-existing M2 documents are dirty; task files and index are clean.


### A2 — retained task-reported evidence

Source: `.superpowers/sdd/2026-10-18-m3-a-keys-storage/task-2-report.md`. Historical evidence at the identified commits; not a C4 rerun.


Status: DONE. Base: `ddf03579c6bd78cf71d989a27c2d4e42101c3b12`.
Workspace: `C:/Users/hyunw/orca/workspaces/ChatProject/m2-iroh-transport`.
Commit: `96c23f9` — `feat(account): persist encrypted account snapshots atomically`.


- Added the internal Rust `Store`, `KeyProtector`, and zeroizing `StoredPlaintext` boundary. No FFI command, OS adapter, UI, generic reset, typed AccountState codec, or future B-type dependency was added.
- A first candidate commit initializes random path-independent 16-byte `store.id`, OS-protected `key.wrap`, and AES-256-GCM `account.bin`. CBOR envelope is exactly `[1, store_id16, installation32, nonce12, ciphertext_with_tag]`. Each commit uses a fresh random 96-bit nonce and AAD `b"ChatProject/store/v1" || [1] || store_id16 || installation32`.
- All file sizes are inspected before allocating their contents, and bounded reads also reject growth beyond 4 MiB. Candidate size is rejected before initialization/writing; the largest encoded snapshot is exactly 4 MiB. Payload collection limits remain with A5/B2/B4/B6.
- The fixed `.account.lock` is opened in the canonical folder and exclusively `File::try_lock`ed before any protector call or secret read. Windows/Android retain ownership for the entire Store lifetime, including suspension and failed validation; iOS uses finite operation locks. Methods require `&mut self`. Suspend refuses storage operations; only iOS releases ownership. Resume reuses persistent ownership or reacquires transient iOS ownership, then validates the last complete snapshot before allowing access. This paragraph corrects the initial implementation's cross-platform suspension bug identified in review I1; see fix round 1 below.
- Same-folder temporary writes use `write_all`, `sync_all`, close, then replacement. Windows uses `MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)`; Unix uses rename followed by parent-directory sync. Snapshot temporaries contain ciphertext. Encryption preallocates tag capacity before copying plaintext, avoiding plaintext-bearing Vec growth/reallocation.
- Missing IDs/wrappers, unavailable OS keys, malformed/authentication-failed snapshots, and partial initialization never become fresh accounts or trigger key regeneration. Existing corrupt snapshots cannot be overwritten by `commit`.
- `discard_pending` is crate-private physical cleanup, documented to require future AccountManager/B6 never-active authorization. It can clean an encrypted candidate snapshot, preserves the fixed lock/unrelated files, retains the ID until the remaining known artifacts are removed, and returns errors on OS/file cleanup failure. An ID-only temp is explicitly cleanable because ID durability precedes OS protection. Missing IDs alongside protected/ciphertext resources are refused. Protector deletion must be idempotent for absent resources.
- Added only the pinned AES-GCM/minicbor dependencies and Windows-target-specific pinned windows-sys feature. Lockfile adds minicbor 0.26.5/minicbor-derive 0.16.2; no existing dependency versions were upgraded.


#### Tests and command evidence

Commands ran in PowerShell in the workspace above, with Rust 1.92.0 on Windows x86_64 MSVC.

### TDD RED

1. `cargo test -p chat_core --test account_storage` initially failed with unresolved `account::storage` and minicbor imports, confirming the missing API/dependency boundary. After adding compile-only API stubs, the same command reached behavioral failures:

   ```text
   running 8 tests
   encrypted_round_trip_random_nonce_and_relocated_store ... FAILED
   missing_wrapping_key_never_creates_new_account ... FAILED
   tamper_aad_nonce_version_rejected ... FAILED
   limits_preserve_old_snapshot ... FAILED
   two_processes_canonical_folder_one_writer ... FAILED
   suspend_blocks_access_and_resume_reacquires_ownership ... FAILED
   incomplete_initialization_is_not_fresh_and_unknown_files_survive ... FAILED
   not yet implemented
   test result: FAILED. 1 passed; 7 failed
   ```

   The one passing function is the child-process entry point when invoked without child arguments. The failures came from the deliberately unimplemented storage API, rather than assertions about fake-only behavior.

2. `cargo test --locked -p chat_core --lib account::storage` with private cleanup tests and the still-unimplemented API:

   ```text
   authorized_pending_snapshot_cleanup_preserves_lock_and_unrelated_files ... FAILED
   failed_pending_cleanup_remains_incomplete_instead_of_fresh ... FAILED
   not yet implemented
   test result: FAILED. 0 passed; 2 failed
   ```

3. `cargo test --locked -p chat_core --lib account::storage::atomic` before implementation:

   ```text
   crash_at_each_replace_boundary_keeps_complete_snapshot ... FAILED
   not yet implemented
   test result: FAILED. 1 passed; 1 failed
   ```

4. The real storage implementation passed the initial acceptance tests. Added the private injected I/O failure regression before wiring injected failures into the actual I/O result path. `cargo test --locked -p chat_core --lib account::storage`:

   ```text
   flush_and_replace_failures_are_errors_and_preserve_old_snapshot ... FAILED
   sync failure reported success
   test result: FAILED. 5 passed; 1 failed
   ```

5. Added pre-protection orphan cleanup and initialization crash cases before fixing missing-ID cleanup. Same focused unit command:

   ```text
   cleanup_pre_protection_orphan_without_committed_id ... FAILED
   initialization_crashes_remain_explicit_orphans_until_cleanup ... FAILED
   CoreError { code: StorageMissing, detail: "Account storage file unavailable" }
   test result: FAILED. 6 passed; 2 failed
   ```

6. Self-review added an independent exact-format/AAD regression. A temporary mutation changed the production domain `/v1` to `/v0`. `cargo test --locked -p chat_core --test account_storage envelope_decrypts_with_independently_constructed_required_aad` failed authentication as intended:

   ```text
   envelope_decrypts_with_independently_constructed_required_aad ... FAILED
   called Result::unwrap() on an Err value: Error
   test result: FAILED. 0 passed; 1 failed
   ```

   The domain was immediately restored to `/v1`; the independent test constructs literal required AAD and directly decrypts the actual file, without calling storage encoding/AAD helpers.

### GREEN and final verification

- `cargo test --locked -p chat_core --lib account::storage` after the final storage fixes:

  ```text
  running 8 tests
  crash_child ... ok
  cleanup_pre_protection_orphan_without_committed_id ... ok
  failed_pending_cleanup_remains_incomplete_instead_of_fresh ... ok
  authorized_pending_snapshot_cleanup_preserves_lock_and_unrelated_files ... ok
  ios_policy_simulation_releases_between_operations_and_during_suspend ... ok
  flush_and_replace_failures_are_errors_and_preserve_old_snapshot ... ok
  initialization_crashes_remain_explicit_orphans_until_cleanup ... ok
  crash_at_each_replace_boundary_keeps_complete_snapshot ... ok
  test result: ok. 8 passed; 0 failed
  ```

- `cargo check --locked -p chat_core`: exit 0, `Finished dev profile`, no compiler warnings.
- `cargo test --locked -p chat_core`: exit 0, default core suite run once after focused RED/GREEN:

  ```text
  unit tests:           57 passed; 0 failed
  account_keys:         7 passed; 0 failed
  account_storage:     12 passed; 0 failed
  puppet:               0 tests (feature gated)
  puppet_disabled:      1 passed; 0 failed
  doc tests:            3 passed; 0 failed
  ```

  Total 80 passing test functions, including two no-op child entry points; no warnings or failures. Public DHT/relay endpoints were not started. Existing transport unit tests use their local profile. Controller agreed that the unchanged feature-gated transport harness need not be repeated for A2.

- After the self-review exact-AAD regression and restoration of its temporary mutation, `cargo test --locked -p chat_core --test account_storage`: exit 0, `13 passed; 0 failed`, no warnings. This is the only added test after the full-suite run; final production code is unchanged from the full-suite pass.
- `rustfmt --edition 2024` on the newly owned storage/test files succeeded; `git diff --cached --check` succeeded with no whitespace diagnostics.

### Coverage notes

- Real child processes terminate with code 86 at six replacement boundaries: before write, after write, after file sync, before replace, after replace, after directory sync. Reopening produces the complete expected old/new snapshot, and a later commit succeeds despite orphan ciphertext.
- Six separate first-initialization crash boundaries cover ID temp/commit, after OS protection before wrapper writing, wrapper temp/commit, and account candidate before replacement. None reopens fresh; explicit private cleanup permits fresh state afterward.
- The two-process canonical-folder case gives the losing process a protector that panics on any secret operation. WriterBusy must happen first.
- Tampering covers version, store ID, installation/AAD, nonce, ciphertext, tag, and trailing bytes. A separate direct decryption proves the required domain and fixed field order.
- Boundary tests prove a 4 MiB encoded snapshot is accepted, one additional payload byte is rejected, the old snapshot survives rejected commits, and every storage file is bounded before protector access.
- Missing files and unavailable OS wrapping keys preserve the old snapshot; no new key is generated. Partial OS protect initialization and file-delete cleanup failure remain explicit incomplete/error states.


#### Self-review and limitations

Read the complete scoped staged diff, then checked the committed file list and verified no remaining diff in A2 paths. Corrected the pre-protection ID-temp cleanup gap and reserved cipher tag capacity to avoid plaintext reallocation. Added the independent AAD mutation regression because symmetric round trips alone would accept a wrong domain on both sides. The final commit contains only A2 files/dependency registration. Post-commit status retains exactly the original three dirty M2 documentation paths, with no staged changes.

Windows file locking/replacement and host subprocess crashes have native host evidence. The iOS ownership policy is simulated using Windows real file locks; native iOS/Android filesystem behavior was not executed. OS key protection remains the A3 integration; the fake protector is compiled only in tests. Unsupported File::try_lock errors map to StorageLocked and are not treated as ownership success.

As with atomic replacement generally, an error after replacement/physical deletion can mean that the operation completed but its durability could not be confirmed. No success is reported in that case. Future B6 owns durable discard intent and UI reconciliation; this task does not add a generic journal or active-account deletion authorization. Corrupt/missing IDs with external protected resources remain explicit unavailable/orphan states requiring reconciliation rather than guessed cleanup.



#### Retained correction rounds

## Fix round 1 — Important I1: persistent ownership through suspension

Review: `.superpowers/sdd/2026-10-18-m3-a-keys-storage/task-2-review.md`, read in full against base `96c23f9`.

The initial implementation incorrectly generalized iOS release-on-suspend behavior to Windows/Android. There was no controller authorization for this exception to Store-lifetime ownership. `Ownership::suspend` now retains the persistent lock, and `Ownership::resume` reuses it rather than attempting a new lock. Failed Store validation calls suspend without surrendering that persistent ownership. The iOS policy still reacquires transient ownership and releases it on success/error/suspend.

Changed only `core/chat_core/src/account/storage/ownership.rs` and `core/chat_core/tests/account_storage.rs`, plus this ignored report. Persistent-policy integration cases are excluded on iOS, whose finite policy remains covered by the explicit host simulation. The Command import follows the same platform gate to avoid introducing an unused import on iOS.

### RED reproduction before production fix

Changed the tests first to require WriterBusy while the persistent owner is suspended, after failed OS-key validation, and from a competing child process during suspension. Ran:

```text
cargo test --locked -p chat_core --test account_storage

running 13 tests
suspend_blocks_access_and_retains_writer_until_drop ... FAILED
unavailable_os_key_never_regenerates_or_overwrites_snapshot ... FAILED
two_processes_canonical_folder_one_writer ... FAILED

assertion failed: matches!(Store::open(dir.path(), protector()),
    Err(CoreError { code: ErrorCode::WriterBusy, .. }))

ownership_child: second process acquired writer ownership

test result: FAILED. 10 passed; 3 failed; 0 ignored; 0 measured; 0 filtered out
exit 1
```

The failures directly establish the unwanted ownership handoff, rather than inspecting a held-lock field. The updated child-process test also covers failed corrupt-snapshot validation; both that case and the unavailable-key case verify a competing owner becomes possible only after the original Store drops.

### GREEN after fix

```text
cargo test --locked -p chat_core --test account_storage

suspend_blocks_access_and_retains_writer_until_drop ... ok
unavailable_os_key_never_regenerates_or_overwrites_snapshot ... ok
two_processes_canonical_folder_one_writer ... ok
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
exit 0

cargo test --locked -p chat_core --lib account::storage

ios_policy_simulation_releases_between_operations_and_during_suspend ... ok
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 49 filtered out
exit 0

cargo check --locked -p chat_core

Checking chat_core v0.1.0
Finished dev profile [unoptimized + debuginfo] target(s)
exit 0
```

Compiler/test output had no warnings. The private iOS simulation now additionally proves ownership is available during suspension and after failed snapshot validation, while storage access remains blocked. Native iOS/Android execution remains outside this fix's evidence.

Applied rustfmt to the two owned files and reviewed their complete diff. `git diff --check` passed. Per controller instruction, the unchanged whole core suite was not repeated for this localized fix; no new failures warranted it. The original three dirty M2 documentation files remain untouched and unstaged.

After gating the test-only Command import for iOS, reran `cargo test --locked -p chat_core --test account_storage` (13 passed, 0 failed), `cargo test --locked -p chat_core --lib account::storage::ownership` (1 passed, 0 failed, 56 filtered out), and `cargo check --locked -p chat_core` (exit 0, Finished dev profile). Output remained free of compiler/test warnings.

Fix commit: `56109b2` — `fix(account): retain persistent writer ownership during suspension`. Commit includes only the two specified source/test files. Post-commit status retains exactly the original three dirty M2 documentation paths, with no staged changes.


### A3 — retained task-reported evidence

Source: `.superpowers/sdd/2026-10-18-m3-a-keys-storage/task-3-report.md`. Historical evidence at the identified commits; not a C4 rerun.


Status: **DONE_WITH_CONCERNS** (iOS compilation/runtime and physical hardware validation remain unexecuted).
Base: `56109b283b0347a3f28518cc3b31413b6657f188`.
Workspace: `C:/Users/hyunw/orca/workspaces/ChatProject/m2-iroh-transport`.
Commit: `4bec5891c1f386a3209aba588b9e211b87d4dd66` — `feat(account): protect storage keys with platform facilities`.
Post-commit status contains only the three pre-existing unstaged M2 docs; no A3 source remains uncommitted. This local `.superpowers` report/log directory is intentionally ignored, consistent with A1/A2 reports.


- Rust `platform_keys::provider(&Path) -> Result<Arc<dyn KeyProtector>, CoreError>` chooses actual OS facilities. No production fake adapter or Dart secret access was added. StoreId is the A2 public 16-byte ID; protection is independent of filesystem relocation. A2 ownership/suspend invariants were not changed.
- Windows: current-user DPAPI with UI forbidden, namespace + StoreId optional entropy, bounded blobs, zeroized returned native allocation followed by `LocalFree`. The adapter has no external per-store resource to delete; deletion is idempotent and Store removes `key.wrap`. Existing copies of a DPAPI blob are not revoked by adapter deletion. The test removes the wrapped file and proves Store cannot reopen its key.
- Android: Keystore AES-256-GCM alias `ChatProject.account.<lowercase StoreId hex>`, fresh 12-byte IV, StoreId AAD, versioned 61-byte wrapper, strict 32-byte plaintext, max 16 KiB boundary. Unprotect never generates a missing key. Only `StrongBoxUnavailableException` falls back after a supported-feature StrongBox attempt; other exceptions become structured failures. Existing-key `KeyInfo` supplies the actual protection level, not a hardware claim inferred from device features. Exact alias deletion is idempotent.
- Android JNI: existing network initialization still runs first. Managed initialization caches the AccountKeyStore object as a persistent GlobalRef. Every worker attaches independently and uses its own JNIEnv/local frame; no custom-class FindClass from a native worker. Java plaintext arrays and Rust temporary buffers are cleared. R8 keeps the helper and existing MainActivity native entrypoints.
- iOS: real Runner Keychain implementation with fixed C ABI `cp_key_protect/unprotect/delete`, 0/1/2/3/4/5 status convention, zero out length on failures, pointer borrowing only during calls. Item stores a random versioned marker plus the 32-byte storage key. The marker is compared before release, so scope substitution/tampering fails. Existing items are never silently overwritten. Accessibility is AfterFirstUnlockThisDeviceOnly, synchronizable=false, and explicit `$(AppIdentifierPrefix)dev.chatproject.chatapp.account` access group. Swift sources/tests are registered in their respective Sources lists; all three Runner configs explicitly use Runner.entitlements. Info.plist expands the same access group for runtime queries. NSE source/entitlements are untouched.
- Storage path-only channel: fresh Windows data uses LocalAppData/ChatProject/account; Android uses noBackupFilesDir/account; iOS uses existing App Group/account with isExcludedFromBackup set. Any of A2's six committed/temporary M3 artifacts in the old support/core folder preserves that location. No migration/overwrite occurs. Explicit dataDir continues to be respected. Android legacy support/core is excluded in both legacy backup and Android 12+ cloud/device-transfer rules, in addition to allowBackup=false.
- Native `setSensitiveScreen(bool)` enables Android FLAG_SECURE and iOS scene-switch overlay. C2 can call the Dart boolean-only helper. Android protectionLevel accepts public StoreId and returns a capability string. No account screen was built.


#### TDD evidence

### Windows actual DPAPI

Command: `cargo test -p chat_ffi platform_keys::windows`.
Tests were written against compile-only unavailable stubs before the DPAPI implementation.

```text
RED: round_trip_randomized_and_scope_bound ... FAILED
RED: deletion_is_idempotent_and_invalid_blob_rejected ... FAILED
RED: restart_reads_persisted_blob ... FAILED
RED: missing_wrapped_key_cannot_reopen_store ... FAILED
called Result::unwrap() on Err: CoreError { code: NotAvailable, detail: "Not implemented" }
invalid blob: left NotAvailable, right StorageCorrupt
test result: FAILED. 0 passed; 4 failed; 1 ignored

GREEN: all four tests ... ok
child process: restart_child ... ok (1 passed)
test result: ok. 4 passed; 0 failed; 1 ignored
```

The ignored child helper is explicitly spawned by the restart test and actually passes; it is not skipped restart coverage. Tests exercise real Windows DPAPI, randomized protection, StoreId binding, tampering, malformed/oversize wrappers, file loss, and a fresh process opening the persisted blob. Different Windows users/machines were not provisioned or tested.

### Android native / JNI

Common command, from `app/android`, with task-local environment:

```powershell
$env:JAVA_HOME='C:/Program Files/Android/Android Studio/jbr'
$env:FLUTTER_ROOT='C:/tools/flutter'
$env:ANDROID_SERIAL='emulator-5580'
./gradlew.bat :app:connectedDebugAndroidTest `
  '-Pandroid.testInstrumentationRunnerArguments.class=dev.chatproject.chat_app.AccountKeyStoreTest' `
  '-Ptarget-platform=android-x64' --console=plain
```

Initial native tests were authored against unavailable Kotlin stubs. After resolving the build prerequisites above:

```text
RED invalidInputsAndCapacityDoNotCreateKeys: expected:<5> but was:<4>
RED roundTripScopeTamperAndDelete: expected:<0> but was:<4>
Starting 3 tests ... 2 failures
```

After actual Keystore implementation those three tests passed (the restart test intentionally does nothing without its explicit phase; separate runs below supply actual persistence coverage). A new JNI worker test first hit a fixture error constructing Activity off the main thread; fixture construction was moved to runOnMainSync without launching any UI. Against the compile-only false-returning native test scaffold:

```text
RED nativeWorkerUsesCachedHelperAndThreadLocalEnvironment: java.lang.AssertionError
Starting 4 tests ... 1 failure
```

The test helper then exercised real provider operations on a spawned Rust thread. It is gated by **both** `debug_assertions` and existing `test-hooks`; it returns only Boolean and uses a synthetic fixture. It adds no production secret getter/RNG/approval hook.

```text
GREEN/final: Starting 5 tests on chat_e2e_1(AVD) - 16
Finished 5 tests ...
BUILD SUCCESSFUL in 22s
XML: tests="5" failures="0" errors="0" skipped="0"
Measured account wrapping protection: software
```

Final tests cover scope substitution with a real other alias, changed ciphertext, deleted alias, invalid lengths/capacity without key creation, actual measured capability/missing key, no-backup root and six legacy artifacts, plus Rust worker JNI roundtrip/tamper/delete. Hardware/StrongBox fallback behavior is implemented but real StrongBox/TEE hardware is **not** proven by this software emulator.

Fresh artifact: `app/build/chat_ffi/build/x86_64-linux-android/debug/libchat_ffi.so`, 255757496 bytes, local write time 2026-10-18 14:59:15.
SHA-256: `5C299D0F4BD62601DFB1D26C352A3357E84C65399D1B3C1B933F8727CBD9B7FA`.
NDK llvm-nm confirmed both initialization and test-native-worker JNI symbols in that artifact. Final instrumentation really invoked the Rust worker bridge (0.054 seconds in the test XML). Gradle's corrected build also compiled the i686 target in its existing configuration.

### Android process restart

Gradle UTP uninstalls instrumentation after its run, so an initial direct am instrument returned `Unable to find instrumentation info`. Installed the just-built debug app and androidTest APKs onto **only owned emulator-5580**, then ran:

```text
adb -s emulator-5580 shell am instrument -w
  -e class dev.chatproject.chat_app.AccountKeyStoreTest#restartPersistence
  -e accountPhase write dev.chatproject.chat_app.test/androidx.test.runner.AndroidJUnitRunner
OK (1 test), Time: 0.081

adb -s emulator-5580 shell am force-stop dev.chatproject.chat_app

adb -s emulator-5580 shell am instrument -w
  -e class dev.chatproject.chat_app.AccountKeyStoreTest#restartPersistence
  -e accountPhase read dev.chatproject.chat_app.test/androidx.test.runner.AndroidJUnitRunner
OK (1 test), Time: 0.071
```

Only the random StoreId and encrypted wrapper persisted in the fixture; read phase deleted the alias/fixture. No physical phone was connected/accessed. Owned emulator was newly started as headless, read-only `chat_e2e_1`, port 5580, launcher PID 25800. Stopped via `adb -s emulator-5580 emu kill`; PID 25800 no longer existed. No AVD directory was deleted; emulator used read-only/no-snapshot-save flags. No GUI/public DHT/relay/remote push/CI/distribution was used.

### Dart path / prerequisite error tests

- Path test first failed to compile because accountStorageRoot did not exist; after implementation, fresh LocalAppData root and all six legacy artifact preservation cases passed.
- The new account error test first reproduced the existing nonexhaustive-switch compilation error. After fixed mappings it passed. The strengthened no-enum assertion then failed with `Actual: storageCorrupt · 계정 작업을 완료할 수 없어요...`; adding the account-only display branch made it pass while preserving M2 behavior.
- `flutter test test/account_storage_path_test.dart test/network_model_test.dart --no-pub`: 8 passed.
- Final covering rerun after no-enum branch: `flutter test test/network_model_test.dart --no-pub`: 7 passed. The account-error test iterates all sixteen new codes and asserts exact fixed Korean output, no enum/detail, and cleared busy state.
- `flutter analyze lib/core_client.dart lib/network/network_model.dart test/account_storage_path_test.dart test/network_model_test.dart --no-pub`: **No issues found**. One initial braces lint was corrected before the final clean run.


#### iOS and remaining validation gate

The initial XCTest file was authored **before** AccountKeyStore.swift. No xcodebuild or Swift toolchain is available on this Windows host. iOS Swift/C ABI linking, Rust iOS target compilation, Keychain execution, scene-overlay UI, simulator persistence, signing-profile/access-group compatibility, and first-unlock physical behavior are **not executed/proven**. Later tests added attribute/backup policy and duplicate-item protection assertions; those too are unexecuted. No fake test adapter substitutes for Security APIs.

On an owned Mac/simulator, run the brief's command:

```text
xcodebuild test -workspace app/ios/Runner.xcworkspace -scheme Runner
  -destination 'platform=iOS Simulator,name=<owned simulator>'
  -only-testing:RunnerTests/AccountKeyStoreTests
```

For restart coverage, run the persistence test in two separate processes with test environment `ACCOUNT_KEY_TEST_PHASE=write` then `read` (configure the test runner environment/.xctestrun; do not assume arbitrary shell variables automatically reach XCTest). Preserve the app's fixture between runs. C4 must verify the signed Runner entitlement and profile include the new keychain access group; NSE must continue without it. Simulator cannot prove pre-first-unlock behavior or physical key protection. Android physical StrongBox/TEE and device backup behavior likewise remain C4 checks. Privacy screen presentation remains a native hook for C2 UI/manual validation, and A4 must supply the real bounded lifecycle ABI.

Official API references used: [DPAPI](https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata), [Android Keystore](https://developer.android.com/privacy-and-security/keystore), [JNI tips](https://developer.android.com/ndk/guides/jni-tips), [Apple accessibility](https://developer.apple.com/documentation/security/ksecattraccessibleafterfirstunlockthisdeviceonly).


#### Retained correction rounds

## Fix round 1 — review Important I1 and I2

Status: **DONE_WITH_CONCERNS**, retaining the same iOS native/C4 validation gate. Base `4bec5891c1f386a3209aba588b9e211b87d4dd66`. Scoped fix commit `5159604af76175dbcb62b3f5e6e0c17f5d70b277` — `fix(account): erase owned native key buffers and mark restart phases`. Post-commit status contains only the same three pre-existing unstaged M2 docs.

### I1: actual iOS application-buffer ownership and cleanup

The initial deferred Swift `Data.resetBytes` did not establish that a retained dictionary/bridged alias was erased. Replaced both plaintext paths with a small production `withAccountKeyBuffer` scope owning one 49-byte **NSMutableData reference**. The Keychain input dictionary retains that exact mutable object; deferred `NSMutableData.resetBytes(in:)` modifies that object in place on every body return, rather than detaching a Swift Data value. Protect fills and copies through its raw pointers, without introducing a Swift Data plaintext value.

Unprotect obtains Security's immutable result in a separate `copyKeychainValue` helper/autoreleasepool, copies bytes into the application-controlled mutable object, and releases its local CFTypeRef/NSData references before returning to marker validation/output. Cleanup erases the application-controlled buffer on success, Keychain failure, malformed result, and marker mismatch. **Security/framework-internal copies cannot be promised erased**; source comments explicitly state this boundary. No mutation of immutable CFData or false claim of erasing framework memory was added.

Added `testOwnedKeyBufferErasesRetainedAliasOnSuccessAndFailure` before the production helper implementation. It fills the real buffer with nonzero bytes, retains the exact NSMutableData object in a kSecValueData dictionary, verifies object identity, returns each status 0/1/2/3/4, then checks the retained alias's full 49 bytes are zero. Removing the deferred cleanup would leave the retained bytes nonzero. This covers the production cleanup primitive used by both native paths; no mock Keychain, secret getter, or production test hook was added.

**iOS TDD limitation:** test authored first; neither native RED nor GREEN, Swift syntax/typecheck, Security execution, nor C ABI link was executed on Windows. No swift/xcodebuild command is available. Source ownership review was performed; Python plistlib parsed Runner Info.plist/entitlements and ElementTree parsed the Runner test scheme XML. These are configuration checks only, not Swift compilation. Mac execution of the new alias-retention regression and existing actual Keychain tests remains required at the existing platform gate.

Reference checked for method semantics: [official Swift Foundation NSMutableData implementation](https://github.com/swiftlang/swift-corelibs-foundation/blob/main/Sources/Foundation/NSData.swift) implements reference-type resetBytes on mutableBytes. This source inspection does not substitute for Darwin Foundation runtime evidence.

### I2: explicit restart phase status

Android now calls a JUnit assumption when accountPhase is absent, and explicitly asserts that a provided phase is exactly write or read before touching the fixture. iOS throws XCTSkip when ACCOUNT_KEY_TEST_PHASE is absent, and fails immediately for an unknown provided phase. Existing separate-process write/read behavior is preserved.

Observed incorrect baseline (not a failing suite): ran the unmodified Android AccountKeyStoreTest on the newly-owned emulator. Gradle reported success, and the XML showed `tests=5, failures=0, errors=0, skipped=0`; restartPersistence had a zero-duration empty testcase. This reproduces the review's reporting defect. Log: `a3-fix1-android-before.log`.

Covering command before/after, from app/android:

```powershell
$env:JAVA_HOME='C:/Program Files/Android/Android Studio/jbr'
$env:FLUTTER_ROOT='C:/tools/flutter'
$env:ANDROID_SERIAL='emulator-5580'
./gradlew.bat :app:connectedDebugAndroidTest `
  '-Pandroid.testInstrumentationRunnerArguments.class=dev.chatproject.chat_app.AccountKeyStoreTest' `
  '-Ptarget-platform=android-x64' --console=plain
```

Corrected GREEN output:

```text
restartPersistence[chat_e2e_1(AVD) - 16] SKIPPED
BUILD SUCCESSFUL in 21s
XML: tests=5 failures=0 errors=0 skipped=1
restartPersistence: <skipped />
```

Thus the actual default suite result is **4 passed + 1 explicit skip**, including the existing real Rust worker/JNI test. Gradle's console inconsistently printed “Finished 6 tests”; the JUnit XML contains five testcases and one skip. The earlier report's five-green framework count must not be interpreted as default-run persistence coverage. Log: `a3-fix1-android-after.log`.

Installed the just-built app and instrumentation APKs on only emulator-5580 after UTP's automatic uninstall. The deliberate invalid-phase negative probe:

```text
adb -s emulator-5580 shell am instrument -w
  -e class dev.chatproject.chat_app.AccountKeyStoreTest#restartPersistence
  -e accountPhase invalid dev.chatproject.chat_app.test/androidx.test.runner.AndroidJUnitRunner
java.lang.AssertionError: accountPhase must be write or read
FAILURES!!! Tests run: 1, Failures: 1
```

This is the **expected rejection**, not an outstanding product/test failure; adb's shell process exit was 0, so the instrumentation result text was inspected explicitly. No fixture was read/created for the invalid phase.

Actual separate-process persistence remains GREEN:

```text
adb -s emulator-5580 shell am instrument -w
  -e class dev.chatproject.chat_app.AccountKeyStoreTest#restartPersistence
  -e accountPhase write dev.chatproject.chat_app.test/androidx.test.runner.AndroidJUnitRunner
OK (1 test), Time: 0.084
adb -s emulator-5580 shell am force-stop dev.chatproject.chat_app
adb -s emulator-5580 shell am instrument -w
  -e class dev.chatproject.chat_app.AccountKeyStoreTest#restartPersistence
  -e accountPhase read dev.chatproject.chat_app.test/androidx.test.runner.AndroidJUnitRunner
OK (1 test), Time: 0.078
```

New owned emulator launcher PID **24548**, serial **emulator-5580**, chat_e2e_1, headless/read-only/no-snapshot-save. Inventory initially showed no connected devices. Terminated with `adb -s emulator-5580 emu kill` after the read phase; launcher process no longer present. No physical device/GUI/public networking or recursive cleanup was used.

### Scope / remaining limitations

Only three source/test files changed: Runner/AccountKeyStore.swift, RunnerTests/AccountKeyStoreTests.swift, and Android androidTest AccountKeyStoreTest.kt. Rust, Flutter, Keychain ABI/status values, path/entitlement policy, and the existing three dirty M2 docs were untouched. `git diff --check` passed (only existing repository CRLF notices). Unaffected full Rust/Flutter suites were not rerun, per controller instruction. All original Mac compile/link/Keychain/first-unlock/signed-profile and physical Android StrongBox/TEE evidence remains pending; no platform gate is waived by this fix.


### A4 — retained task-reported evidence

Source: `.superpowers/sdd/2026-10-18-m3-a-keys-storage/task-4-report.md`. Historical evidence at the identified commits; not a C4 rerun.


Status: DONE_WITH_CONCERNS (Apple native evidence unavailable; Active restart is explicitly A5-owned).
Base: 048b4ed4e69c367ae521ddb0472e2100c8a5f7ee.
Commit: be9de7804356437d6850f9282248e0e9324dbaeb — feat(account): bind stable installation transport keys.
Post-commit status: only the three pre-existing unstaged M2 documents remain dirty.


- One AccountHandle actor owns the actual Store and private AccountState. RunningCore retains the handle. CoreSlot::open_account is once-only and takes A3's Arc<dyn KeyProtector>; no Dart secret roundtrip and no second CORE/writer were introduced.
- Minimal Candidate payload is exactly canonical definite CBOR [1,0,device_seed32]: bytes 83 01 00 58 20 followed by 32 seed bytes (37 bytes total). Decoder rejects mismatched derived installation ID, noncanonical representations, wrong version/tag/length, and trailing data. AccountState has no Debug/Clone/serde; payload allocations use Zeroizing and Secret32 owns the decoded seed. A5 must extend this codec in place.
- prepare_candidate persists a new DeviceSeed before handing its derived transport key to the network actor. It returns only public installation_id and endpoint_id after a successful bind. No QR/account identity/certificate/membership/Active success is fabricated.
- NetworkHandle::activate_key uses a private bounded control mailbox to serialize stop/epoch increment/pending dial cancellation/session closure/old Endpoint closure before rebinding. An already installed key cannot be replaced by a different key. Normal commands return Busy during transition; Info remains available. Old-generation notes are discarded. Bind failure keeps the saved key and Failed/offline state for a same-key prepare_candidate retry.
- Opening a saved Candidate gates Start before returning and installs the key without implicit Start. Resume also leaves the network stopped; callers explicitly Start. The old diagnostic SecretKey::generate behavior remains until account opening/activation, and diagnostic keys are never persisted/promoted.
- Actual iOS C symbols call crate::global::CORE.account_suspend/account_resume. Existing privacy overlay remains in place. NSE, entitlements, A3 protector code/ownership policy, providers, and external app state were not changed.



#### TDD and verification evidence

Initial RED:
`cargo test --locked -p chat_core --features test-hooks --test account_endpoint`
Exit 1. Expected missing interfaces: E0412 AccountHandle; E0599 CoreSlot::open_account and fail_next_bind_for_test. Initial tests covered candidate restart/new instance, diagnostic session transition, persisted-key bind failure retry, and suspend/failed-resume ownership.
Initial GREEN: same command, 4 passed / 0 failed, 0.15s.

Concrete regression RED/GREEN:
- `cargo test --locked -p chat_core --features test-hooks --test account_endpoint suspend_interrupts_a_binding_transition_and_latest_request_wins`: a held bind plus suspend/resume/suspend timed out at 3s (FAILED). Root cause: cancellation dropped bind work without resolving its activation reply, so the account actor could never consume lifecycle work. Added network actor cancellation branch that runs existing stop and completes the pending activation reply. Six-test focused GREEN then passed in 0.15s.
- `... --test account_endpoint lifecycle_gates_during_slow_open_without_waiting_for_protector`: regression injection removing the pre-lock lifecycle gate produced FAILED: unwrap_err got Ok Info(state: Starting) during blocked unprotect. Restored shared gate before try-lock; GREEN 1/1, 0.06s. The test holds the real CoreSlot initialization with a slow fake protector, asserts suspend returns within 100ms, then exercises resume/suspend before release and confirms no Start or candidate preparation while suspended.
- `... --test account_endpoint lifecycle_without_account_does_not_strand_diagnostic_start`: RED Busy after resume without account; fixed account-open tracking so no-writer lifecycle does not strand diagnostic gate. GREEN covered by the final focused/full runs.

Final focused command before full run:
`cargo test --locked -p chat_core --features test-hooks --test account_endpoint`
10 passed / 0 failed, 0.20s. Includes actual separate-process writer refusal before suspend, after suspend, and after failed resume; canonical payload rejection; opaque unprotect failure without replacement; loopback probe closure; bind failure saved-key preservation; rapid lifecycle; and same Candidate restart with new CoreInfo.instance_id. The named active_and_candidate_restart_keep_endpoint_new_instance covers Candidate only, explicitly documented in source; **the real Active half is A5's acceptance obligation**.

Full core command:
`cargo test --locked -p chat_core --features test-hooks`
First attempt exposed one existing unit-test literal missing new NetworkHandle fields (E0063); fixed the internal test setup to wire its actual controls/gate. Rerun exit 0:
- 65 unit tests (includes real key-transition stale epoch and saturated control mailbox assertions)
- account_endpoint 10; account_keys 8; account_storage 13; account_wire 7
- puppet 7; transport 17 (30.21s); compile-fail doc tests 3
- total 130 passed, 0 failed; no Rust warnings. Transport is the actual locked `--test transport` target included in this full run; it was not redundantly rerun afterward.

Additional available platform verification:
- `cargo test --locked -p chat_ffi`: exit 0; 11 passed + 1 explicitly ignored restart child which is separately spawned and passed (1/1). Includes real Windows DPAPI and separate-process persisted wrapped-key test. No new iOS coverage is inferred from it.
- `$env:ANDROID_NDK_HOME='C:/Users/hyunw/AppData/Local/Android/Sdk/ndk/28.2.13676358'; cargo ndk -t arm64-v8a check --locked -p chat_ffi`: exit 0, aarch64-linux-android dev check, 16.54s. Build/typecheck only; no Android lifecycle runtime claim.
- `cargo check --locked -p chat_ffi --release --features test-hooks`: exit 0, 14.54s. All newly added failure/hold hooks and Apple native test module require test-hooks AND debug_assertions (test module also requires test); Release excludes them.
- `git diff --check -- core app`: exit 0, only repository CRLF notices.


#### Apple/C4 limits — not waived

No swift, xcodebuild, Apple Rust target, Apple SDK, simulator, or signing execution was available on this Windows host. The real iOS ABI test source `lifecycle_abi_controls_the_same_core_actor` is authored under the Apple module and uses the actual global CORE; it is **not run or typechecked here**, and no native RED/GREEN claim is made. Swift compilation/linking of the C symbols, UIKit scene/background-task/expiration behavior, first-unlock Keychain behavior, cross-process lock release under iOS finite guards, and background network gating on native iOS all require C4's Mac/native harness. The existing host iOS-policy simulation is policy evidence only, never native execution evidence. No physical device or public DHT/relay probe was used.


#### Retained correction rounds

## Fix round 1 — review I1 and I2

Base: be9de7804356437d6850f9282248e0e9324dbaeb. Status: DONE_WITH_CONCERNS (unchanged A5/C4 evidence boundaries). Fix commit: d197fd4dc962aaa03e16ffb3662696cbd1c26b70 — fix(account): permit authorized rekey and retain suspend cancellation. Post-commit status contains only the three pre-existing unstaged M2 documents.

### I1 — account policy belongs to the account actor

Removed the transport Manager's `installed` flag and blanket different-key AccountRestricted branch. This **supersedes the original report's claim that an installed key cannot be replaced**. NetworkHandle::activate_key now performs the same serialized stop/epoch/drain/rebind for any caller-authorized persisted installation key. Same-key offline retry remains unchanged. No generic Core reset, extra writer, new account command, Active payload, or authorization shortcut was added.

Exact consumer policy: A5's state-owning actor must reject create/recover/link for active accounts, and authorize/commit the approved never-active Candidate recovery or B6 discard/fresh setup before calling activate_key. Transport has no information with which to make that account-state decision. A5 must still implement and test real same-Core Candidate recovery with a fresh DeviceSeed/certificate and active-state refusal.

The regression is a private cfg(test + test-hooks + debug_assertions) fixture in account/manager.rs. It owns one actual Store, invokes the actual Manager::prepare handler, uses the same actual CoreSlot/NetworkHandle throughout, and connects a real loopback peer. It persists/loads the first Candidate, commits a fresh Candidate to the same Store, assigns that private state as the future authorized actor would, calls prepare again, and checks the new persisted installation, changed EndpointId, unchanged CoreInfo.instance_id, empty old session list and rejected send to the old peer. The fixture tests the persistence-to-transport boundary; it does not claim to exercise an A5 recovery command or its authorization policy.

RED command:
`cargo test --locked -p chat_core --features test-hooks --lib persisted_never_active_candidate_can_change_key_in_same_core`
After correcting one test cfg delimiter typo (not counted as the regression), exit 1:
`called Result::unwrap() on an Err value: CoreError { code: AccountRestricted, detail: "network operation unavailable" }`
`test result: FAILED. 0 passed; 1 failed; ... finished in 0.11s`
GREEN, same command after removing the blanket transport restriction: exit 0, 1 passed / 0 failed, 0.14s.

### I2 — durable revision recheck closes the token publication window

Manager::start records the gate revision while replacing the shared cancellation token. After releasing that mutex, it rechecks the revision and cancels the newly published token if it changed. A suspend whose try_lock missed the replacement mutex therefore leaves a durable revision change that start observes. After unlock, a later suspend can directly acquire/cancel the new shared token. Existing activation cancellation then resolves the pending reply and lets the account actor process suspend. Native lifecycle calls still never block on the token replacement mutex, and no timeout-as-success behavior was introduced.

Added the per-NetworkHandle `hold_next_token_replacement_for_test` scheduling hook under feature=test-hooks AND debug_assertions. It pauses the test-owned network actor inside the token replacement mutex, immediately after its revision read. Test channel disconnection or a five-second recv_timeout releases this window even on test failure; no global barrier/helper process or secret/approval/RNG hook exists. The separate held-bind hook remains cancellable by the real token/runtime shutdown.

RED command:
`cargo test --locked -p chat_core --features test-hooks --test account_endpoint suspend_inside_token_replacement_settles_without_releasing_bind`
The deterministic test requests suspend while the exact mutex window is held, asserts the native-style request returns within 100ms and Start is Busy, releases only the replacement window, and deliberately never releases bind. Before the fix: exit 1, `Err value: Elapsed(())`, 0 passed / 1 failed, 3.02s (activation reply stuck).
GREEN, same command after the post-unlock revision recheck: exit 0, 1 passed / 0 failed, 0.01s. Both the activation error reply and completed suspend status 0 occur with bind still held; final network state is Stopped. This proves the previously missing interleaving rather than repeating the post-Starting suspend test.

### Focused verification after both fixes

- `cargo test --locked -p chat_core --features test-hooks --lib manager::tests`: exit 0; 20 passed / 0 failed, 0.22s. Includes account persistence/rekey, transition stale epochs/full queue, network start/stop/cancel/session regressions.
- `cargo test --locked -p chat_core --features test-hooks --test account_endpoint`: exit 0; 11 passed / 0 failed, 0.20s.
- `cargo test --locked -p chat_core --features test-hooks --test transport`: exit 0; 17 passed / 0 failed, 30.24s. Relevant because start/cancellation behavior changed.
- `cargo check --locked -p chat_core --release --features test-hooks`: exit 0, 9.12s; new scheduling hook excluded by debug_assertions gate.
- `git diff --check -- core`: exit 0; repository CRLF notices only. No unrelated full suites rerun.

Self-reviewed only the three changed covering files: transport/manager.rs production correction + scoped hook; account/manager.rs private test fixture only; tests/account_endpoint.rs race regression. Existing three dirty M2 documents remain unstaged/untouched. No iOS/native source changed in this round. Apple compilation/linking/actual ABI, UIKit/background/expiration, and native finite-lock evidence remain explicitly unverified C4 obligations; no host test result is promoted to native evidence. The separate A5 Active-state/recovery acceptance obligations remain mandatory. The revision recheck fixes this demonstrated lost-cancellation window; it is not a claim that separate lifecycle intent/request/status writes form a fully linearized concurrent protocol.


### A5 — retained task-reported evidence

Source: `.superpowers/sdd/2026-10-18-m3-a-keys-storage/task-5-report.md`. Historical evidence at the identified commits; not a C4 rerun.


Status: DONE_WITH_CONCERNS (implementation verified on Windows; Apple compile/runtime remains C4).
Base: d197fd4dc962aaa03e16ffb3662696cbd1c26b70
Implementation commit: 1575dd3079af7a7a9385cd50f7f2d767b2670e66 — feat(account): add creation and recovery lifecycle


Extended the actual AccountHandle/Manager and actual AccountState codec; no second actor, reset, alternate snapshot, publication stub, or pairing/import success path. Creation generates recovery entropy, derives identity signing keys, returns the 24 words once, and retains only signing keys plus three zeroizing confirmation answers. Original entropy/full phrase are not persisted. Confirmation commits signing seed, fresh independent DeviceSeed, valid B1 certificate, revision and typed local publication intent before activating the installation transport key. Recovery requires explicit replacement confirmation, validates the phrase, derives the same PeerId, creates fresh device keys/cert, persists replacement intent, and reports PublishPending with empty contacts. It does not publish a record.

Never-active Candidate recovery works in the same running Core with a new endpoint/installation, without reset. Established LocalReady/PublishPending accounts refuse begin/create/recover/prepare_candidate with AccountExists and the safe instruction to use a separate empty storage folder. The actual created-account Store/CoreSlot restart test proves identical PeerId, certificate, EndpointId, and new CoreInfo.instance_id. Existing real Candidate restart test remains unchanged and passing.

All private commands carry enqueue lifecycle tickets; stale commands are rejected even after a suspend/resume cycle ends. Actor readiness is rechecked before persistence, transport activation and result handoff. Suspend/resume invalidates the in-memory creation flow. Snapshot returns Locked with no account identifiers while lifecycle is suspended, pending or failed. Commit errors fail closed: network is blocked, lifecycle reports failed, and new writes are refused until an explicit suspend/resume cycle reloads and validates the actual snapshot. This handles both old-on-disk errors and errors after completed replacement; a private unit-only failpoint proves the latter resolves to AccountExists after reload rather than overwrite.

Explicit new begin_create replaces the previous unconfirmed flow with a fresh ID/phrase; old IDs cannot confirm or cancel the replacement. Confirmation is three sorted distinct one-based positions (1..=24); answers correspond to that order. Each raw answer has a 64-byte UTF-8 cap before NFKD, trim and ASCII lowercase normalization, must contain one word, and failures use InvalidRecovery without echoing inputs. No automatic word correction. Recover(false) returns AccountRestricted before replacement intent creation.

Secret display has no Debug/Clone/Serialize, is absent from snapshots/events, and zeroizes its owned words on drop. Secret command strings/answer arrays, generated phrase/words, retained expected answers and storage payloads use zeroizing ownership. Keys use existing Secret32 erasure; the minimal crate-private AccountKeys::from_identity consumes an already-derived signing seed and recomputes PeerId without running HKDF twice. Memory erasure does not promise erasing arbitrary caller-created clones, FFI strings or OS keyboard buffers.


#### TDD evidence

1. Initial tests: `cargo test --locked -p chat_core --features test-hooks --test account_lifecycle` failed with unresolved AccountPhase/PublishIntent/RecoveryDisplay and missing begin_create/confirm_create/cancel_create/recover/snapshot. Expected missing feature/API. After implementation, six of seven ran green; the phrase-on-disk test tried reading Windows' locked `.account.lock` and was corrected to inspect account artifacts, not the lock byte. This was a test fixture issue, not a product failure.
2. Revision RED: same focused command with the added sorted/normalized/revision test failed `assertion failed: creating > initial`. Added actor revision tracking and durable established revision; GREEN later includes this test.
3. Queue epoch RED: `cargo test --locked -p chat_core --features test-hooks --test account_lifecycle queued_creation` failed because begin unexpectedly returned Ok across suspend/resume. Commands now carry enqueue tickets; GREEN test rejects stale begin with StorageLocked.
4. Pre-write failure RED: `cargo test --locked -p chat_core --features test-hooks --test account_lifecycle commit_error` failed lifecycle_status left=0/right=2 after real filesystem atomic-temp obstruction. Minimal fail-closed persistence gate made it GREEN and requires successful explicit lifecycle reload before subsequent writes.
5. Actual post-replace regression: initial new private fault fixture failed because the after_replace fault boundary was not wired. Then the fault was wired, fail-closed guard temporarily removed, and `cargo test --locked -p chat_core --features test-hooks --lib completed_replace_error` failed left=Candidate/right=Locked after the file actually contained established state. Restoring guard made the same command pass 1/1. It checks blocked create/recover, validates actual durable Active snapshot, explicitly reloads through suspend/resume, then proves AccountExists/no overwrite. The scoped thread-local helper restores its previous setting on success, Err and unwind, is synchronous/no await, and exists only under cfg(test).
6. Replacement-flow RED: `cargo test --locked -p chat_core --features test-hooks --test account_lifecycle lost_flow` failed Busy on second explicit begin. Removing the pending-flow Busy guard (after established/readiness checks) made fresh flow replacement pass, including old confirm/cancel rejection.

Final focused lifecycle GREEN: `cargo test --locked -p chat_core --features test-hooks --test account_lifecycle` -> 10 passed, 0 failed, clean compiler output. Includes phrase confirmation/absence from encrypted artifacts/general Debug/serialized events, valid public cert, cancellation/no intent, same-Core recovery with actual original-account PeerId and fresh device, established refusal, old-flow isolation, real created-account restart, suspended command gating, stale queue gating, and commit reconciliation.


#### Verification

- Full feature core suite run once as requested: `cargo test --locked -p chat_core --features test-hooks` -> 148 total passed (70 lib, 11 endpoint, 8 keys, 10 lifecycle, 13 storage, 7 wire, 7 puppet, 17 transport, 5 compile-fail docs). No failures/warnings. Network fixtures all local; account tests configure_local(None) before candidate/create/recover/start. No public DHT/relay probe.
- `cargo test --locked -p chat_core` final clean run -> 97 passed (64 lib, 7 keys, 13 storage, 7 wire, 1 hooks-disabled, 5 docs). Endpoint/lifecycle/puppet feature integration targets each ran 0; transport target requires test-hooks and is omitted. Those omitted tests are covered only by the feature run above, not counted as default passes. Earlier default run had a cfg(test) unused reexport warning, fixed by matching its feature/debug gate; final run is clean.
- After private enum boxing/test relocation lint cleanup: `cargo test --locked -p chat_core --features test-hooks --lib account::` -> 17 passed. Full feature suite was not redundantly repeated for this mechanical change.
- `cargo check --locked --workspace` -> passed.
- Strict Clippy initially exposed owned enum-size/test placement issues (fixed) and two A4 transport prerequisites. Controller authorized narrow prerequisite edits: box only Control::Activate's key and collapse only ConfigureLocal's relay validation if. No lint suppression or neighboring refactor.
- After those transport edits: `cargo test --locked -p chat_core --features test-hooks --lib transport::manager::tests` -> 19 passed; `cargo test --locked -p chat_core --features test-hooks --test account_endpoint` -> 11 passed.
- Final `cargo clippy --locked --workspace --all-targets -- -D warnings` -> passed. Final `cargo clippy --locked -p chat_core --all-targets --features test-hooks -- -D warnings` -> passed.
- Owned Rust files formatted with rustfmt edition 2024 / skip_children=true; transport prerequisite deliberately preserves existing surrounding style. `git diff --check -- core/chat_core` -> passed (Git emits only repository line-ending conversion notices, no whitespace errors).
- `rustup target list --installed`: Windows MSVC plus four Android Rust targets; no Apple target installed. Apple-target compilation/runtime was not performed and is not claimed. C4 remains required on an Apple host. No physical device, push/merge/upload, original checkout/provider, or transcript mutation.


### B1 — retained task-reported evidence

Source: `.superpowers/sdd/2026-10-18-m3-b-identity-pairing/task-1-report.md`. Historical evidence at the identified commits; not a C4 rerun.


Status: DONE_WITH_CONCERNS (Apple native validation unavailable on this Windows host).
Base: `231f6a9185b3234c1019afc52d66e25abf7bef73`.
Scoped implementation commit: `048b4ed4e69c367ae521ddb0472e2100c8a5f7ee` — `feat(identity): add signed account record codecs`.


Implemented only B1 in `C:/Users/hyunw/orca/workspaces/ChatProject/m2-iroh-transport`:

- `core/chat_core/src/account/wire.rs`: bounded fixed-byte/array/unsigned/kind decoding and canonical byte comparison using minicbor 0.26.5.
- `certificate.rs`: exact `[1,peer32,installation32,transport32,kind,created_at,sig64]`, signing `ChatProject/cert/v1 || 0x01 || canonical six-field body array`, strict Ed25519 verification against expected raw PeerId, canonical full-cert SHA-256.
- `record.rs`: exact `[1,generation16,issued_at,expires_at,[[installation32,transport32,kind],...]]`, maximum five devices, strictly ascending raw installation IDs, unique installation and transport keys, definite/canonical CBOR, one `_account` IN TXT RR at TTL86400. Base64url has no padding. Pkarr's TXT builder splits the payload into <=254-byte character strings inside the same RR, below the DNS255 limit.
- Independent checks for full wrapper<=1000 and DNS BEP44 value<=1000 on send/receive; receive rejects incorrect origin/name/class/TTL, other records/sections/header options, duplicate account RRs, trailing DNS, malformed base64/CBOR, incorrect expected PeerId, invalid signature, and unsigned/unchecked-deserialized tampering.
- Record validity enforces `expires_at > issued_at`, lifetime<=604800, future issuance<=300 seconds. `parse_record` intentionally preserves valid signed expired records for B2 renewal; `is_expired(now)` is a separate authorization check. Neither old seq nor DNS `last_seen` changes application validity. Seq above signed BEP44 i64 range is rejected.
- `pkarr_backend.rs`: object-safe `RecordBackend: Send+Sync`, boxed Send futures, `Lookup::{Found,CompletedAbsent}`, DHT-only `PkarrBackend`. Constructor explicitly disables HTTP relays and the local pkarr cache. Every lookup uses exact `Client::resolve(&key, ResolvePolicy::NetworkOnly)`. Only `ResolveError::NotFound` becomes completed absence. Every other lookup error remains `Err(RecordUnavailable)`. Publication `NotMostRecent` becomes `RecordConflict`; every other publication failure remains `RecordUnavailable`. Detail strings remain fixed, without reflecting remote content.

The old iroh-mainline-address-lookup endpoint protocol is unchanged. No account handle, publication high-water, authorization state machine, pairing, A4/A5, B2, FFI secret getter, or later-task success stub was added. All test fakes are inside `cfg(test)`.

#### TDD and focused verification

Initial RED, before production codec/API implementation:

```text
cargo test -p chat_core --test account_wire
error[E0432]: unresolved imports IdentityRecord, InstallationCert, RecordDevice,
parse_record, record_packet, sign_cert, verify_cert
error: could not compile chat_core (test account_wire)
```

This was an API-absence compile RED, not a behavioral test failure; the initial wire tests were written first. Full output: `b1-red.log` in this report directory. After implementing the codecs, the same focused command passed all6 initial tests (`b1-green-initial.log`), including all four required named tests. A seventh regression test was then added for unchecked signature tampering, oversized wrapper with legal DNS size, additional DNS sections and trailing DNS; production code needed no change. A first attempt used nonexistent Packet::into_owned; corrected the test to retain its DNS bytes, then7/7 passed (`b1-hostile-green.log`).

Backend tests were written before the backend implementation (`b1-backend-red.log`, absent APIs). A minimal error mapper first returned errors for every failure; the focused runtime RED established the required missing absence branch:

```text
cargo test -p chat_core --lib account::pkarr_backend
only_not_found_means_completed_absent ... FAILED
assertion failed: matches!(resolve_result(Err(ResolveError::NotFound)),
    Ok(Lookup::CompletedAbsent))
test result: FAILED. 1 passed; 1 failed
```

Output: `b1-backend-behavior-red.log`. After adding the exact NotFound branch, the final focused backend command passed3/3 (`b1-backend-green.log`). A test helper import and NonZeroUsize constructor mismatch were corrected during test authoring; the final command is warning-free.

Backend tests inject every ResolveError variant plus Found through a cfg(test) fake implementing the actual object-safe RecordBackend. Publication tests exercise every PublishError variant. An actual pkarr client test binds only127.0.0.1 with empty bootstrap, preloads a higher-seq900 cache entry with both fresh and expired last_seen, and invokes PkarrBackend::lookup. Both cases return a network error rather than the cached packet or completed absence, verifying actual NetworkOnly behavior with no public network. The wire expiry test independently parses seq1/DNS-expired packets, checks app expiry and future300/301 boundaries, and preserves a signed expired renewal base. B1 does not discard lower-seq records based on cache freshness; later B2 compares accepted signed bases/high-water.


#### Full and platform verification

After rustfmt of only the owned Rust files, ran the appropriate full core suite once:

```text
cargo test --locked -p chat_core --features test-hooks
unit:64 passed
account_keys:8 passed
account_storage:13 passed
account_wire:7 passed
puppet:7 passed
puppet_disabled:0 tests (feature configuration)
transport:17 passed
compile-fail doc tests:3 passed
total:119 passed;0 failed;0 ignored
exit0
```

Full output: `b1-full-core.log`. No compiler/test warnings. All transport tests use existing local profiles/local fixture services; no public DHT/relay probes were used. No production changes followed this full-suite run.

```text
cargo +1.92.0 check --locked -p chat_core
Checking pkarr v8.1.0
Checking chat_core v0.1.0
Finished dev profile ... in4.98s
exit0

cargo +1.92.0 check --locked -p chat_core --target aarch64-linux-android
Finished dev profile ... in27.39s
exit0

cargo +1.92.0 build --locked -p chat_ffi --target aarch64-linux-android
Compiling pkarr v8.1.0
Compiling chat_core v0.1.0
Compiling chat_ffi v0.1.0
Finished dev profile ... in36.23s
exit0
```

Logs: `b1-host-check.log`, `b1-android-check.log`, `b1-android-link.log`. Android used SDK-installed NDK27.0.12077973, `toolchains/llvm/prebuilt/windows-x86_64/bin/aarch64-linux-android24-clang.cmd` as CC/linker and llvm-ar.exe as AR. Task-local process environment only. Final native linked artifact is `target/aarch64-linux-android/debug/libchat_ffi.so`; NDK llvm-readelf confirms ELF64/DYN/AArch64. Artifact SHA256 `b04a13c2bd9a182ca422cb35b20bf780b7268999753414a03ec65680b8bc2a17`. This is a real Android target compilation/link, not an emulator/device test or app APK validation. Other Android ABIs were not rebuilt for B1.

Attempted supported local iOS cross-check:

```text
cargo +1.92.0 check --locked -p chat_core --target aarch64-apple-ios
error[E0463]: can't find crate for core
note: the aarch64-apple-ios target may not be installed
error occurred in cc-rs: failed to find tool xcrun: program not found
exit101
```

Full output: `b1-ios-check-unavailable.log`. Installed rustlib inventory contains only host Windows plus four Android targets; Get-Command xcodebuild returned no executable. Windows has no Xcode/iPhoneOS SDK/xcrun. **No Apple compile, link, Swift/native, simulator or physical iOS success is claimed. Real Mac/iOS-target validation remains a mandatory C4 gate and is not waived.** No Apple toolchain installation or unsupported compiler substitution was attempted.


#### Exact dependency delta

Added only direct `pkarr = { version="=8.1.0", default-features=false, features=["dht"] }` in chat_core; minicbor0.26.5 was already present. `cargo fetch` resolved these9 new locked packages:

1. `async-compat` 0.2.6
2. `base32` 0.5.1
3. `document-features` 0.2.12
4. `flume` 0.12.0
5. `litrs` 1.0.1
6. `mainline` 8.0.1
7. `ntimestamp` 1.0.0
8. `pkarr` 8.1.0
9. `self_cell` 1.3.0

Cargo.lock delta is117 added lines:9 package entries plus chat_core's pkarr dependency edge; **0 existing package version upgrades,0 removals**. Verified by parsing base/current locks with Python tomllib and comparing(name,version) sets. No broad cargo update. Official registry source `pkarr-8.1.0/src/client/client_impl.rs`, `client/errors.rs`, `client/builder.rs`, `client/backend/dht.rs`, `types/signed_packet.rs`, `types/keys.rs` was inspected to verify actual NetworkOnly/NotFound/error enum/signing/TXT/timestamp APIs. `cargo tree -p pkarr@8.1.0 -e features --depth1` confirms DHT and signed packet dependencies with no pkarr reqwest/relay feature. Existing iroh's reqwest is unaffected. Existing iroh/iroh-base feature unification enables dalek3 zeroize for the temporary pkarr signing key; account signing's dalek2 already has zeroize enabled.


### B2 — retained task-reported evidence

Source: `.superpowers/sdd/2026-10-18-m3-b-identity-pairing/task-2-report.md`. Historical evidence at the identified commits; not a C4 rerun.


Status: DONE_WITH_CONCERNS. B2 source/policy is implemented and validated. B3 real authenticated connected-peer fanout/receipt processing and C4 Apple native validation remain mandatory gates; neither is claimed here.

Base: `1575dd3079af7a7a9385cd50f7f2d767b2670e66`.
Scoped implementation commit: `5ed42599bec2bb857ae9415a6e8d10cc41e07853` (`5ed4259`) - `feat(identity): persist publication and revocation conflicts`.


The existing account Manager remains the single state owner/writer. A5 confirmation/recovery persists local ownership, then invokes publication. Startup/resume refresh established state, and a foreground Tokio interval reconciles every 60 seconds. Observed records renew at 23 hours, within the 24-hour scheduling requirement. Suspend cancels pending service futures through the existing lifecycle watch/ticket gate. Every commit and notification/publication handoff rechecks readiness; every packet handoff rechecks current wall time/expiry. Service awaits have a 10-second timeout. No second Store owner/actor, reset, general journal, secret getter, production test approval or deterministic RNG was added.

Completed absence differs from backend failure. Initial confirmed creation allocates one new generation/device only after completed absence; explicitly confirmed recovery replaces an observed old list with one fresh local device/generation. Valid expired signed bases renew the same generation/list after seven days. Missing remote records cause pending renewal, not observation. Old/competing records preserve observed floors and hold publication. Unexpected list changes, same-sequence different packets and generation changes retain signed conflict evidence. Exact pending packets become accepted only when reobserved; publication ACK alone never marks Active/Observed. Expired unobserved pending packets reconcile against the same base/absence, preserve approved membership and allocate a fresh sequence.

Sequence is `max(checked(now_seconds * 1_000_000), observed + 1, allocated + 1)`, limited to signed i64 and current time + 300 seconds. Both floors and exact signed packet/base/operation/status are committed before sending. Exact retry survives restart; lower lookup values and clock rollback cannot reduce floors. Snapshot cryptographic/canonical validation is independent of current clock; public B1 `parse_record` retains future-issued rejection. Even a cryptographically valid future observation raises the durable observed floor while remaining unusable for authorization/publication.

Revocation requires a fresh completed lookup of the accepted unexpired same-generation list including exact local installation/transport. Cached absence is only a renewal basis. Self/last-device revocation is refused. Signed revoke intent, local tombstone and exact pending public packet are committed together before notification. No notifier is installed in production: publication remains pending and the view reports notification pending. Notifier failure preserves the local block and prevents public mutation; success is durably recorded before publishing. Clock rollback during that await still prevents publication. Tombstones survive conflicts and renewal/rebase and apply by installation OR transport key.

Conflict confirmation requires the displayed conflict hash still match the fresh lookup and requires same-generation exact local membership. Changed generation, excluded writer, expired/current-time-invalid base or obsolete hash cannot reassert ownership. Allowed explicit rebase supersedes held packets and retains tombstones. Initial recovery replacement consent is not repeated takeover consent.

#### TDD and validation evidence

All commands ran only in the linked m2-iroh-transport checkout, using fake services or existing explicit loopback fixtures.

RED evidence:

1. `cargo test -p chat_core --features test-hooks --test account_publication --locked`: missing B2 API compilation failures (AccountServices, PublicationState, refresh_record, confirm_record_conflict, publication). Initial tests preceded production policy.
2. `cargo test -p chat_core --lib account::devices --locked`: missing RevokeIntent, followed by implementation and focused pass.
3. Additional behavior RED from complete publication target: completed absence incorrectly remained Observed instead of Pending; expired unobserved packet returned AccountRestricted instead of renewing with fresh sequence. `cargo test -p chat_core --features test-hooks --test account_publication --lib account --locked` caught cached-absence revoke returning Ok. Fixed branches; integration10/policy3 passed.
4. `cargo test -p chat_core --features test-hooks --test account_publication future_signed --locked`: observed floor0 instead of1800000600000000. Fixed durable future observation floor without authorization.
5. `cargo test -p chat_core --features test-hooks --test account_publication concurrent_add_revoke --locked`: held notification count0 instead of1. Fixed truthful conflict view.
6. `cargo test -p chat_core --features test-hooks --test account_publication revoke_is_durable --locked`: clock rollback inside notifier completion produced3 publishes vs expected2. Added immediate pre-handoff time checks; final complete publication suite15/15.

GREEN and gates:

- `cargo test -p chat_core --features test-hooks --lib account::publication --locked`:3/3, actual tombstone256/pending32 bounds preserve old state, codec rejects257/33, invalid time/high-water overflow, absent cached base cannot authorize revoke.
- `cargo test -p chat_core --features test-hooks --lib completed_packet_replace_error --locked`:1/1, real completed-replacement failure sends zero packets and locks; disk contains exact allocated packet; explicit reload retries exact bytes.
- `cargo test -p chat_core --features test-hooks --locked`:168/168: lib75, endpoint11, keys8, lifecycle10, publication15, storage13, wire7, puppet7, transport17, doc5. Zero failed/ignored. Full suite ran once before final narrow pre-handoff clock check; complete affected publication suite reran afterward, avoiding redundant broad transport repetition.
- Final `cargo test -p chat_core --features test-hooks --test account_publication --locked`:15/15 after last functional edit, zero failed/ignored.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: final exit0. Initial two collapsible-if findings mechanically fixed before final pass.
- `cargo clippy -p chat_core --all-targets --features test-hooks --locked -- -D warnings`:exit0.
- `cargo check --workspace --locked`:exit0.
- `git diff --check` and staged diff check:exit0. Git CRLF normalization advisories are not source/test warnings.

Intermediate harness issues: integration-only service helper initially lived in a support file also imported by library tests, causing unresolved chat_core names; moved to dedicated account_services.rs. Notification test initially tried a second Windows Store owner while suspended actor retained process ownership; corrected by dropping the original Core before Store inspection. Neither was product RED evidence. Final metadata replacement encountered Windows cp949 em-dash encoding and truncated this ignored report; restored with apply_patch and verified full content/commit ID. Source/commit were unaffected.


#### Self-review, files and remaining gates

Reviewed every owned diff and both production modules. Confirmed no active overwrite/reset, secret Debug exposure, ACK-only completion, public-network tests or fake authenticated delivery. Self-review fixed future-floor preservation, missing-record status, expired pending renewal, cache-only revoke authorization, held notification visibility and clock change during notification. Existing candidate/lifecycle tests pass. Three original dirty M2 docs remain unstaged/untouched; original checkout/provider/transcript stayed read-only.

Created account/publication.rs, account/devices.rs, tests/account_publication.rs, tests/support/account_services.rs. Modified account/manager.rs, mod.rs, record.rs, recovery.rs (one initializer), storage/schema.rs, runtime.rs (narrow shared open/injection), account_endpoint.rs/account_lifecycle.rs (pre-spawn offline injection). Exactly twelve source/test files committed. Report is ignored .superpowers orchestration metadata, following prior task convention.

Mandatory remaining gates: B3 actual authenticated same-peer fanout/receipt validation and connected-device ordering evidence; B4 contacts; B5 approved pairing/addition; B6 import transactions; C1 FFI/UI; C4 real Apple compile/link/runtime. Host checks are not Apple/native success. No physical-device, public DHT/relay, push, merge or upload work ran. Shared IdentityKey copies are not cryptographically revoked; best-effort DHT lookup is not global absence proof, linearizable consensus or instantaneous offline revocation.



#### Retained correction rounds

## Fix round 1: durable future-observation conflict hold

Reviewed base: `5ed42599bec2bb857ae9415a6e8d10cc41e07853`. Scoped fix commit: `a66472533573df3efbce9c0b85df0d24dcf5a4db` (`a664725`) - `fix(identity): retain future publication conflict holds`.

Root cause: the future-time branch returned before ordinary conflict reconciliation. It persisted the numeric observed floor but discarded the differing signed wrapper and left dependent packets Ready. A later completed absence could retry the old packet below that floor, or allocate a renewal of obsolete authority after the clock caught up. A same-sequence/different-hash future record also bypassed conflict detection.

Changed only `core/chat_core/src/account/publication.rs` and `core/chat_core/tests/account_publication.rs`. Cryptographically valid unknown future observations now call the existing durable hold path: retain the signed conflict packet, preserve the high-water, and mark dependent intents Held. Subsequent absence, clock catch-up and restart cannot release it. Exact known accepted/pending wrappers made future by clock rollback retain the clock-only LocalBlocked behavior and do not invent a conflict. Tombstones and pending notification evidence remain present through the hold and snapshot round trip. No manager/persistence/handoff/service/schema API changes were needed.

Controller-approved initial-recovery nuance: only Replacement + accepted=None + pending empty + allocated_seq=0 + LocalBlocked is eligible for provisional future evidence. The existing conflict slot retains that signed evidence with LocalBlocked; absence and restart cannot allocate. Only a later fresh Found passing current-time/signature checks and seq>=observed can consume the original replacement consent. It clears provisional evidence and allocates the first one-device replacement. Actual Conflict is excluded from this exception. A conflict-confirm or revoke action cannot consume provisional evidence. Once any packet has been allocated or accepted authority exists, this initial exception is unavailable. This introduces no new snapshot field or repeated takeover consent.

RED commands and actual failures:

- `cargo test -p chat_core --features test-hooks --test account_publication future_ --locked`: exit1, 1 passed/3 failed. `future_competitor_holds_ready_pending_across_absence` and `future_excluded_writer_stays_held_after_clock_catches_up` both received `Ok(())` where `unwrap_err()` was expected. `future_same_sequence_conflict_and_pending_hold_survive_restart` observed 3 sends instead of 2 after restart. These failures reproduced the reported stale retry/renewal/evidence-loss paths before production edits.
- `cargo test -p chat_core --features test-hooks --test account_publication future_initial_recovery --locked`: exit1, 0 passed/1 failed. The provisional recovery record hash was None instead of the signed future packet hash. This test then checks blocked absence before/after clock catch-up and restart, rejection of a fresh lower-sequence base, no consumption by conflict-confirm, and successful initial replacement only after a fresh valid base.

Final GREEN commands and actual results after the last edit:

- `cargo test -p chat_core --features test-hooks --test account_publication --locked`: exit0, 19 passed, 0 failed/ignored, finished0.42s. Includes ready renewal + future changed generation + absence, future excluded writer + clock catch-up + absence, same-sequence differing future packet + restart with a held pending renewal, exact-known accepted/pending rollback, provisional initial recovery, and all prior publication integration cases.
- `cargo test -p chat_core --features test-hooks --lib account::publication --locked`: exit0, 4 passed, 0 failed/ignored, finished2.46s. New snapshot test directly verifies PacketStatus::Held, unchanged tombstones and visible pending notification through encode/decode and subsequent completed absence. Existing 256/32 limits, high-water/time overflow and cache-only revocation tests also pass.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: exit0, finished2.12s.
- `cargo clippy -p chat_core --all-targets --features test-hooks --locked -- -D warnings`: exit0, finished2.04s.
- `cargo check --workspace --locked`: exit0, finished1.59s.
- `git diff --check`: exit0. Git CRLF normalization advisories only.

Self-review: verified provisional-state eligibility cannot auto-clear actual Conflict, exact known wrapper handling preserves an existing conflict, invalid/lower fresh provisional bases leave evidence intact, and manager already commits staged error results before returning. Reviewed the complete two-file fix diff. Manager/persist code is unchanged, so completed_packet_replace_error and the broad 168-test suite were not redundantly repeated; their earlier evidence remains above. The 19 integration and 4 policy tests cover all amended production paths. Original three M2 docs remain untouched/unstaged; no public-network, physical-device, native Apple or remote work ran. B3 authenticated peer delivery/receipt and C4 Apple native gates remain mandatory. Independent fix review remains the controller's next step.


### B3 — retained task-reported evidence

Source: `.superpowers/sdd/2026-10-18-m3-b-identity-pairing/task-3-report.md`. Historical evidence at the identified commits; not a C4 rerun.


Status: DONE, strict CI gates passed, pending independent controller review. Final HEAD: `185445cf01c8b3e0a4134870f9794caf08918a7c` (review fix1 complete; strict CI gates passed). Initial implementation commit: `a02d1ce6b848a459d4fbfe4aee29bda3f2d401c6` (`feat(identity): authenticate account sessions with channel proofs`).
Base: `a66472533573df3efbce9c0b85df0d24dcf5a4db`.
Workspace: `C:/Users/hyunw/orca/workspaces/ChatProject/m2-iroh-transport`.


The existing account actor now authenticates actual iroh connections with the persisted installation key and the actual endpoint identity. It uses ALPN `chatproject/account/1`, verifies the account-signed certificate, exact installation/transport/kind membership, generation, current signed public record, expiry, actual `Connection.remote_id()`, and a fresh installation signature bound to both role-ordered hellos and the connection TLS exporter. A certificate or M2 connection alone never authorizes account operations. No second identity, Store writer, unauthenticated ingestion API, or private-key getter was introduced.

The exporter label is `ChatProject/account-exporter/v1`; signature input is `ChatProject/account-auth/v1 || context || exporter32`. Context is canonical `[1,[dialerPeer32,dialerTransport32,dialerInstallation32,dialerCertHash32,dialerGeneration16,dialerNonce32],[acceptorPeer32,acceptorTransport32,acceptorInstallation32,acceptorCertHash32,acceptorGeneration16,acceptorNonce32]]`. Exporter and signature-input buffers zeroize. Roles come from actual connection side. Both peers exchange the exact accepted public packet and perform strict installation-key signature verification. Raw signed wrapper decoding is bounded to 104..1000 bytes before SignedPacket construction, then signature/structure/current-time parsing and separate expiry checks run.

Expiry and same-generation renewal remove readiness; public record synchronization remains allowed over the established connection, and a fresh proof is required to grant again. Restrictions are sticky even if the wall clock rolls back. Removal, generation change, conflict and lifecycle/storage failure close affected sessions. The account actor rechecks authorization on every account control and before final grant installation. A 100 ms actor tick reports time-based restrictions; commands independently check current time immediately. Public record synchronization observes signed data without allocating mutations. Kind4 canonical `[1,4,null]` requests a fresh connection/proof by closing the existing one.

Remote records, accepted sequence floors and conflict evidence persist in the same encrypted Store before grants. Cache fallback supports a fresh connection proof only while the accepted record is unexpired/current and has no conflict/high-water violation. It emits an independent cache-used event. It never authorizes public mutation from cached absence. Unknown future observations remain conflict evidence through clock catch-up; rollback/competing records cannot silently become authority. The remote cache has no implicit high-water eviction.



#### TDD and debugging evidence

Initial focused REDs were compile failures for missing AuthHello/auth/connect/events/local endpoint integration, followed by successful real loopback proof. The first real fixture exposed a lifecycle gate race: waiting for status alone was insufficient, so the fixture now waits on the account actor snapshot barrier before connecting. An expiry test initially constructed an arbitrary competing packet and correctly encountered B2 RecordConflict; it was corrected to use legitimate completed-absence renewal rather than weakening B2 policy.

A behavioral future-record RED showed an unknown future remote observation could become cache authority after clock catch-up. `RemoteRecord::observe` now only accepts current same-membership renewal and preserves future conflict evidence; the focused regression passes. Receiver-failure topology initially connected a different first eligible installation; fixture seeds were sorted by actual InstallationId to establish the intended owner/remaining-peer/target links.

Retained focused RED logs:
- `task-3-recipient-red.log`: `receiver_store_failure...` failed because disconnected retry returned success. Durable recipient capture fixed it; `task-3-recipient-green.log`: 1 passed. Final test also covers sender reload.
- `task-3-expiry-red.log`: restricted-event count became 2 instead of 1 after clock rollback, proving an old grant was resurrected without a proof. Sticky registry restriction and explicit ready checks on controls fixed it; final auth suite passes.
- `task-3-recipient-schema.log` records a test-fixture failure: explicit Confirm was attempted before recording conflict. The fixture now first observes conflicting evidence; final amended unit suite passes. This was not a production-policy relaxation.
- `task-3-workspace-tests.log` records the first broad run's two obsolete B2 expectations (array8 and absent notifier). Only expected schema9/installed-empty-registry counts were updated as controller approved. The final broad run is green.
- `task-3-flutter-analyze.log` records exhaustive Dart switch failure after the new typed variants. Controller-approved documented no-op M2 cases resolved it.


#### Validation commands and actual results

Run from the worktree root unless noted. Logs are alongside this report.

1. `cargo test --workspace --features chat_core/test-hooks` -> exit 0, `task-3-workspace-tests-final.log`. All suites passed: 77 core unit, 13 account_auth, 11 account_endpoint, 8 account_keys, 10 account_lifecycle, 19 account_publication, 13 account_storage, 7 account_wire, 7 puppet, 17 transport, 12 outer chat_ffi tests plus its 1 child-process test (1 separately ignored fixture), 9 NSE, 5 decrypt integration, 5 doc tests. Summed log rows: 214 passed, 0 failed, 1 ignored. This broad run preceded the final durable-recipient/sticky-expiry amendments.
2. After the recipient amendment and transport admission self-review: `cargo test -p chat_core --features test-hooks --lib --test account_auth --test account_publication --test account_storage --test transport` -> exit 0; `task-3-amended-tests.log`: 78 + 13 + 19 + 13 + 17 = 140 passed, 0 failed. Includes fresh-empty real registry, fake notifier failure/order, new recipient strict schema/renew/rebase checks, real receipts and lifecycle/transport checks.
3. After sticky-expiry fix and strengthened target-close ordering evidence: `cargo test -p chat_core --features test-hooks --test account_auth` -> exit 0; `task-3-auth-final.log`: all 13 passed, 0 failed (1.22 s). Covers valid proof, actual endpoint mismatch, identity-key substitution, cross-connection replay, role/nonce alteration, cache-only limits, expiry/new proof/clock rollback, generation/conflict, own removal, actual three-peer revoke order, durable failure/retry/reload, future evidence, old array8 preservation, lifecycle cancellation, and expired public sync without mutation/grant.
4. `cargo check -p chat_core --release --features test-hooks` -> exit 0, optimized profile; `task-3-release-check-final.log` (2.36 s). Local test injection requires debug_assertions and does not compile into Release.
5. `C:/Program Files/Git/bin/bash.exe tools/gen_bindings.sh` -> exit 0 twice, `task-3-bindings.log` and `task-3-bindings-repeat.log`. Only standard script used; actual host chat_ffi DLL/chat_nse lib were built at generation time. Second generation compared seven generated-file hashes: zero changed. Four generated files differ from base; UniFFI generated sources remain unchanged. Later amendments are private core implementation/schema changes and do not alter generated API. Generator unit-struct/lifetime notices and package-update suggestions were informational.
6. From `app`: `C:/tools/flutter/bin/flutter.bat analyze --no-pub` -> `No issues found! (ran in 9.1s)`, exit 0; `task-3-flutter-analyze-final.log`.
7. From `app`: `C:/tools/flutter/bin/flutter.bat test --no-pub` -> 60 passed, exit 0; `task-3-flutter-tests.log`. Existing intentional failure-path tests print simulated failure stacks; suite result is green. Includes affected M2 stability/shared typed-event forwarding coverage. Rust conversion assertion is in the broad chat_ffi suite.
8. `git diff --check` and `git diff --cached --check` -> exit 0. Git emits usual LF-to-CRLF normalization notices; no whitespace errors.

Full broad suites were not repeated after the small final amendments, per controller ruling; affected auth/publication/storage/unit/transport suites and final Release check cover them. No Android build, iOS native compile/link, physical device, public relay/DHT, push, merge or upload is claimed. C4 still owns mandatory real iOS evidence.


#### Required strict CI gates — follow-up verification before independent review

Controller requested all three Rust CI gates after B3 source changes. Initial runs:

- `cargo clippy --workspace --all-targets --locked -- -D warnings` -> exit 101, `task-3-clippy-workspace.log`.
- `cargo clippy -p chat_core --all-targets --features test-hooks --locked -- -D warnings` -> exit 101, `task-3-clippy-hooks.log`.
- `cargo check --workspace --locked` -> exit 0, `task-3-check-workspace.log`: `Finished dev profile [unoptimized + debuginfo] target(s) in 8.72s`.

Both initial Clippy runs identified only four directly introduced `clippy::collapsible_if` diagnostics: nested result/conflict/accepted/hash checks at account manager lines 588/589; recipient-capture condition at account manager line 648; failed account-input route handling at transport manager line 971. They ended with `could not compile chat_core (lib) due to 4 previous errors` and the corresponding lib-test failure. Full exact diagnostic output is retained in those logs. No lint suppression was added.

The follow-up changes only collapse those conditions using Rust let chains, preserving short-circuit order, bindings, calls, errors and branch behavior. Self-review verified the complete two-file diff and absence of adjacent changes. No behavior, public API, schema or generated binding change; no broad suite repeat was needed.

Final gate commands and output:

```text
cargo clippy --workspace --all-targets --locked -- -D warnings
    Checking chat_core v0.1.0
    Checking chat_ffi v0.1.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.99s
exit 0

cargo clippy -p chat_core --all-targets --features test-hooks --locked -- -D warnings
    Checking chat_core v0.1.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.32s
exit 0

cargo check --workspace --locked
    Checking chat_core v0.1.0
    Checking chat_ffi v0.1.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.30s
exit 0
```

Exact full outputs, including absolute project paths and routine Cargo lock waits, are in `task-3-clippy-workspace-final.log`, `task-3-clippy-hooks-final.log`, and `task-3-check-workspace-final.log`. The check command was rerun after the lint-only source fixes.

Covering command: `cargo test -p chat_core --features test-hooks --locked --test account_auth --test account_publication --test transport`; full output in `task-3-lint-covering-tests.log`. Exit 0: account_auth 13 passed (1.25 s), account_publication 19 passed (0.51 s), transport 17 passed (30.23 s); total 49 passed, 0 failed, 0 ignored.

`git diff --check` is clean. The original three M2 dirty docs remain unstaged and untouched. Follow-up scoped commit/final HEAD: `9a61c9b31e89ec2e4c1f8051b447222ad0915af6` (`style(identity): satisfy strict account integration lint gates`).




#### Retained correction rounds

## Review fix round 1/5 — I1, I2, I3

Reviewed starting HEAD: `9a61c9b31e89ec2e4c1f8051b447222ad0915af6`. All three Important findings were reproduced with behavioral failures before their production fixes. No architecture ruling, public API, wire kind, schema or generated binding change was needed. Fix HEAD: `185445cf01c8b3e0a4134870f9794caf08918a7c` (`fix(identity): enforce account authority transitions`). Status: all three addressed, pending the same reviewer's fix-range review.

### I1 — received tombstones versus queued publication

Root cause: received-intent persistence only appended a tombstone, so an earlier Ready renewal could still retry its signed membership, including a removed receiver. Authentication closure was not a publication admission gate.

The received-intent transaction now holds every incompatible pending packet in the same commit as its new tombstone. `RecordState::removed` checks exact installation or transport removal; `can_publish` checks owned generation/local membership and exclusion of every tombstoned installation/transport. `restrict_pending` changes status only, retaining exact signed bytes/sequence, nonce, captured recipients and receipt obligations. It does not silently sign a new membership or manufacture notification success. Existing explicit Conflict is retained; otherwise affected pending work becomes LocalBlocked/Held.

The manager also rejects owned publication before lookup when the current installation is tombstoned, and checks `can_publish` on the exact queued signed packet before notification/public handoff. This is separate from unexpired auth readiness: initial creation/replacement and legitimate expired-known-base renewal continue through B2's existing table. Nothing blanket-gates the publisher on `local_authorized`.

Two actual loopback receiver regressions first leave a Ready renewal queued by injecting a publish failure, then deliver a correctly account-signed kind3 through a separately owned installation's actual endpoint/exporter proof. They cover both removal of another device and removal of the receiver itself. Both now deny stale publication against the same accepted base, completed absence, and after suspend/reload/automatic refresh, retaining the pending packet. Own-removal observes the actual connection close; the receiver commits before responding for the other-target case. Existing three-peer target-close-before-receiver-persistence/publication evidence remains green. A new library test verifies the exact packet bytes/sequence, revoke nonce, recipient set and pending notification survive Held persistence/redecode when another received tombstone invalidates that packet.

RED command:
`cargo test -p chat_core --features test-hooks --locked --test account_auth received_`

`task-3-fix1-i1-red.log`: exit 101, 0 passed / 2 failed. Exact failure messages:
- `stale Ready packet published after received revoke (own=false, absent=false)`
- `stale Ready packet published after received revoke (own=true, absent=false)`

Same command after fix: `task-3-fix1-i1-green.log`, exit 0, 2 passed / 0 failed, 0.32 s.

### I2 — explicit publication conflict without signed competitor evidence

Root cause: known `PublicationState::Conflict` could exist with `records.conflict == None` after publish returned RecordConflict; auth predicates checked only optional signed evidence. An unavailable lookup or known-packet clock rejection could also downgrade the explicit conflict to LocalBlocked.

Both local authentication admission and connection closure now treat explicit Conflict as loss of authority independently of optional signed evidence. Public synchronization is not launched from a conflicted local state. Unavailable lookup and known-packet clock rejection preserve an already-known explicit conflict; ordinary cache fallback and clock-only LocalBlocked remain distinct when no conflict existed.

The regression begins with a real current authenticated two-account connection, allocates a legitimate renewal, and makes publish return RecordConflict. It verifies Conflict with no signed competitor, false readiness, remote observation of actual connection closure, denial of fresh proofs, preservation across unavailable lookup and clock rollback, then legitimate signed conflict observation + explicit B2 confirmation + exact observation followed by a successful fresh proof.

RED command:
`cargo test -p chat_core --features test-hooks --locked --test account_auth publish_conflict_without_signed`

`task-3-fix1-i2-red.log`: exit 101, 0 passed / 1 failed: `known publish conflict retained the active grant`.
After the fix, `task-3-fix1-i2-green.log`: exit 0, 1 passed / 0 failed, 0.51 s.
After adding the explicit clock-rollback preservation assertion, the same focused command was rerun: `task-3-fix1-i2-final.log`, exit 0, 1 passed / 0 failed, 0.51 s. This last test-only edit preceded the final strict checks.

### I3 — different-account public-only record exchange

Root cause: kind2 rejected different-PeerId sessions and outgoing public synchronization selected only same-PeerId installations. The general expired-session public-only allowance therefore could not renew a remote account cache.

Owned accepted public records now synchronize over every retained authenticated account session, including different accounts and sessions restricted by expiry/renewal. Kind2 still binds decode/signature/current-time parsing to that connection's actual authenticated peer. Same-PeerId owned records keep the existing B2 `apply_record_change` path. Different PeerIds use a narrowly extracted `observe_remote(peer, signed, request)` method, shared with backend lookup, through the same bounded durable remote cache/high-water/conflict commit and grant restriction. No private control or grant is enabled by this observation. The same-PeerId revoke gate remains unchanged.

The two-PeerId loopback regression expires both sides, renews public records, observes each exact wrapper and sequence floor durably in the other side's encrypted Store mirror, checks no new ready events and exactly the two intended local renewal publishes (no remote-observation allocation), then uses a new proof with the backend unavailable to regain readiness from the updated cache.

RED command:
`cargo test -p chat_core --features test-hooks --locked --test account_auth different_accounts_exchange`

`task-3-fix1-i3-red.log`: exit 101, 0 passed / 1 failed: `different-account public renewal never reached durable remote cache: Elapsed(())`.
After fix, `task-3-fix1-i3-green.log`: exit 0, 1 passed / 0 failed, 0.53 s. A test-side Vec/Bytes comparison compile error was corrected before the retained behavioral RED; it did not drive a production change.

Additional actual raw-peer control coverage checks wrong-PeerId rejection, rollback, and signed future evidence. It exposed that rejecting the current-time parse before observation discarded future high-water evidence on the new control route. `task-3-fix1-i3-policy-red.log` records exit 101, 0 passed / 1 failed: `public control lost future observation policy`, floor `1800000001000000` versus required `1800000600000000`. The remote branch now computes current-time validation but commits verified observation/conflict evidence before returning rejection, exactly as remote lookup does; unusable evidence never grants readiness. Invalid expected-peer/signature/structure fails before any observation. The final affected suite includes this regression green; the owned B2 path retains its previous validation order.

### Final validation and self-review

Commands run from the worktree root:

```text
cargo test -p chat_core --features test-hooks --locked --lib --test account_auth --test account_publication --test transport
    79 library tests passed; 0 failed (2.50 s)
    18 account_auth tests passed; 0 failed (1.24 s)
    19 account_publication tests passed; 0 failed (0.41 s)
    17 transport tests passed; 0 failed (30.24 s)
exit 0 — 133 passed, 0 failed, 0 ignored
```

Full exact output: `task-3-fix1-tests.log`. This includes B2 creation/replacement/expired-known-base renewal, receipt/pending ordering, strict pending-recipient schema and real Store behavior, all original authentication tests, all five new integration tests, and the new packet/receipt-preservation unit test. The only later change was the additional I2 clock-rollback assertion, covered by its focused final command above. There was no codec change, so no separate storage suite rerun was required. Connection synchronization changed, so the transport suite was included.

```text
cargo clippy --workspace --all-targets --locked -- -D warnings
    Checking chat_core v0.1.0
    Checking chat_ffi v0.1.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.73s
exit 0

cargo clippy -p chat_core --all-targets --features test-hooks --locked -- -D warnings
    Checking chat_core v0.1.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 3.62s
exit 0

cargo check --workspace --locked
    Checking chat_core v0.1.0
    Checking chat_ffi v0.1.0
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 9.09s
exit 0
```

Full exact outputs (including routine Cargo lock waits/absolute paths): `task-3-fix1-clippy-workspace.log`, `task-3-fix1-clippy-hooks.log`, `task-3-fix1-check-workspace.log`. All strict checks ran after the final source/test edit. `git diff --check` and staged diff checks are clean apart from normal LF/CRLF notices.

Owned fix files only: `core/chat_core/src/account/manager.rs`, `publication.rs`, `connection.rs`, `core/chat_core/tests/account_auth.rs`. Test infrastructure adds controlled publish failure/attempt recording, actual raw installation proof/control helpers, and read-only Store mirrors; it adds no production test bypass. No transport manager, bridge, generated bindings, Flutter, public API, wire schema, or unrelated source changes. The three original dirty M2 docs remain untouched/unstaged.

Self-review checked the full four-file diff: tombstone and pending status commit atomically; held signed bytes and recipients survive; own-removal denies before network lookup and auto-refresh; B2 renewal/create exceptions still pass; explicit conflict survives unavailable/clock-only outcomes; public-only exchange never creates a grant, never crosses PeerId signature binding, and retains the same future/rollback durable floor policy; revoke stays same-PeerId-only. Production helper extraction is limited to sharing the already-existing remote acceptance path with the new kind2 branch. All local network fixtures use explicit pre-open local configuration and disabled relays; no public DHT/relay/device/remote actions occurred.

Remaining gate: same reviewer re-review of the fix range. No known unresolved I1/I2/I3 blocker. Existing C1 account projection and C4 real native iOS evidence remain outside this private fix scope, with no new claim of those results.


### B4 — retained task-reported evidence

Source: `.superpowers/sdd/2026-10-18-m3-b-identity-pairing/task-4-report.md`. Historical evidence at the identified commits; not a C4 rerun.


Status: DONE (implementation complete; independent review belongs to controller).
Base: 185445cf01c8b3e0a4134870f9794caf08918a7c.
Workspace: C:/Users/hyunw/orca/workspaces/ChatProject/m2-iroh-transport.
Commit: pending scoped commit; exact SHA appended below.


Public Rust exports from `chat_core::account`:

- `AccountHandle::create_invite(name: Option<String>) -> Result<String, CoreError>` (async).
- `parse_invite(input: &str, now: u64) -> Result<InvitePreview, CoreError>` (pure synchronous parser).
- `AccountHandle::accept_invite(input: String, alias: String) -> Result<ContactView, CoreError>` (async).
- `AccountHandle::contacts() -> Result<Vec<ContactView>, CoreError>` (async; controller-approved metadata API).
- `AccountHandle::confirm_safety(peer: PeerId, digest: [u8; 32]) -> Result<(), CoreError>` (async).
- `safety_digest(a: PeerId, b: PeerId) -> [u8; 32]`.
- `safety_display(a: PeerId, b: PeerId) -> SafetyDisplay`.

#### TDD / test-strength evidence (honest chronology)

The test file was written before source implementation. Initial command:
`cargo test -p chat_core --test account_invite`
Exit1, expected compile RED:
```
error[E0432]: unresolved imports `chat_core::account::parse_invite`, `chat_core::account::safety_digest`, `chat_core::account::safety_display`
error: could not compile `chat_core` (test "account_invite") due to 1 previous error
```

Additional executable negative control used a temporary reject-all parser branch and:
`cargo test -p chat_core --test account_invite --features test-hooks`
Exit1:
```
running 7 tests
name_64_bytes_accept_65_reject ... FAILED
unicode_controls_bidi_not_trusted_name ... FAILED
expiry_future_and_all_fields_signed ... FAILED
malformed_canonical_size_and_relay ... FAILED
actor::duplicate_keeps_alias_and_verified ... FAILED
test result: FAILED. 2 passed; 5 failed; 0 ignored; 0 measured; 0 filtered out
```
The failures included valid64-byte/63-byte Hangul names, a valid boundary-time invite, a valid HTTPS relay and valid actor acceptance. This negative control was run after the initial implementation had been written, then removed; it is test-strength evidence, NOT a claim of behavioral-before-implementation TDD chronology. Strict preimplementation executable behavioral RED was not captured. The initial missing-API RED above was captured before implementation.

After restoring actual parsing, focused GREEN:
`cargo test -p chat_core --test account_invite --features test-hooks`
`test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.48s`.

Test-development corrections: initial nested support-module imports/path were fixed before executable RED. An initially attempted immediate reconnect after suspend/resume returned NetworkNotStarted because networking had been quiesced; that unnecessary reconnect assertion was removed. Real fresh proof is already verified before reload; reload separately verifies durable notice/safety and device-unverified state. No product workaround or auth relaxation was introduced.


#### Verification commands and exact relevant output

1. Focused B3 real loopback proof integration:
`cargo test -p chat_core --features test-hooks --test account_auth device`
```
running 2 tests
invited_current_installation_proof_does_not_report_new_device ... ok
contact_device_status_requires_live_fresh_proof_and_preserves_safety ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 18 filtered out; finished in 0.64s
```
These use explicit local routes/fake backend and real authenticated connections, not mocked ready flags. They cover stale hint at first proof, current-device no notice, expiry, persisted same-PeerId verification/notice, and different-PeerId separation.

2. Full relevant core suite, run ONCE:
`cargo test -p chat_core --features test-hooks`
Full output: `b4-core-tests.log` next to this report. Exit0, no warnings/failures.
Counts: unit80; account_auth20; account_endpoint11; account_invite7; account_keys8; account_lifecycle10; account_publication19; account_storage13; account_wire7; puppet7; puppet_disabled0; transport17; doctests5. Total204 (80 unit +119 integration +5 docs).
Relevant lines:
```
test result: ok. 80 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.64s
test result: ok. 20 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.55s
test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.43s
test result: ok. 19 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.40s
test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.39s
test result: ok. 17 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 30.24s
test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.30s
```

3. Required strict workspace gate:
`cargo clippy --workspace --all-targets -- -D warnings`
Exit0 (`b4-clippy-workspace.log`):
```
Checking chat_core v0.1.0 (.../core/chat_core)
Checking chat_ffi v0.1.0 (.../app/rust)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 4.30s
```

4. Required strict core test-hooks gate:
`cargo clippy -p chat_core --all-targets --features test-hooks -- -D warnings`
Exit0 initial: `Finished ... in 3.60s` (`b4-clippy-hooks.log`).
Rerun only this gate after adding the actor-cap test, exit0 (`b4-clippy-hooks-final.log`):
```
Checking chat_core v0.1.0 (.../core/chat_core)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.28s
```

5. Required locked check:
`cargo check --workspace --locked`
Exit0 (`b4-check-locked.log`):
```
Checking chat_core v0.1.0 (.../core/chat_core)
Checking chat_ffi v0.1.0 (.../app/rust)
Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.65s
```

6. Narrow amendment after broad suite: persisted actual actor contacts-at-cap regression, plus final formatting/self-review:
`cargo test -p chat_core --test account_invite --features test-hooks`
Exit0 (`b4-final-invite.log`):
```
running 8 tests
actor::contact_capacity_rejects_without_eviction_or_store_change ... ok
actor::duplicate_keeps_alias_and_verified ... ok
expiry_future_and_all_fields_signed ... ok
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 2.41s
```
This cap test writes an explicit synthetic canonical1000-contact snapshot through the fake-protector Store after the previous CoreSlot has fully closed, reopens the real account actor, verifies views1000, rejects contact1001 with StorageFull and unchanged encrypted file/no eviction. It is test setup, not another product Store writer.
The invite test also explicitly closes/reopens CoreSlot and encrypted Store, retaining alias/safety state; suspend/resume alone is not the only restart evidence.

7. Final self-review strengthened the safety group assertion to compare all64hex with the expected digest, beyond length alone:
`cargo test -p chat_core --test account_invite symmetric_full_digest_and_same_peer_recovery`
Exit0 (`b4-final-safety.log`):
```
running 1 test
symmetric_full_digest_and_same_peer_recovery ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 5 filtered out; finished in 0.00s
```
No unrelated broad rerun. Final known coverage includes205 distinct tests (one actor-cap test added after the204-test full run).


#### Files / self-review / limitations

Owned source/test files only in commit:
- core/chat_core/src/account/{invite,contacts,safety}.rs (new).
- core/chat_core/src/account/{manager,mod,recovery,types}.rs.
- core/chat_core/src/account/storage/schema.rs.
- core/chat_core/tests/account_invite.rs (new).
- core/chat_core/tests/account_auth.rs (only schema fixture/assert changes, legacy8/9 rejection coverage, and2new proof/contact tests).
- core/chat_core/tests/account_publication.rs (outer schema assertion only; nested RecordState9 unchanged).

Self-review read the entire owned diff and the new codec files. Confirmed full signed body/cert/hint checks, canonical re-encoding, byte limits, duplicate preservation, no alias truncation, full safety display/QR identity, no key clone/debug/serde accessor, same writer/lifecycle/revision behavior, existing publication/cache/receipt preservation, current-grant-derived device status and sticky device notice. Fixed formatting only within owned additions/touched test blocks, not whole-manager formatting. `git diff --check -- <owned source/test paths>` produced no whitespace errors (Git may warn about repository LF/CRLF conversion for types.rs).

Pre-existing manager.rs is large; this task adds commands and contact state operations in its existing ownership model without restructuring it. No known functional blocker. Legacy unshipped snapshots intentionally fail unchanged. A new-device notice has no reset by approved scope. Invite creation currently uses optional null relay; contacts do not persist/dial untrusted relay hints. Rust-only public APIs and current contact metadata are ready for C1/C2 integration; no bridge generation/UI/pairing/MLS/conversation behavior was added.

All external services were fake/offline or explicitly local loopback test services configured before binding. No public network, devices, push, merge, upload, original checkout/provider/transcript mutation or memory writes. Evidence is Windows host Rust/loopback only, not physical mobile/platform integration. The3original dirty M2 docs remain untouched and unstaged. This UTF-8 report/logs are task artifacts under ignored .superpowers, not staged source. Controller owns independent review.


### B5 — retained task-reported evidence

Source: `.superpowers/sdd/2026-10-18-m3-b-identity-pairing/task-5-report.md`. Historical evidence at the identified commits; not a C4 rerun.


Status: DONE_WITH_CONCERNS (upstream Snow memory-erasure residual; B6 durable transfer intentionally remains closed).
Base: `eedcf6f2e0e22bf78e9c8f13ca26417be2256d1d`.
Checkout: `C:/Users/hyunw/orca/workspaces/ChatProject/m2-iroh-transport`.
Commit: `678eaa4e713c4e1c3ea8556dfd477edefb4cd076` — `feat(pairing): add Noise pairing and explicit approval gates`.
Postcommit status verified: only the three original dirty M2 documents remain unstaged; all eleven owned product files are committed.


Implemented the approved B5 QR directions, real Snow channel, InstallationKey proofs, fresh E membership checks, local comparison and E-only approval gates. AccountHandle commands execute in the existing sole protected-state actor. No second Store writer, identity owner, secret getter, storage schema extension, event/FRB/Dart change, or new durable transaction was introduced. Existing active private array10, candidate37B, contacts, remote cache, publication receipts/tombstones remain the predecessor codecs.

Public account exports:

- `PairDirection::{NewDisplays,ExistingDisplays}`.
- `PairDisplay { flow:FlowId, qr:String, expires_at:u64 }`: one-shot response, no Debug/serialization, QR zeroized on Drop.
- `PairPhase::{WaitingQr,Comparing,Approved}`: the phases implemented in B5; B6/C1 own transfer/publication/UI phases.
- `PairView { flow,direction,phase,sas:Option<String>,expires_at,disclosure_possible }`: public data only.

#### Exact B6 seam and fixture limits

`account::pair::session::DisclosurePermit` is an opaque crate-internal type with private exact flow and Noise handshake-hash fields. It has **no production constructor/provider in B5**. Only `Session::fixture_permit` exists under `cfg(all(feature="test-hooks",debug_assertions))`.

`Session::send_gated(&mut self, clock:&dyn Clock, permit:DisclosurePermit, build:impl FnOnce()->Zeroizing<Vec<u8>>)->Result<usize,CoreError>` independently verifies live expiry, E role, E local comparison, remote N comparison, explicit E approval, exact flow/hash and candidate endpoint+installation pins before calling `build`. Thus the future secret builder is not invoked while gates are closed. It marks the ephemeral disclosure flag before writing, and returns actual written ciphertext+u32 framing length. Raw Channel send is restricted to the pair module; the account actor cannot bypass this boundary via public send-stream access.

B6 must add a narrow production permit issuance path only after its sole actor commits the actual approval/disclosure transaction, then build the exact PairPayload under this gate. It must retain the real tx/candidate/generation/base binding, implement exact transfer envelope/chunk/receipt codecs, capped4MiB transfer, receipt-before-ack persistence, retry/reconciliation, latest membership before activity, and prove actual Store failure ordering. B5 production has no private payload provider, receipt, durable disclosure, identity import, membership add or success claim.

Debug AND test-hooks only:

- `AccountHandle::pair_fixture_send(flow,Vec<u8>)->Result<usize,CoreError>` uses a bounded <=16KiB opaque synthetic fixture through the real send gate and real Snow transport. Its explicit fixture permit substitutes only for the future durable commit.
- `pair_fixture_received(flow)->Result<Vec<u8>,CoreError>` exposes received synthetic bytes; the test-only receive path is bounded to16KiB and requires N local confirmation plus remote E confirmation. These bytes have no production transfer kind/schema. Production accepts only confirmation after the ordered handshake.
- Tests assert denied preapproval sends and empty receiver, then nonzero ciphertext length greater than plaintext and exact successful decrypted bytes after approval. This is not an always-zero stub and is not evidence of B6 durable identity transfer.
- Final Release compiler probe, even with test-hooks enabled, reports E0599 for both fixture methods. The release pairing test binary has zero tests by cfg.


#### TDD chronology and verification evidence

Initial executable behavioral RED **before feature logic** used a minimal compiling public API skeleton. Command:

`cargo test -p chat_core --features test-hooks --test account_pair_channel`

Output: compiled successfully; five named tests FAILED, `0 passed; 5 failed`, each reaching `pair_begin` and reporting `CoreError { code: NotAvailable, detail: "pairing not implemented" }`. The initial five tests exercised both directions and the missing actor behavior; extra adversarial cases were added during focused implementation/self-review, not represented as having all failed initially.

Initial GREEN of those five: same command, `5 passed; 0 failed`. The first postimplementation run had two failures because confirmation delivery is asynchronous; the test now waits on the real approval result with a bounded condition loop, rather than treating send completion as remote processing. Other iteration compile/test corrections included a test moving a Drop-owned QR string, a test PSK slice type, and a raw adversarial writer initially unwrapping an expected early connection close. No mock replaced Noise, endpoint binding, gate state or frame decoding.

Expanded focused GREEN: 16/16, followed by focused candidate-bind and restart cases. Final pair suite is 19/19; command:

`cargo test -p chat_core --features test-hooks --test account_pair_channel --locked`

Output: `19 passed; 0 failed; 0 ignored`, finished1.43s. Coverage includes:

- Both-direction same SAS, both local confirmations, zero gated bytes before E local approval, N remote confirmation never E approval.
- Actual endpoint/role mismatch, wrong PSK three failures in both directions, forged installation signature, old handshake hash, endpoint+installation candidate replacement denial and same-candidate retry.
- Noise ciphertext replay, AEAD corruption, forged remote approval, oversized length prefix, malformed/noncanonical/extra-field QR, expiry and unknown scheme/resume rejection before candidate write.
- Fresh versus cached E authority, N remains inactive candidate on failed validation, 300s total/60s postapproval expiration, mismatch/cancel/suspend, candidate persistence before QR/bind failure, restart dropping flow/PSK, fresh session independence.

Full relevant core feature suite was run **once**:

`cargo test -p chat_core --features test-hooks --locked`

Result: **226 passed; 0 failed**: unit81, account_auth20, account_endpoint11, account_invite8, account_keys8, account_lifecycle10, account_pair_channel18, account_publication19, account_storage13, account_wire7, puppet7, puppet_disabled0, transport17, doctests7. Full local output: `target/b5-core-suite.log` (ignored artifact). Includes `transport::wrong_alpn_and_magic_are_rejected ... ok`, existing account/protected storage tests and SAS vector. All observed output was inspected; no hidden known failing test.

After that full-suite run, self-review found E `NetworkOp::Start` returned before binding after an explicit stop. A new regression was written/run first:

`cargo test -p chat_core --features test-hooks --test account_pair_channel existing_display_waits_for_rebound_endpoint_before_returning_qr`

RED: `assertion failed: e.a.pair_begin(PairDirection::ExistingDisplays,None).await.is_err()` under injected failed bind, `0 passed; 1 failed`. Fix: bounded lifecycle-cancellable wait for actual Online before displaying QR; failed bind errors and leaves no pair. Same targeted command GREEN `1 passed; 0 failed`. The final19-test pair run above verifies this narrow amendment. The226-test full-suite result precedes this amendment; it was not rerun merely to inflate provenance.

Final strict gates after amendments all exit0:

- `cargo clippy --workspace --all-targets --locked -- -D warnings`
- `cargo clippy -p chat_core --all-targets --features test-hooks --locked -- -D warnings`
- `cargo check --workspace --locked`
- `cargo test -p chat_core --release --features test-hooks --test account_pair_channel --locked` — optimized build, 0 tests by required cfg.
- `git diff --check` — no whitespace error (Git only emitted existing line-ending notices).

Initial Clippy iterations found new channel test-module placement and three pre-existing manager unit-fixture initializers needing the newly introduced pair runtime field; both were corrected. Final gates are clean. Release negative probe used `rustc --edition=2024 --crate-name b5_release_probe target/b5-release-probe.rs --extern chat_core=<fresh release rlib> -L dependency=target/release/deps --emit=metadata`; expected exit1 with exactly two `E0599` errors: no methods `pair_fixture_send`/`pair_fixture_received`. This is positive evidence of hook exclusion, not a build failure claimed as success.


#### Dependency evidence and memory-erasure limitation

Pinned exact `snow = "=0.10.0"`. Downloaded via Cargo dependency fetch only. Primary local crate source at `C:/Users/hyunw/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/snow-0.10.0`:

- `Cargo.toml` rust-version1.85; host compiler observed `rustc 1.92.0 (ded5c06cf 2025-12-08)`.
- `tests/vectors/cacophony.txt:9004` contains exact `Noise_NNpsk0_25519_ChaChaPoly_SHA256`; `tests/vectors.rs` consumes official/cacophony vectors. Availability/source was inspected; the upstream vector harness itself was not run. The actual selected pattern was exercised by our real two-party transport tests.
- `builder.rs` supports PSK slot0 and prologue; `handshakestate.rs` exposes safe set_psk; `transportstate.rs:117` exposes rekey_manually.
- Source audit found no Zeroize feature/Drop guarantee for Snow handshake PSK/chaining/ephemeral copies or default cipher key arrays (`resolvers/default.rs`).

After fix1, application-owned PSK, binary and base64 QR temporary buffers, scan input, handshake output scratch and plaintext fixture buffers use Secret32/Zeroizing; display QR zeroizes on Drop. The original report incorrectly generalized the binary-buffer cleanup to the separate ordinary base64 String allocation; I1 identified that omission, and fix1 now explicitly wraps the base64 String in Zeroizing before formatting. A small RAII handshake wrapper best-effort overwrites retained PSK slot0 on failure/cancellation and before conversion; Channel Drop overwrites both transport keys through Snow's public rekey_manually, then closes the connection. Snow owns AEAD/nonces; no unsafe memory writes, custom crypto, vendor fork or new protocol were added.

**Residual:** safe overwrite APIs do not guarantee optimizer-proof erasure of Snow's internal prior copies/chaining/ephemeral state. Freed memory may retain those until reuse. This is explicitly tracked, not a full-wipe guarantee. Controller independently verified and authorized continuing with this reported limitation for review.


#### Files, self-review and limits

Owned implementation files: `Cargo.lock`, `core/chat_core/Cargo.toml`, `core/chat_core/src/account/mod.rs`, `core/chat_core/src/account/manager.rs`, new `core/chat_core/src/account/pair/{mod,wire,channel,session}.rs`, `core/chat_core/src/transport/{endpoint,manager}.rs`, new `core/chat_core/tests/account_pair_channel.rs`.

This UTF-8 report is in the existing ignored `.superpowers` controller-artifact area, not forced into the product commit. No original dirty M2 docs were touched or staged. Root owns review/memory; no helpers/subagents/reviewers were spawned. Tests configured local profile/backend/Clock before actor open; loopback target addresses were installed after endpoints existed using the existing debug local-only hook. No public DHT/relay, physical devices, push, merge, upload, original checkout/provider/transcript mutation occurred.

Self-review examined wire canonicality/prologue role ordering, actual remote pin, cert/record freshness, bounded allocations, cancellation/expiry, production permit absence, sender visibility, snapshots and final diff. It fixed the stopped-E QR-before-bind bug with executable RED/GREEN and narrowed raw Channel write visibility. Existing manager is now ~2300 lines; the added actor integration remains scoped but size is a maintainability concern. No unrelated restructuring was attempted.

B6 durable payload/import/receipt/order proofs and C1 UI/bridge projection remain explicit next-task work. Native iOS compilation/link/runtime and physical-device behavior were not exercised and are not inferred from host tests; C4 remains mandatory.



#### Retained correction rounds

## Fix round 1 — I1/I2, 2026-10-18

Status: implemented both Important findings; awaiting the controller-owned scoped rereview. Original reviewed base `678eaa4e713c4e1c3ea8556dfd477edefb4cd076`; fix commit **`fe29a9a638427b61042d1cdef42ae3be671c3b06`**, `fix(pairing): zeroize QR encoding and recheck authority time`.

### Changes and self-review

- **I1:** `Qr::display` now binds the base64 encoder's returned String to `Zeroizing<String>` before formatting the final display response. The canonical binary temporary was already Zeroizing-owned; the returned QR remains owned by PairDisplay and zeroizes on Drop. There is no additional ordinary base64 String clone. This removes the avoidable app-owned allocation identified by review. This ownership fix is independent of the still-disclosed upstream Snow internal-memory-erasure residual. Verified by direct ownership inspection and the existing complete QR/real-channel pairing coverage. No unsafe freed-heap inspection, historical memory-observation RED, or full-memory-erasure claim is made.
- **I2:** private `Establish` takes `clock:&dyn Clock`, supplied from the actor's existing service Clock, instead of an eagerly sampled scalar. After the final `backend.lookup(peer).await`, it takes one current wall-clock sample and applies the certificate creation-time allowance, record issue-time validation, signed-record sequence freshness and expiry checks using that sample. No subsequent network await occurs before returning the verified channel. Public APIs/wire/schema remain unchanged.
- Test fixture now accepts the already-supported `RecordBackend` trait and an explicitly supplied FakeClock before account open. A dedicated backend actually yields at lookup, updates N's FakeClock, and returns a real independently signed public record. E remains a real local actor with actual protected state; no ready/grant/Noise state is mocked. Fixed test-owned recovery entropy signs the bounded timestamp fixtures without reading private account state. The fixture epoch is anchored to E's public certificate timestamp because current account creation uses SystemClock; every tested time change then uses FakeClock.
- Four independently isolated rejection cases: record expiry crossed during lookup; rollback making certificate creation too far ahead while record times remain valid; rollback making record issuance too far ahead while cert/sequence remain valid; rollback making sequence too far ahead while cert/issuance remain valid. A fifth control keeps all authority valid after lookup and must reach Comparing. All assert QR wall expiry and monotonic lifetime remain live, so QR expiry cannot hide the bug.
- The changed product files are only `core/chat_core/src/account/pair/wire.rs`, `core/chat_core/src/account/pair/channel.rs`, and the one-line argument change in `core/chat_core/src/account/manager.rs`; direct regressions are in `core/chat_core/tests/account_pair_channel.rs`. Commit stat: four files,159 insertions,12 deletions. No N DeviceKind/carrier work, schema, protected-storage codec, transport dispatch, public API, fixture cfg boundary, or durable permit constructor changed.

### RED provenance

First fixture run failed before channel verification because recovery publication had not yet been freshly observed (`Option::unwrap` on absent accepted record). That setup-only failure is **not** claimed as behavioral RED. The fixture was corrected by explicitly refreshing E's record before constructing the independently signed response; product code was still unchanged.

Then this command produced the genuine pre-fix behavioral RED:

`cargo test -p chat_core --features test-hooks --test account_pair_channel authority_time_ --locked`

Retained relevant output (exit1):

```text
   Compiling chat_core v0.1.0 (C:\Users\hyunw\orca\workspaces\ChatProject\m2-iroh-transport\core\chat_core)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 3.32s
     Running tests\account_pair_channel.rs (target\debug\deps\account_pair_channel-8c14dcd300b3befe.exe)

running 5 tests
test authority_time_fresh_after_lookup_still_compares ... ok
test authority_time_rollback_rechecks_certificate_creation ... FAILED
test authority_time_rollback_rechecks_record_sequence ... FAILED
test authority_time_rollback_rechecks_record_issue_time ... FAILED
test authority_time_expiry_during_lookup_rejects_while_qr_live ... FAILED

---- authority_time_rollback_rechecks_certificate_creation stdout ----
thread 'authority_time_rollback_rechecks_certificate_creation' (42460) panicked at core\chat_core\tests\account_pair_channel.rs:957:9:
N accepted authority invalid at lookup completion

---- authority_time_rollback_rechecks_record_sequence stdout ----
thread 'authority_time_rollback_rechecks_record_sequence' (37900) panicked at core\chat_core\tests\account_pair_channel.rs:957:9:
N accepted authority invalid at lookup completion

---- authority_time_rollback_rechecks_record_issue_time stdout ----
thread 'authority_time_rollback_rechecks_record_issue_time' (16296) panicked at core\chat_core\tests\account_pair_channel.rs:957:9:
N accepted authority invalid at lookup completion

---- authority_time_expiry_during_lookup_rejects_while_qr_live stdout ----
thread 'authority_time_expiry_during_lookup_rejects_while_qr_live' (42576) panicked at core\chat_core\tests\account_pair_channel.rs:957:9:
N accepted authority invalid at lookup completion

test result: FAILED. 1 passed; 4 failed; 0 ignored; 0 measured; 19 filtered out; finished in 0.29s
```

### GREEN and affected coverage

After the fixes, the same focused command returned exit0:

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 7.10s
running 5 tests
test authority_time_rollback_rechecks_certificate_creation ... ok
test authority_time_rollback_rechecks_record_issue_time ... ok
test authority_time_expiry_during_lookup_rejects_while_qr_live ... ok
test authority_time_fresh_after_lookup_still_compares ... ok
test authority_time_rollback_rechecks_record_sequence ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 19 filtered out; finished in 0.29s
```

Complete affected pairing suite ran once after these fixes:

`cargo test -p chat_core --features test-hooks --test account_pair_channel --locked`

Retained output (exit0):

```text
    Finished `test` profile [unoptimized + debuginfo] target(s) in 0.49s
     Running tests\account_pair_channel.rs (target\debug\deps\account_pair_channel-8c14dcd300b3befe.exe)

running 24 tests
test qr_requires_durable_candidate_and_successful_endpoint_bind ... ok
test restart_keeps_candidate_but_discards_flow_psk_and_approval ... ok
test malformed_expired_qr_and_resume_fail_closed_before_candidate_write ... ok
test noncanonical_qr_extra_fields_and_wrong_owner_role_reject_without_write ... ok
test oversized_frame_prefix_closes_without_payload_allocation ... ok
test existing_display_waits_for_rebound_endpoint_before_returning_qr ... ok
test forged_installation_proof_never_reaches_code_comparison ... ok
test authority_time_rollback_rechecks_certificate_creation ... ok
test cached_existing_record_cannot_replace_fresh_pair_authority ... ok
test authority_time_rollback_rechecks_record_sequence ... ok
test authority_time_rollback_rechecks_record_issue_time ... ok
test authority_time_fresh_after_lookup_still_compares ... ok
test authority_time_expiry_during_lookup_rejects_while_qr_live ... ok
test replayed_noise_frame_is_rejected_and_new_qr_is_independent ... ok
test aead_corruption_and_remote_approval_message_close_channel ... ok
test wrong_endpoint_role_qr_candidate_replay_denied ... ok
test three_failures_and_five_min_expiry ... ok
test actual_endpoint_role_and_old_handshake_proof_are_bound ... ok
test remote_confirm_is_not_existing_local_approval ... ok
test same_sas_both_local_confirmations ... ok
test zero_gated_bytes_before_existing_local_approval ... ok
test failed_candidate_cannot_be_replaced_but_same_candidate_can_retry ... ok
test three_bad_psk_handshakes_destroy_qr_in_both_directions ... ok
test approval_deadline_rejection_and_lifecycle_close_channels ... ok

test result: ok. 24 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.53s
```

Direct affected pair unit command:

`cargo test -p chat_core --features test-hooks --lib account::pair:: --locked`

Retained output (exit0):

```text
   Compiling chat_core v0.1.0 (C:\Users\hyunw\orca\workspaces\ChatProject\m2-iroh-transport\core\chat_core)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 6.34s
     Running unittests src\lib.rs (target\debug\deps\chat_core-3c6b0937c6981c18.exe)

running 1 test
test account::pair::channel::tests::sas_fixed_hkdf_sha256_vector ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 80 filtered out; finished in 0.00s
```

### Final strict gates, retained exact outputs

Command: `cargo clippy --workspace --all-targets --locked -- -D warnings` (exit0):

```text
    Checking chat_core v0.1.0 (C:\Users\hyunw\orca\workspaces\ChatProject\m2-iroh-transport\core\chat_core)
    Checking chat_ffi v0.1.0 (C:\Users\hyunw\orca\workspaces\ChatProject\m2-iroh-transport\app\rust)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.52s
```

Command: `cargo clippy -p chat_core --all-targets --features test-hooks --locked -- -D warnings` (exit0):

```text
    Checking chat_core v0.1.0 (C:\Users\hyunw\orca\workspaces\ChatProject\m2-iroh-transport\core\chat_core)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.56s
```

Command: `cargo check --workspace --locked` (exit0):

```text
    Checking chat_core v0.1.0 (C:\Users\hyunw\orca\workspaces\ChatProject\m2-iroh-transport\core\chat_core)
    Checking chat_ffi v0.1.0 (C:\Users\hyunw\orca\workspaces\ChatProject\m2-iroh-transport\app\rust)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.70s
```

`git diff --cached --check` passed before commit. No new full-core or Release suite was run: this fix changes no transport routing/session or public-test-hook/cfg boundary, so the previous full-core226/M2/Release compiler-exclusion evidence remains explicitly prior-commit provenance, as requested. Final postcommit status shows only the three original dirty M2 documents, all unstaged. This report remains an ignored UTF-8 controller artifact, not forced into the product commit.

Remaining limits: I1's app-owned intermediate ownership is verified structurally and by existing canonical/channel behavior, not by inspecting allocator residue. Snow's upstream internal-copy erasure limitation remains exactly as previously disclosed. B6 durable disclosure/import and native iOS/device evidence remain outside this scoped fix. No subagents, public service/device execution, upload/push/merge, original checkout, or protected-document edits occurred.


### B6 — retained task-reported evidence

Source: `.superpowers/sdd/2026-10-18-m3-b-identity-pairing/task-6-report.md`. Historical evidence at the identified commits; not a C4 rerun.


Status: DONE_WITH_CONCERNS (completed; existing large account actor and explicit validation limitations below).
Commit: dd14e57442078c75ae66433cdfa39b7e082ebcef — feat(pairing): persist transfer receipts and safe resume
Base: fe29a9a638427b61042d1cdef42ae3be671c3b06
Workspace: C:/Users/hyunw/orca/workspaces/ChatProject/m2-iroh-transport
Branch: HyunwookYoo/m2-iroh-transport


The existing account actor remains the sole Store writer. After fresh B5 proofs, both confirmations and local E approval, E stores the exact transaction/payload and conservative disclosure history before issuing a private commit-backed permit. The permit binds the current flow, Noise hash, candidate installation/transport/kind and accepted authority. Current authority is checked again after the synchronous Store commit and before each gated frame. An error, including successful replacement followed by a reported commit error, leaves the actor locked and sends zero further gated bytes.

N commits the exact E PeerId/installation/transport plus transaction/hash/size on the envelope before accepting any payload chunk. It then validates the identity seed-derived PeerId, N certificate and its own keys/kind, exact payload, contact bounds/digests, ordered chunk lengths and final SHA256. The imported payload, receipt and record floors are committed atomically before sending the channel-bound receipt. RecoverySeed and N DeviceSeed never enter transfer wire.

E verifies and stores the actual N signature bound to this flow/handshake before fresh membership/base/capacity checks and a durable B2 Add intent. N activates exclusively after a fresh backend lookup, current time sampled after the await, exact own membership and retained E membership, generation/floor/conflict checks, and an activation snapshot. Backend unavailability, expiry or conflicts do not produce success. Pending N uses a one-second poll only while the live pairing exists; explicit refresh_record supports restart. No status wire was added.

Resume uses fresh QR/PSK/Noise/SAS/confirmations and fresh E approval. Exact payload, tx, E/N keys and candidate kind remain pinned. Active N origin resumes in the New role and updates only its receipt; it cannot replace its keys or contacts. Pending imports cannot use normal account APIs. Already-observed inclusion does not allocate/publish a duplicate add.


#### Executable TDD evidence

Initial command, before feature logic:
`cargo test -p chat_core --features test-hooks --test account_pair_recovery -- --nocapture`

Actual RED: `durable_transfer_activates_exact_candidate` failed with:
`durable receipt and observed membership must activate the same candidate`
`test result: FAILED. 0 passed; 1 failed; ... finished in 3.21s`
This ran real local account actors/Noise and failed because B5 did not import/activate. An earlier mistaken path invocation found no target and is not counted as RED.

The same real behavioral test passed after implementation; expanded integration suite subsequently passed 14/14 (details below).

Self-review introduced `authority_rechecked_after_disclosure_store_commit_before_first_byte`. A real KeyProtector callback shifts the FakeClock back 301 seconds during the actual disclosure commit, making the accepted record future-invalid without changing the monotonic live-pair deadline. Executable RED:
`assertion failed: e.a.pair_approve(f).await.is_err()`.
After adding post-commit authority checking and per-frame authority validation, GREEN:
`test authority_rechecked_after_disclosure_store_commit_before_first_byte ... ok`
`test result: ok. 1 passed; 0 failed; ... finished in 0.58s`.
The assertions require zero gated bytes and retained durable disclosure history.


#### Verification commands and results

Full feature suite (before final allocation-only tightening):
`cargo test --locked -p chat_core --features test-hooks`

Exact successful groups: unit86, account_auth20, account_endpoint11, account_invite8, account_keys8, account_lifecycle10, account_pair_channel24, account_pair_recovery14, account_publication19, account_storage13, account_wire7, puppet7, puppet_disabled0, transport17, doc-tests7. Total 251 passed, zero failed. Full output retained locally at `task-6-full.log` beside this report.

B6 full-run output: `test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 17.07s`.
B5 full-run output: `test result: ok. 24 passed; 0 failed; ... finished in 3.17s`.
Unit output: `test result: ok. 86 passed; 0 failed; ... finished in 2.47s`.

An earlier full run passed86 units then failed the20 auth fixtures because their handwritten stored array10 layouts had not yet appended pair state. That was a real fixture integration failure, not a product bypass. Only direct schema construction/length expectations were corrected; old-layout refusal now covers8/9/10. Focused auth20/invite8/publication19 then passed; the successful full run above followed. No redundant full run was performed after the final allocation-only amendment.

After that final amendment:
`cargo test --locked -p chat_core --features test-hooks --test account_pair_recovery --test account_auth`
- auth: `20 passed; 0 failed; ... finished in 1.38s`
- B6: `14 passed; 0 failed; ... finished in 12.96s`
`cargo test --locked -p chat_core --features test-hooks --lib account::storage::schema`
- `2 passed; 0 failed; 84 filtered out; finished in 0.02s`

Final exact checks, all exit0 after the amendment:
- `cargo clippy --locked -p chat_core --features test-hooks --all-targets -- -D warnings` — Finished dev in2.43s.
- `cargo clippy --locked --workspace --all-targets -- -D warnings` — Finished dev in2.34s.
- `cargo check --locked --workspace` — Finished dev in1.71s.
- `cargo check --locked --release -p chat_core --features test-hooks` — Finished release in2.33s.
- `git diff --check` — no whitespace errors; Git emitted existing LF/CRLF normalization notices only.

Release exclusion proof: a temporary local `core/chat_core/examples/b6_fixture_exclusion.rs` imported PairTestStop and referenced AccountHandle::pair_test_stop/pair_test_bytes. Command:
`cargo check --locked --release -p chat_core --features test-hooks --example b6_fixture_exclusion`
Expected actual compiler failures: E0432 unresolved PairTestStop (compiler points to `#[cfg(all(feature="test-hooks",debug_assertions))]`), E0599 no pair_test_stop, E0599 no pair_test_bytes. Compiler resolved chat_core from this workspace's `core/chat_core`; base HEAD was fe29a9a with this owned working tree, not a registry or original-checkout library. Output is in `task-6-release-exclusion.log`. Probe was removed after the expected failure. The normal Release library check passed, including after the allocation amendment.

Focused tests additionally verify changed E/candidate/tx/hash/size pins; actual tampered stored E payload rejection; exact same-tx restart with fresh approval only; unpublished transaction-only rights; already-published duplicate avoidance; 1000 verified contacts crossing multiple actual Noise chunks without inheriting device trust; cancel/timeout warning persistence. Channel tests retain preapproval0, SAS, mismatch/expiry/cancel, and final-lookup current Clock assertions.


#### Self-review and remaining limits

- Fixed current authority changing during disclosure Store commit; captured behavioral RED/GREEN above.
- Fixed exact payload tx vs envelope binding, current N platform claim, and resumed Active N role preservation while reviewing the implementation.
- Tightened secret snapshot allocation after the successful full suite; final focused and strict checks above cover the final tree.
- Existing account/manager.rs is large and now contains the required serial orchestration. No unrelated split/refactor was introduced; maintainability is a concern for controller review.
- Retained E transaction/history and exact payloads consume bounded snapshot capacity; history is deliberately not evicted. Reaching count/4MiB capacity fails closed. There is no history-pruning API in this task.
- Partial OS/blob deletion followed by process restart without retained never-active provenance cannot safely prove eligibility. That restart fails closed; only an actor retaining proven never-active state and explicit cleanup flag may retry cleanup. No arbitrary failed-load reset exists.
- Pinned snow0.10 internal handshake/PSK/chaining/ephemeral copies lack guaranteed erasure. App-owned buffers are zeroized; no unsafe workaround/crypto fork or complete-memory-erasure claim.
- Tests use actual local iroh endpoints, Noise, actor/Store paths with explicit fake backend/Clock/KeyProtector, configure_local before open. No public DHT/relay or physical device/native-platform keychain validation was performed. The crash table is operation-boundary/reopen evidence, not exhaustive per-byte power-loss testing.
- C1 owns UI wording/projection for public warning fields; B6 adds no new UI. No push, merge, publication, provider/transcript edits, original-checkout writes, or helper agents were used.

Post-commit verification: git status --short contains only the three original unstaged M2 documents. The commit contains exactly the18 listed owned source/test files; report/evidence logs remain local ignored artifacts.




#### Retained correction rounds

# B6 fix round 1 — I1 through I5

Reviewed base: dd14e57442078c75ae66433cdfa39b7e082ebcef.
Fix commit: 23846019be1dbbb8d003de3d1dd24a53e570b6a4 — fix(pairing): close activation and resume boundary gaps.
Status: DONE_WITH_CONCERNS: all five findings addressed; the approved conservative activation/discard consequence and existing external-validation limits remain explicit below. This section supersedes the original report's incomplete contact-erasure and crash-table evidence descriptions.

## I1 — post-Store N activation authority

After committing Active, refresh_import now rechecks current authority through local_authorized, which includes current Clock, lifecycle request, record validity, generation/floors, membership, conflict and tombstone state. Failure returns AccountRestricted, clears pairing, closes grants, blocks the network gate and locks the actor until an actual reload. It does not set PairPhase::Active or return successful refresh.

The already-committed Active snapshot is retained as established and permanently ineligible for never-active discard. This explicit controller-approved conservative policy has a rare cost: if time becomes invalid during the activation commit, N loses never-active-discard eligibility even though public activation failed. There is no fake rollback to Candidate/PendingImport and no second corrective commit with another ambiguity window. A still-invalid record after reload projects non-Active with PublicationState::LocalBlocked; ordinary connection/revocation authority stays denied. Normal fresh-current Active refresh can restore authority later. AccountView now requires current local authorization before mapping durable Observed to public Active/Observed.

Regression `activation_commit_clock_change_is_error_and_reload_never_claims_active` uses the real KeyProtector unprotect callback during the actual activation Store commit to move N's FakeClock back301 seconds. It asserts refresh Err, immediate Locked/public pair rejection, actual decrypted durable Active array11/tag1, reload non-Active/LocalBlocked/no pair, connect/revoke denial, discard refusal, then restoration by current-time normal refresh. Initial create_invite-after-reload assertion was corrected: that pre-existing API is intentionally local-only and allowed in LocalReady; it is not ordinary membership authority. No invite policy was changed.

Executable RED before the I1 logic change:
`cargo test --locked -p chat_core --features test-hooks --test account_pair_recovery activation_commit_clock_change -- --nocapture`
`time-invalid activation commit must not report success`
`test result: FAILED. 0 passed; 1 failed; ... finished in 1.16s`.
Final focused GREEN: `1 passed; 0 failed; ... finished in 1.29s`; also passed in final affected suite.

## I2 — finite live import polling only

Removed PendingImport from unconditional auto_refresh. Startup, lifecycle completion and60-second Active refresh retain only the normal Active-account behavior. PendingImport now has only its one-second live-pair path (bounded by existing pair expiry) and explicit refresh_record.

`cancelled_and_restarted_import_has_no_automatic_lookup` counts actual backend lookups. After cancellation, it advances paused Tokio time through three61-second intervals with timer scheduling barriers; count stays unchanged. Restart causes no lookup or activation; three more61-second intervals remain silent. One explicit refresh performs exactly one lookup and activates from the already-current published membership. E is suspended during the counting period to isolate N. Fake backend is shared explicitly, no public network backend is used.

Executable RED before removal:
`cargo test --locked -p chat_core --features test-hooks --test account_pair_recovery cancelled_and_restarted_import -- --nocapture`
`assertion left == right failed: restart must await explicit refresh; left:11; right:10`
`0 passed; 1 failed; ... finished in 1.10s`.
Focused GREEN: `1 passed; 0 failed; ... finished in 1.39s`; also final affected suite.

## I3 — contact ownership and complete encoding capacity

contacts::encode now allocates a Zeroizing<Vec<u8>> itself, reserving the complete sum of alias lengths plus maximum definite-length prefixes and fixed-field widths before copying any private contact material. It returns that zeroizing owner directly; schema no longer wraps an already-grown ordinary Vec.

Each Contact implements Zeroize and Drop, wiping peer, alias, digest, known installation and notice flag. Decode constructs this owner before fallible alias/remaining-field parsing, and the decoded Vec is preallocated to the bounded entry count. Both a partially constructed entry and previously parsed entries therefore own their zeroization when any later decode fails. The valid-contacts/invalid-PairState schema path drops the same guarded Contacts even though LocalAccount never finishes construction. Redundant B6 LocalAccount/Payload drop loops were removed because they protected only successfully built aggregate owners and are now unnecessary.

Structural validation (no unsafe freed-heap inspection or fabricated heap RED):
- `private_contact_owner_and_complete_encoding_reserve`: requires Contact drop ownership;1000 entries with64-byte aliases/all optional fields; encoded capacity equals complete precomputed reserve, length fits; exercises the same zeroize method that Drop invokes and validates fields; malformed second-entry decode errors after a valid first entry.
- `valid_contacts_then_invalid_pair_state_rejects_with_owned_contact_drop`: valid Active/contact snapshot round-trips; invalid final PairState version fails after contact decode; Contact is independently drop-owned.
- Existing canonical contact and invite behavior remains covered.
No full erasure of compiler copies/upstream Snow/native memory is claimed.

## I4 — reached-boundary and durable-artifact evidence

Added per-account debug+test-hooks reached-stop observation. A stop clears its armed marker, records the exact reached enum and closes the live flow. The first-chunk path sets its reached flag only at the actual successful chunk-send checkpoint; arbitrary earlier send errors cannot mark it reached. Tests use bounded completion polling of that exact marker. Approval results are asserted according to whether the stop runs synchronously in approval or asynchronously on receipt/import; the table no longer ignores errors or guesses with200ms sleep.

The revised table opens actual Store files during actor restart and decodes public evidence directly from the protected plaintext: exact tx/hash, all three E pin fields, N state tag/receipt, E transaction stage/receipt/publication hash, B2 Ready Add operation and exact signed packet hash. Successful actor reopen also validates the typed snapshot/receipt codecs. No secret values are printed.

| Exact reached stop | Specific assertions |
|---|---|
| EBeforeDisclosure | approval Err; gated0/ack0; E no tx; N Candidate; no extra publish |
| EAfterDisclosure | approval Err; gated0/ack0; exact durable E tx/hash/stage0/no receipt/Add; N Candidate |
| EAfterFirstChunk | approval Err; exact reached flag after positive actual chunk write; E durable stage0/no receipt/Add; any received N pin/hash must match |
| NBeforeImport | gated positive/ack0; N exact durable CandidatePinned tag2, all E keys/tx/hash, no receipt; E stage0/no receipt/Add |
| NAfterImport | gated positive/ack0; N PendingImport tag3 with durable receipt/hash/pins; E stage0/no receipt/Add |
| NAfterAck | positive actual receipt bytes; N durable PendingImport/receipt/pins; E may concurrently process that delivered receipt |
| EBeforeReceipt | positive ack; N durable import/receipt; E stage0 without receipt/Add |
| EAfterReceipt | positive ack; E stage1 with durable receipt and no publication/Add; N import/receipt |
| EBeforePublish | positive ack; E stage2/receipt/exact publication hash and actual Ready Add intent; backend still only initial publication |
| EAfterPublish | backend initial+Add publish; E stage3/receipt/publication hash, pending Add removed; N import or activated origin retains receipt |

Local successful first-chunk write does not imply remote receipt before immediate close: N may still be Candidate. An attempted stronger N-pin assertion at that row exposed this legitimate race and was removed. The NBeforeImport row proves recipient acceptance/pinning. Whenever N has a pin, all E PeerId/installation/transport fields, transaction and hash must match.

Every row then starts a fresh flow after real actor/store restart, rejects old approval and premature new approval, asserts gated0 before fresh confirmations/local approval, reconciles the original tx/payload where one exists, activates the same candidate, checks exactly two devices and exactly two distinct backend publication packets (initial plus single Add). Final N Store reopen verifies Active origin retains the same tx/hash/receipt. Ecommit-before-envelope uses the approved ExistingDisplays first-offer path.

Test-strength mutation evidence (not preimplementation behavioral RED): temporarily removed only the EBeforePublish checkpoint, ran the revised table, and restored the exact source in a finally block. It failed with:
`the exact configured production boundary must be reached: Elapsed(())`
`0 passed; 1 failed; ... finished in 30.62s`.
Retained output: `task-6-fix-1-checkpoint-red.log`.
With the checkpoint restored, table passed. After final test-only strengthening to compare all three E pins, the table alone was rerun: `1 passed; 0 failed; ... finished in 29.90s`.
This is operation-boundary/local-write/recipient-commit evidence, not per-byte OS process-kill coverage.

## I5 — cancelled New-role resume cannot return a QR

pair_prepare captures the account lifecycle request. New-role resume key installation runs through cancellation-aware service and post-await ensure_ready. pair_begin and pair_scan capture/revalidate their request after preparation before constructing/publishing a new session/QR. Transport's production Ok-after-stop contract was not changed.

The approved per-instance debug+test-hooks transition hold is inside actual Activate after stop await and before key assignment/ticket branching. It exposes entered/release oneshots; release, sender drop, or five-second timeout frees the worker. It adds no production behavior or global cross-test state. The regression waits for that exact window, requests suspend, releases it and requires Err/no display/no session, including after resume.

Executable RED before the lifecycle fix (with the same transport hook and unchanged Ok-after-stop behavior):
`cargo test --locked -p chat_core --features test-hooks --test account_pair_recovery resume_key_transition_cancellation -- --nocapture`
`cancelled resume must not return a QR`
`0 passed; 1 failed; ... finished in 0.85s`.
Focused GREEN: `1 passed; 0 failed; ... finished in 0.99s`; also final affected suite.

## Final verification provenance

No whole-core suite was repeated for this fix. The original251-pass full run remains historical at the original implementation, not evidence for this amended tree.

Final affected integration command:
`cargo test --locked -p chat_core --features test-hooks --test account_pair_recovery --test account_pair_channel --test account_invite --test account_auth`
- account_auth:20 passed,0 failed,1.27s.
- account_invite:8 passed,0 failed,2.41s.
- account_pair_channel:24 passed,0 failed,2.45s.
- account_pair_recovery:17 passed,0 failed,31.06s.
Output retained in `task-6-fix-1-affected.log`.
After this set, only crash-table assertions were strengthened to compare all three E pin fields. The affected crash table alone passed again in29.90s, as noted above; product source did not change after this set.

Final scoped units:
- `cargo test --locked -p chat_core --features test-hooks --lib account::manager` —4 passed,0 failed,0.86s.
- `cargo test --locked -p chat_core --features test-hooks --lib account::storage::schema` —3 passed,0 failed,0.02s.
- `cargo test --locked -p chat_core --features test-hooks --lib account::contacts` —2 passed,0 failed,0.01s.
- `cargo test --locked -p chat_core --features test-hooks --lib transport::manager` —19 passed,0 failed,0.18s.

Final strict checks, all exit0:
- `cargo clippy --locked -p chat_core --features test-hooks --all-targets -- -D warnings` — Finished dev5.70s.
- `cargo clippy --locked --workspace --all-targets -- -D warnings` — Finished dev4.02s.
- `cargo check --locked --workspace` — Finished dev3.26s.
- `cargo check --locked --release -p chat_core --features test-hooks` — Finished release2.35s.
- `git diff --check` — no whitespace errors; only Git LF/CRLF normalization notices.
Some concurrent build checks waited for Cargo package/build locks; all completed normally.

Release exclusion command:
`cargo check --locked --release -p chat_core --features test-hooks --example b6_fix_fixture_exclusion`
Temporary example referenced PairTestStop, AccountHandle::pair_test_reached and NetworkHandle::hold_next_key_transition_for_test. Actual expected failures: E0432 missing PairTestStop and E0599 for each new method. Compiler resolved this workspace's core/chat_core (HEAD dd14e57 plus owned fix working tree), and points to the debug_assertions gate. Retained output: `task-6-fix-1-release.log`. Probe removed in finally; normal Release check passed. No probe was committed.

## Scope, self-review and residuals

Exactly seven owned files changed: account/contacts.rs, account/manager.rs, account/pair/session.rs, account/pair/transfer.rs, account/storage/schema.rs, transport/manager.rs, tests/account_pair_recovery.rs. No schema wire/tag changes, dependency changes, generated files, UI, public production API expansion or transport production redesign. New observability/transition APIs are debug+test-hooks only.

Self-review confirmed the gate remains before payload builder, current post-final-await B5 Clock validation is intact, no ephemeral approval restoration, no PendingImport auto_refresh branch remains, and newly owned contact buffers are guarded before filling. Removing aggregate Drop loops leaves key field owners' existing Drop behavior intact. Original three dirty M2 doc hashes still match all three values above and remain unstaged. Original checkout/provider/transcripts untouched; no helpers, public DHT/relay, devices, push/merge/publish.

Remaining explicit limits: rare already-durable Active eligibility cost described under I1; existing partial-delete restart fail-closed limitation; upstream Snow internal memory-erasure limitation; no native OS/device or exhaustive power-loss validation. Controller re-review remains pending; no independent review claim.

Fix1 post-commit status: only the three original dirty M2 documents remain unstaged; post-commit SHA256 recheck matches all three original values. Commit contains exactly seven owned files.


### C1 — retained task-reported evidence

Source: `.superpowers/sdd/2026-10-18-m3-c-app-validation/task-1-report.md`. Historical evidence at the identified commits; not a C4 rerun.


Status: DONE_WITH_CONCERNS (C1 implementation complete; Android/Apple native execution remains explicit C4 coverage gap).
Commit: `adad1c12a2a8d0a7ece922036cd6304317fe8df7` — `feat(app): expose typed account commands and state`.
Worktree: `C:/Users/hyunw/orca/workspaces/ChatProject/m2-iroh-transport`
Base: `23846019be1dbbb8d003de3d1dd24a53e570b6a4`.
All evidence filenames below are relative to this report directory. No public DHT/relay or physical device was used. One source worker; no helper/reviewer agents.


- Added thin typed FRB account command API, public DTO conversion, shared-stream `AccountClient`, and ordered `AccountModel`. Existing ErrorCode mapping already included all A/B codes and was retained. No extra native provider, dependency, private-owner serde/Debug, wire format, storage format, or CORE reset.
- Actor owns process-scoped `public_revision` independently of durable state revision. It publishes coherent public AccountView after actor boundaries and BEFORE replying to snapshot. Event revision equals view revision. ContactView and PairView are sampled inside the same actor, never joined through independent asynchronous getters. Devices come from the accepted record; absent an accepted record, the real local certificate supplies only its local RecordDevice fields. No remote certificate fabrication.
- `EventBus` retains only latest public AccountChanged under its existing lock, replays Connection then AccountChanged on subscription. A snapshot that wins native FRB registration is covered by replay; a subscription that wins receives later publication. AccountModel subscribes before open/snapshot, validates event/view revision equality, and ignores duplicate/older revisions with full Dart BigInt/u64 precision. Connection revision ordering remains independent; no other event replay added. Core test covers registration-before-publication and publication-before-registration; actual native Flutter test subscribes AFTER captured snapshot and asserts replay >= captured revision.
- Explicit retry uses a fresh monotonic network request ticket and existing biased lifecycle mailbox once. No desired-foreground override or direct duplicate lifecycle call. Suspended/native desired-suspend, changed ticket, and cleanup provenance reject before loading. Stale queued command ticket rejected. Actual code-only lifecycle failure changes Missing -> Corrupt -> validated-ready are preserved in `lock_error`. Existing actor is reused; only absent actor may repeat open. No reset/overwrite/migration on Locked/Missing/Corrupt/WriterBusy/unsupported.
- Safe minimal AccountGate wraps existing ChatApp and owns model disposal. Locked route uses code-only Korean explanation/retry, not StartupErrorApp or account creation. Final setup/contact/device/pair screens are C2; Windows sensitive-preview is C3.
- Incoming recovery string, confirmation answer buffers, pair QR are guarded by Zeroizing before fallible handle/ID handoff. RecoveryDisplay/PairDisplay are one-shot command DTOs with no Debug/Clone/serde/event representation; ordinary events/cache contain no words/QR-secret/PSK/private seed. Dart and generated transit strings cannot promise optimizer-resistant RAM erasure: model minimizes references and lifetime, invalidates late secret futures, never logs/stores them. Public invite/safety QR are distinct public DTOs.
- PairTransaction now caches validated public `tx` metadata at the existing two constructors and decoder after existing `p.tx == tx`/other validation. Only public pending-pair projection reads it. Envelope, bytes serialization, canonical validation, transfer validation, other B6 searches unchanged. No new authority field or changed storage bytes.


#### Broad core results and investigated failures (retained, not hidden)

`cargo test --locked -p chat_core --features test-hooks --no-fail-fast -- --test-threads=1` (`c1-core-complete.log`) completed all targets: units90, auth20, endpoint11, invite8, keys8, pair_channel24, publication19, storage13, wire7, puppet7, transport17, doctests7 passed; disabled target0. Two targets failed: lifecycle9/10 and pair_recovery10/17. This command itself is NOT represented as clean.

Lifecycle restart expectation incorrectly compared process public revision across restart. Corrected direct API consumer asserts new process revision1 and exact equality of ALL durable public fields separately (no fields omitted).

Seven pair-recovery failures were new projection overhead. Four resumed approvals returned AuthFailed (`changed_payload_or_candidate_rejected`, `crash_boundaries_reload_durable_history`, `disclosure_before_first_frame_resumes_same_tx_from_existing_display`, `resume_same_pins_fresh_approval_only`); three unpublished-pair setup boundaries timed out (`activation_commit_clock_change_is_error_and_reload_never_claims_active`, `cancelled_and_restarted_import_has_no_automatic_lookup`, `resume_key_transition_cancellation_returns_no_display_or_session`). Isolated unchanged resume test failed in `c1-pair-resume-isolated.log`. Approved bounded diagnostic replaced ONLY new publish_view body with no-op: same isolated test passed1/1 (`c1-pair-no-projection-diagnostic.log`), then exact source restored SHA256 `1edc4ba7b95335e71b4ad64315ce603b579c67df9da25eabc1992d7143ea8365` (backup manager-diagnostic-backup.rs). Initial diagnostic CRLF temporary write produced bare-CR doc-comment compile errors; corrected byte restoration, no diagnostic source remains. No no-op ship/test relaxation.

Inspection: ordinary pending_pairs used envelope -> Payload::decode, reconstructing private keys and cryptographic validation each actor publication/100ms tick. Cached validated public tx avoids this repeated private decoding. With only approved cache correction, exact resume reproducer passed1/1 in3.43s (`c1-pair-cached-tx.log`). Final affected command `cargo test --locked -p chat_core --features test-hooks --test account_pair_recovery --test account_lifecycle -- --test-threads=1` => **lifecycle10/10, pair_recovery17/17** (`c1-pair-lifecycle-fixed.log`,0.58s/54.25s). Existing activation, lookup, payload, durable history assertions and all timeouts preserved. This A/B and affected-suite evidence supports the projection-overhead cause; no blanket claim about every earlier timing flake.

Earlier broad attempts retained separately: c1-core-full.log had auth expired_session_accepts_public_record_sync_without_mutation_or_grant timeout line846; unchanged isolated pass0.50s c1-auth-isolated.log. c1-core-serial.log then found contact assertion expected verified=false after confirmation; corrected full ContactView expected verified=true. c1-core-final.log had different_accounts_exchange_expired_public_records_durably_before_fresh_proof timeout line1320; unchanged isolated pass0.55s c1-auth-second-isolated.log. Final no-fail-fast auth20 passed. Neither early failure is relabeled as a clean run or conclusively diagnosed as contention.

Parent explicitly accepted broad per-target green evidence plus fixed affected suites without another whole-core rerun merely for one clean transcript; C4 final broader sweep remains.


#### Generation / strict / native boundary checks

`cargo clippy --locked -p chat_core -p chat_ffi --all-targets --features test-hooks -- -D warnings` => clean (`c1-clippy-clean.log`). Earlier c1-clippy.log found large AccountChanged enum inflation; fixed by Box. c1-clippy-final.log found items-after-test-module; moved only new conversions before existing tests, then clean. No lint suppressions.

All generated code came only from `& 'C:/Program Files/Git/bin/bash.exe' tools/gen_bindings.sh` (FRB2.11.1/freezed/UniFFI), final run c1-generation-shape.log and second c1-generation-second.log. SHA256 comparison **11 generated files, changed []** (`c1-generated-before.json`, `c1-generated-second-proof.txt`). Script logs existing unavailable-FVM informational note, skipped private WindowsProtector parsing and lifetime informational note, package-update notices. No generator failure. Kotlin chat_ffi.kt and all three NSE Generated files unchanged in Git; no account feature/key reference added to NSE.

Final required `cargo test --locked -p chat_ffi --features test-hooks` => **12 passed, 1 ignored child entry; separately spawned restart_child1 passed** (`c1-ffi-final.log`, compile18.52s). This includes real Windows DPAPI restart, round-trip, scope, invalid-blob and missing-key tests.

`cargo test --locked --release -p chat_ffi --features test-hooks release_storage_fixture_is_unavailable` => **1 passed** (`c1-release-fixture.log`, optimized compile46.78s); result NotAvailable and CORE.info remains None. `cargo expand --release -p chat_core --features test-hooks --lib account::publication` succeeded; expanded AccountServices is present and offline_for_test absent (`c1-release-publication-expanded.rs`, c1-release-expand.log, c1-release-factory-proof.txt). Both runtime stub rejection and actual core factory exclusion demonstrated with feature enabled.


#### Actual Windows Flutter/DPAPI integration

Stable FRB `test_storage_start` stub exists for codegen but actual fixture module and core offline factory compile only `all(feature=test-hooks, debug_assertions)`. It rejects after any existing core or previous fixture and uses existing CoreSlot.start then configure_local(None) BEFORE account open. It retains real platform provider with offline backend (RecordUnavailable) + fake clock anchored to system time; no production fake fallback. Native test process singleton never reset. Optional injected status wraps real provider, only StorageLocked/StorageMissing allowed. Read/write need explicit phase and owned absolute directory; no default-pass path.

Each command from app was a distinct executable process:
`flutter test integration_test/account_storage_test.dart -d windows --dart-define=ACCOUNT_STORAGE_PHASE=PHASE --dart-define=ACCOUNT_STORAGE_ROOT=C:/Users/hyunw/orca/workspaces/ChatProject/m2-iroh-transport/.superpowers/sdd/2026-10-18-m3-c-app-validation/native-store`

PHASE write/read/locked/missing all **1 passed** (`c1-windows-{phase}.log`, build86.9s/32.9s/28.1s/17.4s). Write real DPAPI created account and real key.wrap/account.bin. Read separate process restored same PeerId, installation and endpoint transport public key, and distinct CoreInfo.instanceId. Receipt: `native-store/bridge-receipt.json`, public identifiers + process instance ONLY; no words/QR/PSK. LocalReady persistence is proven, not publication/account-auth readiness. Both native write/read assert actual FRB late-registration public replay.

Locked/Missing are explicitly **INJECTED OS-status/UI paths** around real provider, not proof of actual user-locked Windows DPAPI or actual removed user key. They run production bootstrap and retry button, assert AccountGate remains Locked with exact typed code and Korean code-only text. No user OS lock/account change/system disruption. Existing separate native Rust missing-wrapped-blob test is distinct lower-level evidence, not substituted for Flutter UI proof.


#### Platform limits / source audit

Windows actual native Flutter execution above; Android SDK adb devices -l executed via absolute platform-tools path and returned no attached device (started adb daemon). Bare adb unavailable PATH; no emulator/phone verification executed in C1. Android source MainActivity.onCreate loads chat_ffi -> initializeNetworkContext -> initializeAccountKeyStore before super; bootstrap Rust init -> core.start -> debug hooks ready -> AccountModel subscribes/open/snapshot -> UI. Provider registration is existing real platform_keys::provider. Unsupported OS returns NotAvailable safe Locked route, no fake production protector.

Apple native/Simulator, first-unlock, entitlements/signing/lifecycle execution cannot run on this Windows host and remain explicit C4 open. No source inspection is claimed as native execution. Android native execution remains C4 open. No native adapter files changed.


#### Retained correction rounds

## Fix round 1 — I1 exact projection; M1/M2 focused regressions

Review base: `adad1c12a2a8d0a7ece922036cd6304317fe8df7`. Read full task-1-review.md and task-1-fix-1-brief.md. No helper agents or extra native/API/schema fixture. Fix commit recorded below.

**I1 causal correction:** Command::Snapshot previously published one sampled view then independently called view() again. Native readiness, time, and connection state can change between those reads outside actor serialization. Added a small private `snapshot_view()` used by the actual Snapshot command: expire pair, publish/cache coherent projection, clone precisely `last_public_view`. No resampling after publication. The next externally changed state is published under its next revision; ordinary command authority still rechecks readiness.

**Deterministic behavioral RED:** First reused the live actor and an EventSink which toggled existing native desired-suspend on publication. `cargo test --locked -p chat_core --features test-hooks snapshot_reuses_published_projection_when_native_readiness_changes --lib` failed in0.02s with same revision2 event Locked vs snapshot Unconfigured (`c1-fix1-red-boundary.log`). This was an exact payload mismatch, not timeout.

Controller requested strictly bounded boundary evidence. Final regression uses existing internal Manager fixture pattern, with no account actor/timers/watch processing. A one-shot EventSink atomically disarms then calls existing nonblocking `NetworkHandle.lifecycle_gate(true)` during publication, without event-bus/core-slot reentry. It calls the exact private snapshot_view path used by Command::Snapshot. Before cached-clone correction, the same command failed in0.00s: event revision1 Unconfigured versus snapshot revision1 Locked(StorageLocked) (`c1-fix1-red-oneshot.log`). That log was captured after helper extraction but before changing behavior. All data in this regression is public empty account state.

**GREEN:** Final test obtains three snapshots: first publication, changed readiness publication, and unchanged duplicate revision. Exactly two events are emitted, Unconfigured then Locked; every snapshot is structurally equal to the entire emitted payload for its revision. `cargo test --locked -p chat_core --features test-hooks --lib account::manager::tests:: -- --test-threads=1` => **7 passed,0 failed**, including the new deterministic regression and existing retry/secret/replay/storage-provenance units (`c1-fix1-green-manager.log`,1.21s tests).

**M1:** Replaced the manually injected public-only fixture test with generated `RustLib.initMock` plus real `FrbCoreClient.account` forwarding and AccountModel command handling. Test calls beginCreate and pairBegin, checks actual typed RecoveryDisplay/PairDisplay marker responses, confirms one shared underlying event subscription and two public AccountChanged events, checks generated response/model/event diagnostic representations and captured debugPrint exclude both distinct PUBLIC_TEST markers, then checks close clears references. Markers are deliberately invalid public test strings, never real generated secret material. This is explicitly Dart/generated-entry/mock evidence; it does not replace prior Rust serialization secrecy or actual native integration evidence. No generated files edited.

**M2:** Extended the shared-stream network-model regression with AccountChanged revision99 after a populated M2 revision7 snapshot (connected direct peer, connectMs123/directMs456) and an existing diagnostic line. It asserts exact same snapshot object, online state/timings, lines, captured diagnostics, and no error while account events remain available to their other subscriber. No M2 production code changed.

`cd app; flutter test test/account_model_test.dart test/account_bridge_test.dart test/network_model_test.dart` => **14 passed,0 failed** (`c1-fix1-flutter-amended.log`). An earlier edit invocation used app-prefixed paths while already in app and raised FileNotFoundError before any edit; its subsequent old tests passed in c1-fix1-flutter.log but that is NOT amended-test evidence. Corrected root edit and authoritative amended run above. No behavioral Dart changes after that pass; only formatter and removing a redundant import.

`cargo clippy --locked -p chat_core -p chat_ffi --all-targets --features test-hooks -- -D warnings` => clean (`c1-fix1-clippy.log`,4.92s).
`cd app; flutter analyze` initially identified one new unnecessary dart:typed_data import (foundation already exports it), retained in c1-fix1-analyze.log. Removed that import; final analyze => **No issues found!** (`c1-fix1-analyze-clean.log`,6.0s).

Per fix brief, no full core/Flutter, generation, FFI, release, or native rerun solely for a new clean transcript: only private snapshot projection and two Dart tests changed, so the previously retained generated/native/FFI/release evidence covers unchanged boundaries. M3 existing diagnostic tests/environment package notices remain documented; no suppression or unrelated cleanup. Focused test log contains package-update notices but no failing tests; strict output clean. Android/Apple native execution remains C4, not promoted by this fix.

Self-review: inspected the three-file diff, confirmed actual Snapshot command uses tested helper, cache is initialized by publication before clone, one-shot sink cannot create lifecycle/publication churn, no second sampling remains, generated DTO/model command responses exercised and real shared-stream count asserted. Original three dirty-document hashes match the recorded full SHA256 values unchanged. No new unresolved fix1 implementation concern.

Fix1 status: **DONE**. Commit `5450338a045859a192aaf30bb56bee1c17ee7b02` — `fix(account): return the exact published snapshot projection`. Exactly three owned files committed. Both diff checks passed. Post-commit status lists only the original three unstaged dirty M2 docs. No push/merge. Review scope: I1 fixed; M1/M2 regressions strengthened; M3 unchanged per controller ruling.


### C2 — retained task-reported evidence

Source: `.superpowers/sdd/2026-10-18-m3-c-app-validation/task-2-report.md`. Historical evidence at the identified commits; not a C4 rerun.


Status: DONE_WITH_CONCERNS — scoped C2 source/widget implementation complete; C3 native Windows privacy and QR/pair/device integration still required by the staged plan.
Worktree: `C:/Users/hyunw/orca/workspaces/ChatProject/m2-iroh-transport`.
Base: `5450338a045859a192aaf30bb56bee1c17ee7b02`.
One source implementer; no helper/reviewer agent. Controller owns independent review. No public network, physical device, push, merge, publish, native/provider/schema/generated-code change.


- Seven named screens implement welcome, one-time 24 numbered words, three actual one-based challenge positions, recovery warning/input/replacement consent, public invite generation/paste/parse, editable contact preview/explicit add, and full Rust safety display with separate explicit independent-comparison consent. Names use Unicode FSI/PDI isolation; fields are bounded separately. UTF-8 name length is checked at 64 bytes for display guidance; Rust remains final authority.
- Preserved approved slate/mint AppColors and Material typography through existing ChatApp theme. No mockup, rebranding, new package, font, QR renderer, camera or animation. Bounded scrollable onboarding; at widths >=900 contacts stay beside selected detail/safety; mobile displays one screen at a time with Back/cancel handling. Keyboard traversal uses reading order; semantic field labels use actual positions; default mnemonic semantics excluded. Read-aloud is an explicit SemanticsService action; no clipboard/log/storage use.
- Bootstrap passes the existing AccountGate-owned model into ChatApp and AccountRoutes. Gate remains the sole app-model disposer. Route exits only clear local/model secret references and cancel the exact flow; they never close the application model/listener. Diagnostic HomeScreen remains accessible from contacts, and its network/probe controls remain existing implementations. No contacts()/pairView() getter join or second native subscription.
- Create route tracks an operation ticket, exact flow ID, ephemeral RecoveryDisplay and challenge positions. Word references are dropped before confirmation; answers/controllers clear on submission/disposal; cancel, back, disposal, background and native-protection failure clear local/model references. A pending begin invalidated on route exit/background cancels only its exact late reply flow in Rust. Controller explicitly approved this narrow AccountModel correction: abandoned cleanup errors are not logged and cannot clear/set errors on a replacement route's display. No abandoned-cleanup success is claimed; public core projection remains authoritative.
- Model false/null errors and busy guard are respected; no failure is presented as account/contact/verification success. Publish-pending copy depends on coherent public phase. Recovery never offers active-account overwrite and warns that identity returns without contacts/history and that compromised devices knowing the phrase remain a risk. Recovery input clears on submission/background/disposal. Selected contact detail refreshes from the same coherent snapshot when verification/new-device status changes.
- Moved the one existing Korean error table to error_messages.dart and kept its compatibility export from AccountModel for existing consumers. No duplicate table, raw error detail, enum/key insertion, or native authority duplication.



#### Native sensitivity contract and lifecycle

SensitiveScreen withholds its child until native setSensitiveScreen(true) completes. It uses a shared reference count and serialized calls; removing one of overlapping sensitive routes cannot disable another. Native failure shows Korean failure guidance, withholds content and lets the account route drop/cancel words. Background first requests native concealment, then obscures/drops route inputs/words. The shared lifecycle observer remains until resume if the last sensitive route closes in background; setSensitiveScreen(false) is deferred until resume and zero references. No old words/input are resurrected; restarting is required. Pending begin before a SensitiveScreen exists is independently invalidated by AccountRoutes' lifecycle observer.

Windows Dart setSensitiveScreen now invokes the same `chat_app/account_storage` MethodChannel, method `setSensitiveScreen`, bool argument. This minimal direct-consumer edit was explicitly approved: absent/failed native handler must not be treated as success. C3 must implement the Windows channel/DWM preview hook. Android/iOS existing implementations were not changed or executed here. Source/widget tests do not prove native app-switch privacy or screenshots, and Dart string/reference disposal does not promise optimizer-resistant memory erasure.


#### GREEN / final verification

- `c2-focused-lifecycle-final.log`: required focused command => **15 passed**, before the pending-background test; `c2-focused-final.log` is authoritative final focused evidence => **23 passed,0 failed**. Earlier progress milestone `c2-flutter-full.log` => **83 passed**; final command below supersedes it. Earlier 16-test focused milestone was `c2-focused-final.log` before final amendments and was replaced by the final23 transcript; retained `c2-focused-expanded.log` shows22 before selected-projection correction. No16 result is misrepresented as final.
- Final required focused command above => **23 passed,0 failed**, `c2-focused-final.log`.
- Final `flutter test` => **90 passed,0 failed**, `c2-flutter-final.log` (5s test output). Entire Flutter suite includes original account/network/bootstrap tests and new public-fixture widget cases. Earlier full89 pass was followed by one real selected-contact behavior correction and regression; full90 rerun is final.
- Final `flutter analyze` => **No issues found!** (7.1s), `c2-analyze-final.log`. Initial analyze found10 new braces/import/non-null diagnostics in `c2-analyze.log`; corrected, no suppression. No production changes after final clean verification.
- `git diff --check` passed, with only the existing dirty protected-doc LF/CRLF notice. Staged diff check recorded at closeout below.
- No repeated C1 core/native/FFI/generation/release suite: those boundaries were untouched, as requested. No new native privacy claim.

Widget coverage: new account→24 public marker words→actual positions→pending, invalid words/storage error, confirmation cancel/exact flow, pending duplicate taps/disposal/late cancel, abandoned cleanup error preserving a newer display, background before/during display, entered recovery text discarded on resume, native API failure withheld child and canceled generated words, overlapping sensitivity references, recovery warnings/no restored contacts, active overwrite absent, UTF8 64/65 names, bidi isolation, duplicate alias projection, full safety text/no verification without explicit digest consent, current selected contact projection, keyboard focus and semantics position order, default mnemonic exclusion, 200% onboarding/wide contacts. These use a public invalid fixture client and are UI/state-consumer evidence, not cryptographic same-PeerId/recovery or native OS proof. No actual mnemonic/QR secret was logged or snapshotted.


#### Retained correction rounds

## Fix round 1 — I1 abandoned confirmation cleanup and I2 safety request ownership

Review base: `2f5edb91b1e5b547d567c5e1139758d3845cf25b`. Read task-2-fix-1-brief.md FIRST and complete task-2-review.md; applied only the combined I1/I2 scope. No helpers/reviewer, native/core/schema/bindings/package changes, public network/device work or unrelated cleanup. Controller approved the narrow cleanup helper and exact local/peer request binding before implementation.

**I1 root cause / correction:** Back/disposal previously dropped `_flow` and called the ordinary busy-guarded cancel while confirmCreate was still pending. InvalidRecovery later retained Rust creation, but the abandoned route no longer retained its cleanup obligation. AccountRoutes now captures the pending confirmation Future and exact flow when it abandons. It transfers those to `AccountModel.cancelAbandonedCreate(flow, afterConfirmation: future)`. This narrowly scoped helper copies the flow and captures the app generation, waits for that exact operation, skips cancellation if it succeeded, and otherwise directly submits only the captured exact Rust cancel while still in the same live model generation. It does not clear secrets, mutate current error state, notify as success, bypass busy for ordinary UI commands, or roll back committed accounts. Failure returnsfalse. Idle confirmation cancel still uses existing cancelCreate. The route's later cleanup-error display is ticket-guarded, preventing an abandoned continuation from setting an error in a replacement route. Disposal transfers the obligation without closing the app model.

**I2 root cause / correction:** Wide-layout contact selection changed `_contact` but not its operation ticket, while the safety reply and verification read mutable selection. Each selection now advances the ticket and clears old safety state. A safety request copies its local/peer bytes, and a displayed safety record carries that original pair with the exact Rust display. Reply acceptance requires the same ticket, local identity and selected peer. Confirmation rechecks both identities and submits that record's bound peer/digest. Local identity changes also invalidate an already displayed comparison and its consent. No safety reconstruction, relabeling, parser/crypto change or implicit verification was added.

### RED and public-fixture diagnosis

All commands below run from `app/`; logs are relative to this report directory.

Initial historical preimplementation RED:
`flutter test test/account_onboarding_test.dart test/contact_safety_test.dart test/account_errors_test.dart`
=> **25 passed /5 failed**, `c2-fix1-red.log`.
Expected failures: delayed invalid confirmation Back/disposal each expected exact cancel1, actual0; deferred cleanup/replacement case also cancel0; A's late safety result remained visible after selecting B; pending result remained visible after local identity changed. Successful abandoned confirmation Back/disposal tests already passed and continue to assert no cancel and authoritative PublishPending.

First source correction run `c2-fix1-green-first.log` =>29 passed/1 failed (Back case). Bounded diagnostics showed the cleanup helper received failed confirmation and current generation, then the existing fake native cancel's internal `expect` threw `FlutterError` under Flutter's navigation async guard. The helper correctly treated that as native failure, making the fake's counter misleading. Removed assertion from fake cancel; it now records public copied canceledFlows, with exact-ID assertions made from the test body. `c2-fix1-back-check.log` and `c2-fix1-back-diagnostic{,2,3}.log` retain that authoring diagnosis. Temporary diagnostic output contained only public operation flags/type/fixture match/length, no real secrets; all temporary production/test diagnostic prints were removed. `c2-fix1-green-amended.log` then passed30/30 before the additional displayed-consent identity test.

**Post-fix negative-control replay, not historical preimplementation RED:** with corrected fixtures plus the displayed-consent identity test, saved exact fixed source bytes for ONLY account_model.dart/account_routes.dart in ignored evidence files `c2-fix1-fixed-{account_model,account_routes}.dart`, temporarily wrote exact `git show 2f5edb9:...` baseline bytes for those two files, and ran the same three-file command. `c2-fix1-red-amended.log` => **25 passed /6 failed**. The six failures are the original five behaviors above plus an already displayed/consented comparison remaining actionable after local identity change. This independently confirms the corrected Back/disposal fixtures catch the original busy-cancel bug; it is clearly labeled replay rather than rewritten history.

Restored exact saved fixed source bytes immediately after the replay; no reset/checkout touched docs/tests. `c2-fix1-restore-proof.json` records exact equal SHA256 before/after restoration:
- account_model.dart `ea2b0ae03c1f54fe5eabed826db2208c1a1630aad2e8f6a87950f2dd088da522`
- account_routes.dart `cd09c0a2d70f2f9f99564da25a197b335564cbd0cb63527d0cffe95b371daabc`
Only test formatting followed restoration, then final validation.

### Final fix verification

- `flutter test test/account_onboarding_test.dart test/contact_safety_test.dart test/account_errors_test.dart` => **31 passed,0 failed**, `c2-fix1-flutter-final.log` (2s test output).
- `flutter analyze` => **No issues found!** (7.1s), `c2-fix1-analyze-final.log`.
- No full Flutter rerun solely for another transcript, per controller fix ruling. Earlier full90 evidence belongs to pre-fix C2 and is not presented as a post-fix full run. The affected model/route command and widget interleavings are covered by the amended focused suite; no outside-area failure or interaction justified broader checks. No C1/native/core/FFI/codegen repeats.
- `git diff --check` passed; original protected-document LF/CRLF notice only. Final staged check and commit recorded below.

New evidence covers exact deferred invalid-confirmation cleanup after both system Back and disposal; successful pending confirmation after both abandonment paths leaves PublishPending and no native cancel; cleanup failure after a new flow starts preserves that replacement display/error state and never targets its flow; A's delayed result cannot replace B detail or be confirmed for B; B's fresh result confirms exact bound B/digest; pending and already displayed comparisons are invalidated by local identity changes. Fixtures remain deliberately public invalid marker data. Assertions validate actual model/route behavior and the submitted public exact IDs/digest at the fake boundary; no native persistence/crypto proof is inferred.

### API handoff update / self-review

C3 model ownership/privacy/public QR seams in the original handoff remain unchanged. New narrowly scoped method `AccountModel.cancelAbandonedCreate(List<int> flow, {required Future<bool> afterConfirmation})` is exclusively for a route-owned create flow abandoned while its exact confirmation is pending. Keep the captured Future/flow until the obligation is handled; do not treat it as a general busy bypass or use it to undo a successful account. It preserves generation gating and does not clear/error a newer route. Normal new operations and idle cancel remain behind existing busy guards.

SafetyScreen still receives the exact public `SafetyDisplay`; AccountRoutes now owns its binding to copied local/peer IDs and the navigation ticket. C3 QR rendering must continue to use that bound display's `qr` and retain explicit comparison consent. Changing contact/local identity must discard the old binding/consent instead of reusing its digest with a new peer.

Self-review read the four-file diff, confirmed normal `_result`/busy behavior unchanged, exact failed-only cleanup independent of route disposal, ticket-guarded cleanup errors, successful confirmation skip, no deferred secret clearing, bound response/verification operands and unchanged Rust authority. No diagnostic prints remain. Only four owned source/test files changed. Original three dirty M2 docs match all previously recorded full SHA256 values and remain unstaged. No new known unresolved I1/I2 implementation issue; prior C3/C4 native/QR/platform limits remain explicit.

Fix1 closeout: **DONE**. Commit `33e81d6a360ab690976c65f1f5a103b04e962d29` — `fix(app): retain account flow and safety request ownership`. Exactly4 owned files committed; staged diff check passed. Post-commit status contains ONLY the original three unstaged dirty M2 docs. No source changes after final31-pass focused run and clean analyze. No push/merge/publish. Controller rereview is next.


### C3 — retained task-reported evidence

Source: `.superpowers/sdd/2026-10-18-m3-c-app-validation/task-3-report.md`. Historical evidence at the identified commits; not a C4 rerun.


Status: **DONE_WITH_CONCERNS** — approved implementation committed `8292367a09a18b72b6a6736f952e49c036d3f642`; unavailable/blocked platform gates explicitly remain below. Parent owns independent review. Worktree C:/Users/hyunw/orca/workspaces/ChatProject/m2-iroh-transport; accepted C2 base 33e81d6a360ab690976c65f1f5a103b04e962d29. No helpers/review agents spawned, no push/merge/publication. Three pre-existing dirty M2 documents excluded.


Same AccountGate-owned AccountModel/event stream drives PairScreen/DevicesScreen and existing invite/safety consumers. Existing theme and screen identities retained. Pair QR is command-reply-only route state; expiry, hide, protection failure and disposal drop it. Exact captured flow cancellation reaches core after an in-flight confirm/approve and for late begin/scan replies; no newer-route secrets/errors are mutated by that cleanup. Normal command busy rejection remains. Durable disclosure/remote-revoke warnings are read even with no current PairView; no cancellation or revocation claims erasure of identity-key copies. Separate both-device SAS confirmation and Existing local approval, receipt/publication/active state are actual core phases. Resume always begins fresh QR/Noise/SAS/confirmation/approval.

Controller-approved public projections: PairRole Existing/New; PairResumeView(transaction, role); AccountView.pairResumes separate from unchanged pendingPairTransactions; PairView.localRole plus nullable remoteInstallation/remoteKind. Active recipient origin remains New on resume; Existing pending tx remains Existing. Remote installation is taken only from the verified Channel. Remote New kind is verified encrypted candidate carrier; remote Existing kind is exact installation+transport matched in the validated record. Device type is an authenticated peer claim/certificate field, not OS attestation. No private Payload decoding, wire/storage format or signing/approval authority changes. WaitingQr and replacement flows have no remote metadata. Device identifiers use canonical lowercase no-pad base32 (52 characters for InstallationId); last confirmation is truthfully unavailable, issuance is not freshness.

pair_scan now returns its exact FlowId through core handle, FRB, client, model. Its ready check precedes session installation; dial failures clear the just-created session; failed scanner handshakes clear only captured exact flow. Existing handshake error closes connection, partially constructed Channel Drop closes/rekeys, and Session Drop cancels reader token. Reader is started only after verified handshake and readiness/resume checks; no unknown scanner flow remains to strand. QR owner retains retry budget. No global/peer reset.

Devices exposes actual device list, maximum five, current/last-device revoke disabled and exact target+irreversible-copy warning confirmation. Explicit refresh only, no PendingImport startup/lifecycle refresh. CandidatePinned welcome offers devices/resume/discard rather than create/recover overwrite. Locked gate exposes a narrowly scoped durable pending recovery action; same model and actual allowLocked discard/provenance. Failed discard remains failure/retryable; successful projection follows core.

QrInput single-delivers per widget/key; mobile scanner only Android/iOS, permission/error paste/reverse fallback, Windows paste/reverse. QrPayload sends the exact original String to QrImageView automatic version and full selectable text; overflow is explicit, never truncation. Existing public invitation and bound safety QR use the same original DTO payload.



#### Native privacy and public invitation intake

Android existing FLAG_SECURE and iOS scene background overlay remain native mechanisms. iOS direct screenshots are not claimed blocked. Shared SensitiveScreen withholds child until native success. Last foreground owner now retains protection until the exact identified replacement Flutter frame completes raster, with refs/ticket/foreground checks and new-owner invalidation; unknown/negative frame IDs fail closed with no timeout release. Background retains protection. This establishes Flutter raster completion, not a compositor/Peek guarantee. Widget-only frame-number source supplies metadata; production receives actual frameData/timings.

Windows owns a checked false baseline in its newly created window. DWMWA_FORCE_ICONIC_REPRESENTATION and DWMWA_HAS_ICONIC_BITMAP are **Set-only**; no OS readback is claimed. Initial attempt to query them failed with actual E_INVALIDARG (-2147024809), retained in c3-windows-privacy-diagnostic.log. Per controller correction, only successful setter status/values are tracked; baseline failure withholds secrets, unknown fixture values are null. Both attributes, invalidation, 32-bit solid bitmap creation/submission and restoration HRESULTs are checked; bitmaps are always caller-freed. Thumbnail requested bounds and live-preview client bounds are respected. Native handler failure hides the Flutter child immediately and emits protectionFailed. A failed restore cannot make subsequent activation report success; protection_ready tracks complete successful activation only. Saved restoration target is not replaced by partial failed activation.

Debug-only privacyTest reports public setter/handler status, injects both WM requests, counts GDI resources and forces initial/partial/bitmap/restore failures. Integration-only Dart calls it. No secret/approval getter or product bypass. Release exclusion proof below. Account fixtures retain existing feature=test-hooks AND debug_assertions gates; cargokit enables test-hooks only for Debug.

Public invitation minimum: Android VIEW scheme + onCreate/onNewIntent, iOS URLTypes + SceneDelegate cold/warm contexts, existing account_storage channel pending take/ack delivery; Dart InviteLinks accepts only chatproject:identity-invite/ prefix and queues until configured. Existing Rust parse→preview→explicit save is sole parser/authority, no autoaccept/connect. Pair URI is excluded from intake. Windows consumes public URI argv, stripping custom-scheme args before legacy bootstrap parsing. No new dependency, registry mutation or instance framework. Android/iOS source registration is distinct from OS launch runtime proof; Windows argv is distinct from OS protocol registration/warm forwarding.


#### TDD and focused evidence

All logs below are under this report directory. Failed compile/setup invocations are retained and are not represented as behavioral RED.

- c3-red.log: initial compiling Flutter pair/devices/QR request **0/4**, absent pair/device navigation, missing invitation QR and abandoned late pair cancellation. c3-ui-green.log initial four passed after implementation. Early test harness missing mock/teardown issues fixed without feature bypass.
- c3-core-red.log: Active New origin missing eligible resume, **0/1**; c3-core-green.log after public projection **pair_channel 24/24, pair_recovery 17/17**.
- c3-scan-red.log: mismatched-PSK initiator retained unknown public pair **0/1**; same existing regression now asserts scanner None and owner three-retry semantics unchanged.
- c3-remote-red.log: `cargo test --locked -p chat_core --features test-hooks --test account_pair_channel same_sas_both_local_confirmations -- --test-threads=1` **0/1**, verified installation absent. Stub public fields compiled before projection was implemented. c3-channel-final.log: entire affected channel target **24/24**, both directions SAS, verified installation/kind, WaitingQr absence, replay/replacement behavior. Additional forged-proof absence assertion included in final focused rerun below.
- c3-raster-red.log: old foreground disposal released native protection before later raster **0/1**; c3-focused-final2.log focused Flutter **45/45** after fence and expanded tests.
- c3-restore-red.log: native Windows reactivation after forced restore-invalidate failure incorrectly returned success **0/1**; c3-windows-privacy-final.log **1/1** after protection_ready correction. Further handler-failure assertions/final metadata proof run below.
- `flutter test test/pair_devices_test.dart test/qr_input_test.dart test/account_onboarding_test.dart`: c3-focused-final2.log **45/45**. Actual assertions cover both displayed directions/same SAS, no early completion/automatic Existing approval, explicit approval, Active N role, absent-current-flow pre/post disclosure warnings, expiry exact cancellation, duplicate camera delivery, denied camera paste fallback, Windows paste only, full QR/text equality at 200%, five cap, current and last disablement, exact revoke target, discard failure/explicit refresh, actual Locked button route and successful retry, pending approve/confirm disposal, late begin/scan, stale flow metadata exclusion, queued native/public argv preview before explicit save, pair URI rejection, negative/stale/new-owner raster guards. QR image bytes compared against same-input QrPainter; public invalid fixtures only.
- c3-ffi-projection.log: `cargo test --locked -p chat_ffi --features test-hooks public_pair_metadata_keeps_verified_binding_and_absence` **1/1** actual converter checks role, tx, public fields and null absence.


#### Package/build provenance and failed attempts

Exact qr_flutter 4.1.0 / mobile_scanner 7.1.3 pins in pubspec+lock. Official metadata https://pub.dev/api/packages/qr_flutter/versions/4.1.0 and https://pub.dev/api/packages/mobile_scanner/versions/7.1.3; archive SHA256 respectively 5095f0fc6e3f71d08adef8feccc8cea4f12eec18a2e31c2e8d82cb6019f4b097 and 023a71afb4d7cfb5529d0f2636aa8b43db66257905b9486d702085989769c5f2. Scanner archive retained c3-preflight. App minSdk26, AGP9.0.1/Kotlin2.3.20/Gradle9.1 unchanged; scanner iOS minimum12 compatible with app target, Apple compile still unexecuted.

Initial `flutter build apk --release` c3-apk-release.log failed after247.9s: generated Android plugin registrant referenced integration_test while release compile lacked it. Cause was my overlapping Flutter integration/test/build commands mutating shared plugin metadata. Controller directed serial queue, no generated hand-edits/dependency/OS fallback. Identical-pin serial rerun c3-apk-release-serial.log **PASS63.7s**, APK103.9MB, before final remote metadata addition. Final-source APK/Windows builds listed below supersede its artifact provenance.

Verbatim build warning: “WARNING: Your app uses the following plugins that apply Kotlin Gradle Plugin (KGP): mobile_scanner” and “Future versions of Flutter will fail to build if your app uses plugins that apply KGP.” Flutter recommends plugins migrate to Built-in Kotlin. This is a future compatibility risk; current pinned actual APK build passes. No silent upgrade/minimum-OS/product change.

Other retained development failures: missing test native mock stalled initial green (own process stopped), test fixture missing Directionality, QrImageView.data nonexistent property (replaced by real painter byte comparison), nullable-pattern compile error (fixed typed patterns), early paths/encoding failures. These are not behavioral RED or claimed successes. Analyzer intermediate syntax/lints fixed locally.


#### Final verification / owned commit

Final serial results and provenance are recorded below.

Final source additions and checks:
- Resume route scan gap found in self-review: pending recipients had only resume routes, which hid scanner. Controller approved scan/paste on resume with exact selected transaction guard. c3-resume-scan-red.log **0/2** actual absent action; c3-resume-scan-green.log **2/2** exact match permits local compare, wrong tx cancels returned exact flow and exposes no confirmation. Model obtains coherent snapshot after successful scan for selected resume, without parsing QR or touching newer route errors; missing/mismatched snapshot fails closed. Cancellation failure does not claim rollback/erasure.
- `flutter test` c3-flutter-full.log **124/124** (6s test execution). Final last-device fixture now independently checks a sole non-current device, distinct from current-device disablement.
- `flutter analyze` c3-analyze-clean.log **No issues found** (7.5s), after scoped eight curly-braces lint fixes in owned files only.
- `cargo clippy --locked -p chat_core -p chat_ffi --all-targets --features test-hooks -- -D warnings` c3-clippy-final.log **clean**,8.74s.
- `cargo test --locked -p chat_core --features test-hooks --test account_pair_channel -- --test-threads=1` c3-channel-verified.log **24/24**,10.67s; final forged-proof metadata absence + ready-before-session lifetime check included.
- `cargo test --locked -p chat_core --features test-hooks --test account_pair_recovery resume_same_pins_fresh_approval_only -- --test-threads=1` c3-resume-role-final.log **1/1**,3.47s; actual restarted E/New public resume role assertions.
- Owned Android privacy: `ANDROID_HOME=C:/Users/hyunw/AppData/Local/Android/Sdk; dart run tools/e2e/bin/devices.dart test --count 1 -- integration_test/account_privacy_test.dart` **exit0,1/1**. Framework captured output in build/e2e/devices/1792330717419761/24832.stdout.log, stderr same numeric filename; root c3-android-privacy.log is empty because framework owns output capture. Actual assembleDebug174.1s, installed2.616s, test2s. Debug x86_64+i686 Rust builds, native channel/FLAG_SECURE activation and sensitive-child rendering exercised; no OS-preview visual/assertion of flag readback. Owned chat_e2e_1/5554 process and reverse ports were cleaned and ownership.json removed. Existing three user AVDs unchanged. Windows-only subsequent metadata/handler tests do not alter Android path.

Controller final focused corrections:
- Canonical identifier correction: c3-canonical-red.log **0/2**, prior UI used hex contrary to core types.rs Display/spec3. Minimal shared public_id.dart matches lowercase no-pad base32 with known32-byte→52-character fixtures; c3-canonical-green.log **27/27** pair/devices/QR affected tests. No extra dependency/core authority.
- Exact raster correction: c3-exact-raster-red.log **0/1**, delayed intermediate frame11 released after postframe build while replacement was frame12. Capture replacement frameData in next complete postframe, require >removed and exact matching FrameTiming; c3-exact-raster-green.log **43/43** pairing/onboarding affected cases. Unknown/non-advancing replacement metadata stays protected, no timer fallback.
- c3-windows-native-verified.log failed an added test expectation that handler-failure SW_HIDE could then receive later raster and restore within3s. Withholding/failed-handler assertions passed; retained protection is correct when native hide prevents raster. Final test proves normal restoration first, then separately proves handler-failure concealment and retained state; no product bypass/retry success/fence weakening.
- Final `flutter test` c3-flutter-final.log **125/125**,4s; expected existing Home/network error-path stack traces remain in output, not a pristine-output claim. C1 deferred minor diagnostic noise remains separate review context. Final `flutter analyze` c3-analyze-verified.log **clean**,9s.
- Binding generation: `& 'C:/Program Files/Git/bin/bash.exe' tools/gen_bindings.sh` c3-generation-final.log then c3-generation-repro.log both successful. SHA256 of all11 generated files identical, c3-generation-repro-proof.json changed=[]. Native Git Bash only, no hand-edited generated output; informational FVM/unsupported-private-WindowsProtector parser notes retained.

Native final follow-up:
- c3-windows-native-final.log/final2.log: additional fixture failed by expecting a fresh sensitive widget after forced restoration SW_HIDE. Prior normal exact-raster restore and failure guards passed. Public-only fixture cases were reordered to avoid claiming in-process sensitive recovery from a hidden/inactive window; no product code/fence change. Final `flutter test integration_test/account_privacy_test.dart -d windows` c3-windows-native-final3.log **1/1 PASS**, build12.8s,test1s. Includes exact replacement FrameTiming restoration, checked baseline setters,20 requests each thumbnail/livepreview, GDI delta0, initial/partial/bitmap activation failure withholding, injected handler failure concealment, explicit public-only fixture reset between injected cases, and restoration failure followed by rejected activation. Explicit test reset is not application recovery evidence. No desktop input/focus/visual hold retried.
- c3-windows-key-locked.log failed setup because empty store never invoked key unprotection. Correct fixture precondition is a prior local protected write. A fresh owned c3-native-store was initialized via existing native write test before locked/missing phases; no product fix/reset. Actual successful phase logs below; these inject failure statuses around the native provider, not user OS lock or deletion.

Final serial native/storage/build results:
- `flutter test integration_test/account_storage_test.dart -d windows --dart-define=ACCOUNT_STORAGE_PHASE=PHASE --dart-define=ACCOUNT_STORAGE_ROOT=C:/Users/hyunw/orca/workspaces/ChatProject/m2-iroh-transport/.superpowers/sdd/2026-10-18-m3-c-app-validation/c3-native-store` with PHASE=write,locked,missing sequential separate native processes: **1/1 each PASS**, logs c3-windows-store-{write,locked,missing}.log, builds12.8s/13.2s/13.0s. Write uses real native DPAPI and retains only public receipt metadata in bridge-receipt.json; no words/keys/QR in logs/screenshots. Locked/missing actual Flutter bootstrap/retry UI stayed Locked with typed errors; statuses injected, no user system lock/key deletion.
- Final analyzer c3-analyze-artifact.log **clean**,6.3s, after integration fixture case-order adjustment.
- `flutter build apk --release` c3-apk-final.log **PASS89.8s**, app/build/app/outputs/flutter-apk/app-release.apk103.9MB, final corrected UI/fence/API source. aapt36.0.0 dump badging metadata c3-apk-metadata.txt confirms package dev.chatproject.chat_app unchanged, minSdk26,target36,label ChatProject,CAMERA,optional camera,arm64-v8a/armeabi-v7a/x86_64. Same KGP warning retained, no pinned-dependency/minOS change.

- `flutter build windows --release` c3-windows-release.log **PASS73.6s**, app/build/windows/x64/runner/Release/chat_app.exe. Actual PE FileDescription/ProductName=ChatProject, OriginalFilename=chat_app.exe. Run with the complete Release folder (Flutter DLLs/data), not the exe alone.
- Actual native fixture exclusion: c3-release-fixture-proof.json checks privacyTest/failRestore/failPartial/failBitmap present in Debug exe and absent in Release exe; production setSensitiveScreen/privacy_unavailable remain in Release. Source `#ifndef NDEBUG` fixture branch and integration-only caller corroborate binary proof. Core/FFI account fixture feature+debug double gates unchanged; C1 feature-enabled Release rejection proof remains baseline evidence, not claimed rerun here.
- SHA256 artifact manifest c3-release-artifacts.json (final source; no publication/upload):
  - `app/build/app/outputs/flutter-apk/app-release.apk`: 108971570 bytes, SHA256 `e44e177b6d39c436a4a0884ec3fac1be12febb53e0e2c53badf80ede83fbaf13`.
  - `app/build/windows/x64/runner/Release/chat_app.exe`: 123904 bytes, SHA256 `26a260b22ad0d78b9e616d7f6e1693afa4b38271348c6e45c7f1ea84e883a74a`.
  - `app/build/windows/x64/runner/Release/chat_ffi.dll`: 16081408 bytes, SHA256 `729b3bad53e45d41f729b81c0d325054dd5fa6daf8411db687073df03731b862`.
  - `app/build/windows/x64/runner/Release/data/app.so`: 5686160 bytes, SHA256 `579fdfc7fe3ec51aead7ddf975293cf3ae7b9d87ad3c65e6b9f817263bede163`.

Self-review completed against final source: flow/transaction/role binding, private reply lifetime, durable warnings, no disclosure rollback, no guessed freshness, native checked activation/rollback/restoration and retained failure, canonical identifier encoding, exact replacement-frame fence, public link parsing boundary, labels/permissions and fixture gates. Only own trailing EOF whitespace corrected; git diff --cached --check clean. All app/tool invocations after initial overlap diagnosis serial. No new OS/dependency fallback, private payload projection, registry changes, public DHT/relay/account traffic or user emulator/game interference.

- Final source Android rerun: same owned-framework command **exit0,1/1 PASS** after exact-frame amendment, build71.4s/install2.047s/test2s. Actual output build/e2e/devices/1792332125965271/39924.stdout.log (emulator output separate39640); root c3-android-privacy-final.log intentionally empty framework capture. Ownership record removed, adb devices empty afterward; no user AVD touched.
- Final provenance: all43 owned source files still byte-match c3-final-source-hashes.json after every final build/test; all4 release artifacts still match SHA256 manifest after Android Debug rerun. Staged paths exactly equal43-file owned manifest. Protected three M2 documents hash-identical and unstaged (c3-final-provenance-proof.json). git diff --cached --check clean. No additional implementation work remains within approved available-host scope; independent controller review follows.

Commit: `8292367a09a18b72b6a6736f952e49c036d3f642` — `feat(app): add device pairing QR and privacy controls`. Post-commit status contains only the original three unstaged M2 documents. No push/merge/upload/publish.

Remaining concerns for controller/C4: Windows real shell Peek/app-switch rendering blocked by concrete focus/occlusion evidence; Android API/emulator pass is not app-switch visual proof; Apple compile/Simulator/signing/physical unavailable; Windows OS protocol registration/warm forwarding and native platform URI launch execution remain separate packaging/runtime gates. Pinned mobile_scanner KGP future-compatibility warning retained. Native handler/restore failure deliberately stays concealed and may require window restart; no in-process recovery-success guarantee.



#### Retained correction rounds

## C3 review fix round 1/5

Base `8292367a09a18b72b6a6736f952e49c036d3f642`. Read full task-3-review.md and task-3-fix-1-brief.md; used receiving-code-review skill to verify the two Important findings against source before editing. Controller approved the concrete strategy before feature edits and subsequently approved the measured Devices pending-discard dialog correction. No helpers, model/core/FRB/native/dependency/generated changes or desktop interactions.

- **I1:** latch conservative disclosure risk before Existing approval or New matching-code consent begins. Keep that risk while an operation settles and listen to the model while the close modal is open. The no-known-disclosure message now describes a cancellation request, explicitly says cancellation does not erase delivered copies, and makes no pre-transfer promise. Durable/current public disclosure remains authoritative; this local risk latch grants no approval or completion.
- **I2:** `_abandon` returns the actual cancellation outcome. Failure keeps the route and a live-region error visible, clears the QR/display/confirmation state, retains only a copied public exact flow ID for retry, and blocks fresh pairing until cancellation succeeds. Concurrent retry/close uses one in-flight cancellation future. The retained retry and subsequent dispose do not call `clearSecrets` on a newer owner's pending operation. Neither retry nor late failure writes model errors or uses the replacement flow.
- **Measured layout correction:** at 360x640 logical pixels / 200% text, the original pair-close dialog overflowed 408px and pending-discard dialog 376px. Only these dialog contents gained SingleChildScrollView; existing revoke dialog already scrolls. Device ListTile needed no redesign: full canonical 52-character IDs are present, title/subtitle do not overlap, subtitle bounds remain inside each tile, and the action is reachable. Keyboard Tab traversal followed by Enter activates QR display, SAS match, close-dialog continue, device revoke and pending discard in the actual widgets. These are widget layout/keyboard proofs, not an OS screen-reader or physical-device visual certification.

All commands below ran serially from `C:/Users/hyunw/orca/workspaces/ChatProject/m2-iroh-transport/app`. Logs are beside this report. PowerShell redirected both streams to the named log, captured `$LASTEXITCODE` immediately and returned it after displaying the log tail.

| Exact command | Retained evidence and result |
| --- | --- |
| `flutter test test/pair_devices_test.dart --plain-name 'close warning'` | `c3-fix1-warning-red.log`, exit1, 0/3: pending Existing approval, pending New consent, and disclosure arriving while modal is open each lacked the required risk warning. |
| `flutter test test/pair_devices_test.dart --plain-name 'failed close'` | `c3-fix1-cancel-red.log`, exit1: fresh QR was incorrectly offered while cancellation was pending. To reach the reported outcome defect, moved that early assertion to the later stage; `c3-fix1-cancel-outcome-red.log`, exit1: PairScreen actually popped after failed cancellation. |
| `flutter test test/pair_devices_test.dart --plain-name '200 percent'` | `c3-fix1-layout-red.log`, exit1, 1/3: pair-close 408px and pending-discard 376px RenderFlex overflow; existing revoke route/dialog keyboard case passed. |
| `dart format lib/account/screens/pair_screen.dart lib/account/screens/device_warning_screen.dart lib/account/screens/devices_screen.dart test/pair_devices_test.dart` | Formatted only owned Dart paths. Later formatting after focused amendments used `dart format lib/account/screens/pair_screen.dart test/pair_devices_test.dart`. |
| `flutter test test/pair_devices_test.dart` | First correction run `c3-fix1-green.log`, exit1, 28 passed/1 failed: clearing the route error after successful cancellation inadvertently cleared the existing expiry notice. Moved error clearing to cancellation start; `c3-fix1-green-final.log`, exit0, 30/30. |
| `flutter test test/pair_devices_test.dart --plain-name 'failed close'` | Additional self-review regression `c3-fix1-replacement-red.log`, exit1: retry/dispose cleared the shared secret generation and discarded a newer owner's delayed begin reply. This was a real behavior failure, not a compile failure. Restricted secret invalidation to abandonment of this route's owned flow/pending operation. |
| `flutter test test/pair_devices_test.dart` | **Final** `c3-fix1-green-final2.log`, exit0, **30/30**, 2s test execution. Eight added tests cover both consent roles, live modal update, failed close with exact old-flow retry plus an untouched replacement projection and delayed replacement reply, approval completing after confirmed close, serialized retry, and the three 200% keyboard/layout cases. Existing expiry, late create/scan/confirm/approval cleanup, role/resume, cap/current/last/discard and raster cases remain green. |
| `flutter analyze` | Initial `c3-fix1-analyze.log`, exit0, no issues (30.0s). **Final** after replacement-isolation correction: `c3-fix1-analyze-final.log`, exit0, **No issues found** (6.1s). Dependency resolver retains the normal 28 newer-incompatible-packages notice; no pin changed. |

Self-review: inspected all four owned Dart diffs for local risk versus core authority, exact-flow ownership, cancellation failure visibility, single in-flight retry, no replacement secret-generation/error mutation, no retained secret/QR on retry, and current modal listening. Existing expiry regression caught and corrected a notice-clearing mistake before final pass. Model/core/native/API contracts are unchanged. Staging and protected-document hash proof recorded in `c3-fix1-provenance.json`; only the four approved Dart source/test files enter the source commit. The three original dirty M2 docs remain byte-identical and unstaged.

**Provenance and carry-forward:** original full125, strict/native/generator/Android/Windows Release results and manifests above describe **8292367 only**. They are not claimed as fix-commit coverage and their manifests are preserved. No broad/full/native/build/codegen repeat for this Dart fix; C4 owns the amended-source broad/build pass. M1 explicitly accepted scope exception: existing runner CMake already links dwmapi.lib and the Android manifest literal label requires no strings.xml resource; no no-op file/resource added. M2 pinned scanner future KGP warning and M3 inherited Home/network/parser diagnostic noise stay deferred. Actual Peek, Android OS preview, Apple runtime/build, physical camera and OS URI packaging/launch gates remain blocked/unexecuted as previously documented.

Fix1 source commit: `2b9b8849784dacc96b1b79c753f1af4cc1169549` — `fix(app): preserve pairing cancellation outcome and warnings`. Final staged whitespace check clean; commit contains exactly four approved Dart paths. Post-commit status contains only the three original unstaged M2 documents. **DONE_WITH_CONCERNS**, ready for controller's independent fix review; no push/merge/upload/publish.

## C3 review fix round 2/5

Base `2b9b8849784dacc96b1b79c753f1af4cc1169549`. Read full task-3-fix-1-review.md and task-3-fix-2-brief.md. Scoped review accepted I1/I2 and found I3: successful negative SAS removed route ownership before guarded abandonment, leaving AccountModel.pairDisplay cached. Controller approved the smallest ownership fix. Only pair_screen.dart and pair_devices_test.dart changed; no model/core/native/FRB/API/build/dependency change or helper.

The successful negative-SAS branch now calls `model.clearSecrets()` while the current route/ticket still owns the rejected ceremony, before `_flow = null`. Existing `_abandon` then removes visible state without a redundant core cancel. The retry/disposal ownership guard remains intact, so an abandoned old flow cannot invalidate a replacement owner's delayed reply.

Serial verification from the app directory:

- `flutter test test/pair_devices_test.dart --plain-name 'successful SAS rejection'` — `c3-fix2-sas-red.log`, exit1, **0/1**. Compiling behavioral RED: after successful rejection, expected null model cache but actual PairDisplay remained. Fixtures use only `PUBLIC_INVALID_PAIR_FIXTURE`, no generated secret.
- `dart format lib/account/screens/pair_screen.dart test/pair_devices_test.dart` — owned files formatted. Final repeat after moving the explanatory comment above its call changed 0 files; no behavior changed after verification.
- `flutter test test/pair_devices_test.dart` — `c3-fix2-green.log`, exit0, **31/31**, 2s test execution. New test verifies model cache and visible QR initially present, both gone after successful negative SAS, SAS no longer shown, and no cache/QR retention after an actual route pop/disposal. No redundant cancellation occurs. Existing exact old-flow retry, pending replacement reply preservation, pending approval, live warning and layout/keyboard tests remain green.
- `flutter analyze` — `c3-fix2-analyze.log`, exit0, **No issues found**, 5.9s. Normal 28 newer-incompatible-packages resolver notice retained; pins unchanged.

Self-review: the added clear is inside the existing mounted/current-ticket success guard and precedes dropping `_flow`; it runs only for successful negative SAS. The generic abandonment guard, retry handle and cancellation future are unchanged. New regression checks both UI and shared-model lifetime before and after disposal; replacement isolation remains covered. Protected three M2 hashes match the original values and remain unstaged. Prior full125/native/generation/Release artifacts continue to describe **8292367 only**; amended-source broad/build work belongs to C4. Existing external platform/visual/OS URI gates and deferred M2/M3 warnings are unchanged.

Fix2 source commit: `116ef3b83f24b967098056d73c2c06cc5143fcbf` — `fix(app): clear rejected pairing display before abandonment`. Staged whitespace check clean; exactly the two approved paths committed. Post-commit status contains only the three original unstaged M2 documents. **DONE_WITH_CONCERNS**, ready for scoped independent review; no push/merge/upload/publish.
