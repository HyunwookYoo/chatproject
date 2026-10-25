# Manual Retained Stale Conflict Recovery Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox syntax for tracking.

**Goal:** Add explicit, guarded, byte-identical resend of an accepted signed own record so later real lookup and normal confirmation can recover a retained older conflict.

**Architecture:** The account actor computes presentation eligibility and enforces the full guard set on one explicit command. The local bridge and Own Devices dialog carry the exact displayed accepted hash. Resend never changes account state or authority; existing lookup/Confirm completes resolution separately.

**Tech Stack:** Rust 1.92.0, existing Tokio/pkarr/storage, flutter_rust_bridge 2.11.1, Flutter/Dart.

**Spec:** `docs/superpowers/specs/2026-10-25-m4-retained-stale-conflict-recovery-design.md` (user approved; all guards and exact copy are binding).

## Global Constraints

- Exact accepted packet bytes and sequence, one backend publish per explicit confirmed request; no signing, allocation, reconciliation, state persistence, conflict clearing, grant installation, message retry or automatic resend intent.
- Require Active/ready lifecycle, matching displayed accepted hash, Conflict state, accepted+retained presence, strict retained<accepted with identical generation/full devices, accepted current/unexpired/local generation and at/above observed+allocated floors, no pending publication/revoke obligation, exact current installation+transport ownership, and no accepted device matching any tombstone installation OR transport. Recheck after await. UI eligibility is not authorization.
- ACK is not Observed/authenticated/resolved. Preserve backend NotMostRecent/RecordConflict, unavailable/timeout/lifecycle errors. Real lookup>=floor and existing explicit Confirm remain required. Do not change shared observer/control, NetworkOnly/relay/routing policy, peer wire, schema or dependencies. No CoreError.detail matching or raw/private diagnostic output.
- Same existing linked worktree; three dirty legacy M2 docs remain untouched/unstaged. No primary/trial process, device, UI, store or network runtime actions during implementation. No push/merge/cleanup.
- Reuse cached Release and existing tools/portable Perl; no Debug-cache recreation, dependency/toolchain installation or global environment/profile changes. Keep new receipts in this plan's ignored workspace. Old physical receipts are read-only context; do not overwrite them.

## Review Focus

- Listed non-current device revoked by installation OR transport: Task 1 zero-publish guard tests; unrelated absent tombstones remain preserved.
- Dialog hash becomes stale while open: Task 2 immutable captured-hash test and Task 1 authoritative stale-hash rejection.
- Cancellation/time advance after publish begins: Task 1 original-state/error/no-authority assertions; no claim of unsent data after cancellation.
- Successful ACK followed by the same old lookup: Task 1 still-blocked Confirm test and Task 2 conflict warning retained.
- Missing/default bridge eligibility must be false: Task 2 typed conversion/fixture tests; no secret-bearing field added.

### Task 1: Guarded actor command and Release tests

**Files:** Modify `core/chat_core/src/account/manager.rs`, `core/chat_core/src/account/publication.rs` (public view/default only); add `core/chat_core/src/account/manager/retained_republish_tests.rs`. Reuse the existing private Manager/storage harness in `manager/stale_lookup_tests.rs` through the smallest cfg(test)-only visibility/helper change if needed; do not duplicate a large harness or refactor production policy.

**Interfaces:** Add `AccountHandle::republish_accepted_record(accepted_hash: [u8; 32]) -> Result<(), CoreError>` as async, matching actor command/rejection routing. Add `PublicationView::can_republish_accepted: bool`, default false; Manager sets true only for the complete current eligibility predicate. The bool is public presentation data, not stored state. Preserve existing public methods and encoded schema.

- [ ] Write actual Manager/signed-storage regressions with a `retained_republish` name filter. Eligible request publishes exactly the already accepted bytes once, preserves complete memory/durable state/revision/floors/conflict/tombstones, sends no notification/grant/message, and leaves conflict present after reopen. Include an unrelated absent tombstone positive case.
- [ ] Add meaningful guard/backend/lifecycle matrix from Spec Verification, including wrong displayed hash, current installation/key mismatch, both non-current tombstone forms, pending notify packet, unavailable/NotMostRecent/timeout and clock/lifecycle changes. Ineligible requests publish zero times. ACK alone does not resolve; unchanged stale lookup still fails normal Confirm, while later real accepted lookup plus explicit Confirm uses the existing path.
- [ ] Run cached `cargo +1.92.0 test --locked --release -p chat_core --lib retained_republish` before production command behavior; capture behavioral RED rather than compile/setup errors.
- [ ] Implement the private actor eligibility helper using existing `RecordState::can_publish` for whole-vector ownership/removal checks; command clones only the public stored signed packet and calls the existing cancellable service once. Revalidate current hash/lifecycle/time after await; never weaken normal local_authorized_at. Map stale hash to RecordConflict and other guard failures to AccountRestricted; retain service/backend error codes.
- [ ] Run focused GREEN, then one full cached Release core unit suite plus strict Release core library Clippy/check. Preserve the existing debug-only socket/control integration fence and report exclusions.
- [ ] Self-review, commit exact Task 1 files, and report RED/GREEN commands/exits/logs/source/protected hashes and concerns. Controller independently reviews before Task 2.

### Task 2: Typed local bridge, consent UI and app tests

**Files:** Modify `app/rust/src/api/account.rs`, regenerated `app/rust/src/frb_generated.rs` and `app/lib/src/rust/` files; modify `app/lib/account/account_client.dart`, `account_model.dart`, `screens/devices_screen.dart`; modify/add scoped `app/test/account_bridge_test.dart`, `account_model_test.dart`, `pair_devices_test.dart` and necessary fake-client/view fixture sites only.

**Interfaces:** Export `republish_accepted_record(accepted_hash: Vec<u8>) -> Result<(), CoreError>` asynchronously with existing fixed-array validation; carry `PublicationView.can_republish_accepted` to Dart `canRepublishAccepted`. Add `AccountClient.republishAcceptedRecord(List<int> acceptedHash)` and `AccountModel.republishAcceptedRecord(...) -> Future<bool>` through existing command/busy/lifecycle handling. Preserve all sensitive-flow and event subscription behavior.

- [ ] Add model/bridge/widget RED tests for explicit once-only call, malformed hash, false/missing eligibility, cancel/busy state, captured accepted hash despite snapshot replacement, and ACK retaining Conflict and the Spec success copy. Show accepted generation/hash/full devices in the dialog; never pass conflicting hash as accepted consent.
- [ ] Add the typed API/conversion and regenerate with installed pinned FRB 2.11.1. Keep Rust expansion on cached Release; if the installed codegen path would recreate Debug, stop and ask controller for tooling context, not the user. Do not hand-edit generated files or upgrade dependencies.
- [ ] Implement only eligible Own Devices entry **저장된 목록 다시 게시**, review dialog and explicit action using the exact explanation/success strings from Spec. Capture displayed hash, check mounted/lifecycle as existing flows do; do not automatically Refresh/Apply/authenticate/retry messages.
- [ ] Run scoped app/bridge tests GREEN, then one full Flutter suite and analyzer. Run applicable ordinary Release FFI check/Clippy; no Debug fixtures or forced integration setup. Document skips and limitations.
- [ ] Self-review and commit exact local bridge/generated/app/test files; report test/codegen command evidence, source/protected hashes and concerns. Controller independently reviews before Task 3.

### Task 3: Ordinary Windows Release and factual handoff

**Files:** New Release archive and exact receipts/logs under this plan workspace; factual update only to `docs/superpowers/results/2026-10-25-m4-public-network.md`.

**Interfaces:** Consume accepted Task 1/2 hashes and existing hooks checker. Produce a new ordinary `lib/main.dart` Windows Release, manifest/source stability and fixture exclusion evidence. Do not launch or replace the live trial app.

- [ ] Build once using existing Rust1.92/portable Perl child environment. Archive separately, assert no overwrite, capture exact command/exit/duration/source hashes/free disk, and compare with physical01 context bundle as provenance only.
- [ ] Run Release hooks checker plus exact new fixture-marker UTF-8/UTF-16 exclusion. Verify full archive manifest and protected/source stability; no runtime actions or cleanup.
- [ ] Record only actual command/tests/review/build outcomes and explicit no-physical-recovery limit in results, commit exact report, and hand artifacts to controller for task review and final whole-change review. The later physical trial still requires normal explicit user resend confirmation and real lookup/Confirm; it is not executed by this build task.
