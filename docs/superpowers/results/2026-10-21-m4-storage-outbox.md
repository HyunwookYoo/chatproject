# M4 storage/outbox platform evidence

Task 13 local implementation and verification are complete on the isolated `m2-iroh-transport` worktree from accepted Task 12 base `134c0213d319de431c90fe9d4fe45d962ce118f5`, pending independent review. This document records local verification; it does not grant whole-M4 or external platform acceptance.

The detailed implementation report, exact command/cwd/exit records, raw output, source manifests, immutable artifact copies, and retained failures are under `.superpowers/sdd/2026-10-21-m4-storage-outbox/task-13-report.md` and `task-13-evidence/`. These local artifacts are intentionally preserved and ignored by Git. Toolchains are Rust 1.92.0 and Flutter 3.44.6. No generated bindings were edited manually.

The platform storage-root selectors now preserve a previously selected directory when only SQL/key/mode/preparation evidence survives. Windows and actual Android tests reproduced the old silent root fallback, then passed all 19 marker cases after the minimal selector fix. All three platform lists and tests match; the Apple test is prepared but unexecuted. Marker presence preserves a path only: Rust still decides whether the store is valid, restricted, or corrupt.

| Final-source gate | Observed result |
|---|---|
| `cargo +1.92.0 test --workspace --locked` | 170 passed, one intentional ignored entry; count includes the spawned FFI child. |
| Core with `test-hooks` | 414 passed, one ignored; original 33-cycle/8-second retained-history test executed. |
| FFI with `test-hooks` | 17 parent tests plus one spawned child passed; one ignored parent entry. |
| Strict Clippy: workspace, core hooks, FFI hooks | All three passed. Narrow lint amendments and initial diagnostic failures remain recorded. |
| Actual `tools/gen_bindings.sh`, twice | Both passed; generated-file hashes matched before and after both runs. |
| Flutter full tests with explicit current DLL and owned data root | 208 passed; one optional visual-fixture skip, zero environment-based native skips. |
| Focused actual native bridge/errors | 12 passed, zero skipped. |
| Dart tools | 233 passed, one Windows signal-test skip; fatal-info analyzer clean. |
| Flutter analyzer / Release smoke analyzer | Both clean. |
| CI unit checks | Full suite before the runner review fix: 124 passed, one Linux `/proc`-specific skip. The subsequent focused Release-runner suite passed all 15 tests. |
| Windows native message storage | Fresh write/read processes passed: 55 messages, 50+5 paging, typed change events, same account identity, distinct runtime instances. |
| Windows native account storage | Write/read/locked/missing/read passed on an exact canonical Korean root of 305 characters; actual DB path 316 characters. |
| Windows ordinary and contract Release | Both built, archived, scanned, and launched successfully. Contract reopened the same SQL binding in a fresh process and verified owned-file cleanup. |
| Android native worker/provider and restart | Seven full-class successes plus one intentional restart assumption; separate write/read each passed without skipping, with distinct PIDs. Provider reported software. |
| Android ordinary Release | Rebuilt final-source artifact passed marker scan and actual welcome/native launch. |
| Android contract Release | Fresh AOT artifact passed scan and actual create/reopen/cleanup-only launches, with the same SQL binding and distinct native instances. The initial parent-path failure and diagnostic artifact remain retained. |
| Seven native CLI rows | M4 Windows/Android/mixed and M3/M3-auth Windows/Android all passed. Each run ended with an empty ownership ledger; final process, emulator-port and adb observations were clear. |

The Release contract uses existing ordinary `openAccount` and snapshot APIs in the Unconfigured state. A successful open validates the encrypted database, metadata binding, SQLCipher version, and crypto provider. Networking stays stopped. It does not create an Active/Candidate account or insert a message. The public `storage.mode` binding survives process-close/reopen; wrapped key bytes never enter Dart. Windows cleanup follows process exit. Android uses an additional cleanup-only launch before Rust initialization. A success receipt is emitted only after the complete sequence.

Independent review found that Android's final stop request lacked an explicit PID-absence observation in the runner. The runner now retains activity ownership while polling within its existing bounds, and rejects a persistent or replacement PID. The focused tests reproduce the former false success and cover delayed disappearance and replacement preservation. This finding concerns the reusable completion gate; it does not establish a process leak in the earlier retained run.

The initial Android contract rejected a trusted cache spelling before Rust initialization. A separate diagnostic measured `/data/user/0/dev.chatproject.chat_app/cache` resolving to `/data/data/dev.chatproject.chat_app/cache`, with a real directory and a differing canonical spelling. Only the Android OS-provided parent is now canonicalized; Windows caller-root checks and nonce-child ownership checks remain strict. The diagnostic could not satisfy contract success, and final dedicated Windows/APK contracts were rebuilt and rerun after this change.

All three M4 runs observed the lost-ACK precondition, sender restart, actual payload retransmission, one deduplicated receiver row, and durable Delivered/Read progress. Each had four recovery-privacy checkpoints. A final scan of 1,130 retained current-run log/JSON files found no complete known body/alias fixture markers. This does not extend the point-in-time random-recovery-token audit into a late-buffer or partial-token guarantee; body hashes are not direct stored-wire hashes.

The final Windows native Debug DLL SHA256 is `90324ed1f38c6696eead0a80dd16fd858402898660b2ee9afb47313ce9ef13d2`. The ordinary and contract Windows Release native DLL SHA256 is `decc64c1aa5881d7960d9f8fcd40a7bc86b7f6ad323c802bd1c7576574268221`; their Dart AOT payloads and complete artifact trees are recorded separately. Native source manifests distinguish the earlier pre-lint probe from the final rebuild. Android records distinguish unstripped Rust/JNI/merged output from stripped APK members; those hashes are not interchangeable.

## Timing scope and unmet target

These are Debug automation measurements, not physical key-to-photon or Release performance results. The final write process measured one input-to-framework-frame sample of **55,342 µs**; 4 of 55 checked `sendText` durable-return samples exceeded 16 ms. Its three frame-build samples were 28,910 / 11,226 / 42,627 µs and raster samples 32,521 / 960 / 792 µs. The fresh read process measured **17,771 µs** for one input sample, two build samples 9,507 / 5,902 µs, and raster 856 / 574 µs; it performed no timed sends.

Input measurement starts after the keyboard and loaded UI have settled. Each send measurement includes bridge/queue/native commit/return overhead, not isolated SQL commit time. The first of 55 sends occurs after account opening; the next 54 use that instance. The data does not isolate a causal performance bottleneck. Earlier write measurements of 25,479 µs with 4/55 misses and 50,575 µs with 5/55 misses, and the earlier read sample of 15,015 µs, are retained separately. Both final input samples miss the 16 ms target; no threshold, durability setting, optimization profile, or timeout was changed to hide it.

## Boundaries and remaining acceptance

- Actual Apple SQL/provider runtime, native suspend/handle closure, and resume remain **OPEN** on this Windows host. Existing-ABI lifecycle and CommonCrypto/static-link checks are prepared; synthetic scanner tests or successful compilation cannot prove actual connection closure.
- Actual Flutter Profile artifacts/provider/runtime, selected physical Release devices, public DHT/relay, signing and upload remain **UNEXECUTED/OPEN**. No physical installation or production account was used.
- Current mixed M3 pairing and M3-auth scenarios are not claimed; the approved local matrix contains M4 Windows/Android/mixed plus M3 and M3-auth Windows/Android.
- Private design-canvas fidelity remains unverified. Existing UI tests and native probes cover the implemented behavior and theme.
- All accepted-task deferred findings remain individually indexed in `deferred-review-index.md` for whole-branch review. This task does not silently dismiss the Unix symlink fixture issue, lifecycle/interleaving coverage advice, raw generated numeric narrowing, or reusable-room read scheduling advice.
- The seven earlier Task 9 policy-denied Temp fixtures were not touched. The separate Task 12 `devices_cli` residual remains retained. Three Task 13 native Flutter roots were archived with matching file hashes and removed using approved literal, nonrecursive operations after process exit; the earlier unsigned-script refusal remains recorded. No blanket OS-key deletion or all-Temp-cleanup claim is made.
- A public synthetic malformed-owner marker appears once in `release-owner-red.log` (SHA256 `a518a67179a6b4e8d7c12c59c94f46a0cbeba743e42d6ffe8f4cec4994acd754`) as assertion-source text. It is classified separately from native privacy evidence; no universal zero-marker assertion is made.

The three pre-existing dirty M2 documents remain unchanged and unstaged. Independent Task 13 review and final whole-branch review follow the implementation report and scoped commit.
