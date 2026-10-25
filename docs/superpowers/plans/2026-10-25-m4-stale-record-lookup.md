# M4 Stale Account Lookup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development. Steps use checkbox syntax for tracking.

**Goal:** Prevent eligible stale same-membership account-DHT results from creating new conflicts while preserving fresh proof and publication authority rules.

**Architecture:** Classify at Manager's own publication and remote lookup consumer boundaries, never in the shared observer. Reuse existing cached-auth provenance for remote results and return unavailable before own publication side effects.

**Tech Stack:** Rust 1.92.0, Tokio, pkarr, existing encrypted account snapshots, Flutter Windows Release.

**Spec:** `docs/superpowers/specs/2026-10-25-m4-stale-record-lookup-design.md` (approved contract captured from chat).

## Global Constraints

- Only expected-account, current/unexpired, strictly older-than-accepted, identical-generation/full-device-list candidates qualify; accepted authority/floors must already be valid and conflict-free. Own additionally requires local authorization and no pending intents.
- Own returns RecordUnavailable before allocation/notification/publication/persistence; remote uses accepted cache provenance and still requires fresh proof. Never fabricate a fresh Found or CompletedAbsent.
- Shared observer/control, explicit review, floors/tombstones, pending intents, API/FFI/wire/schema/dependencies and NetworkOnly policy remain unchanged. No CoreError.detail matching.
- Existing retained conflicts are not cleared; no claim that the preserved physical account or M4 overall is fixed.
- Work only in the existing linked worktree. Never edit/stage the three dirty legacy M2 roadmap/carry-forward/results documents. No primary/trial app runtime actions, phone actions, global settings, push/merge or recursive cleanup.
- Reuse cached Release builds and existing portable Perl; no Debug build or dependency/toolchain installation. Keep all receipts under this plan workspace. Existing physical receipts are read-only evidence.

## Review Focus

- A packet below observed but not below accepted must take the strict path: Task 1 matrix.
- An old packet over authenticated control must still restrict/close: Task 1 strict observer coverage; retain existing debug-gated integration test unchanged.
- Own pending/privileged operations must not publish from cached freshness: Task 1 side-effect counters/state checks.
- Clock advances during async work must use current validation time: Task 1 Manager boundary time cases.
- Accepted authority after unavailable must not be inferred from a UI enum alone: Task 1 authorization/state persistence checks.

### Task 1: Implement and test bounded lookup handling

**Files:** Modify `core/chat_core/src/account/manager.rs`; add `core/chat_core/src/account/manager/stale_lookup_tests.rs` if needed for private Release-executable tests. A tiny shared predicate may live in `publication.rs` only if it removes real duplication; do not alter reconcile/observe policy. Existing debug-only `tests/account_auth.rs` is read-only.

**Interfaces:** Consume `Manager::publication`, `Manager::finish_remote_record`, `Manager::local_authorized_at`, SignedRecord, RemoteRecord and existing private snapshot/storage facilities. Preserve all public signatures. Produce only private eligibility logic and lookup-only branches; no new public fixture entry points.

- [ ] Add signed Manager-level regressions named with `stale_lookup`: own returns RecordUnavailable with unchanged encoded state/authorization/floors and zero publish/notify calls; remote returns accepted with cached provenance, unchanged floor and no installed grant merely from lookup. Include durable restart/roundtrip and an authorized fixture tombstone for an unrelated device.
- [ ] Add strict rejection/non-fallback cases from Spec Verification, including own pending publication, current-device tombstone, wrong account/signature, accepted expiry/future/higher floor, equal-seq equivocation, and candidate greater than accepted but below observed. Check meaningful original state/error outcomes; do not assert only a predicate's return.
- [ ] Run cached `cargo +1.92.0 test --locked --release -p chat_core --lib stale_lookup` before production edits; capture behavioral RED rather than compilation failure.
- [ ] Implement the smallest private classification at the two Manager boundaries, validating against current clock after await. Eligible own lookup returns unavailable before persistence; eligible remote lookup uses existing cache checks/event/proof route. Preserve strict paths and the exact accepted case.
- [ ] Run focused GREEN, then `cargo +1.92.0 test --locked --release -p chat_core --lib` once. Run scoped applicable Release check/lint; do not recreate Debug fixtures or weaken cfg fences. Record excluded integration coverage honestly.
- [ ] Self-review exact diff/protected hashes, commit only Task 1 files, and write full report with RED/GREEN commands, exits, logs and limits. Controller dispatches independent spec/quality review.

### Task 2: Verify ordinary Release artifact and record limits

**Files:** Produce a new archived Windows Release and provenance receipts under this plan workspace; update `docs/superpowers/results/2026-10-25-m4-public-network.md` only with verified prevention behavior and explicit no-runtime-recovery limit.

**Interfaces:** Consume accepted Task 1 commit/source hashes and existing `tools/ci/check_release_hooks.py`; produce archived bundle/hash manifest/source stability/marker scan and concise results.

- [ ] Build one ordinary `flutter build windows --release --target lib/main.dart` using existing Rust1.92/portable Perl child environment, capturing full command/exit/log, source hashes and free disk before/after. Do not run/replace the live app.
- [ ] Run `tools/ci/check_release_hooks.py` on the new archive and confirm new test-specific markers are absent; compare with diagnostic3 only as provenance, not a runtime test.
- [ ] Update factual results, verify protected documents/source stability, commit exact results file and report artifacts. Controller performs task review and final whole-change review over the prevention baseline.
