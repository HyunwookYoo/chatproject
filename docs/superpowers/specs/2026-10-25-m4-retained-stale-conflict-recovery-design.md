# Manual recovery of a retained stale own-list conflict

**Status: proposal for user review; implementation and resend are not authorized yet.**

## Why this is needed

The approved stale-lookup prevention preserves already saved conflicts. Physical01 has such a conflict. Normal confirmation cannot accept the currently returned packet because it is below the durable observed floor. Restart, Refresh and repeated Apply cannot resolve unchanged evidence.

Safe relation-only diagnostics now show that accepted is current/unexpired and meets both observed/allocated floors; retained conflict is strictly older than accepted and has exactly the same generation and device list; no pending intents exist. The response itself is below observed. Original conflict timing and global latest state remain unknown. Current-device ownership and tombstone guards must still be checked privately by the operation.

The goal is a narrow, explicit resend of the already accepted **exact signed packet**, making it possible for a later real lookup and existing user-confirmed resolution to proceed. This does not dismiss a conflict, adopt cached data as a fresh lookup, allocate/sign a new version or grant authority.

## User experience

In Own Devices, offer **저장된 목록 다시 게시** only for the eligible retained-stale case. Keep the conflict warning. A review dialog displays the accepted generation, full device list and accepted packet hash, rather than silently using the old conflicting packet. The dialog explains: **저장된 기기 목록을 그대로 다시 게시합니다. 기존 충돌은 유지되며, 이후 공개 목록을 다시 확인하고 충돌 해결을 완료해야 합니다.**

Capture the displayed accepted hash at dialog creation. Confirmation authorizes one bounded publish attempt of that exact packet. Keep normal review/cancel/accessibility behavior and hide sensitive content on inactive lifecycle as existing screens do. A successful publish reports **목록을 다시 보냈어요. 공개 목록 확인이 필요해요.** The UI must not report authentication, observation or conflict resolution success.

After this action, normal Refresh may display the exact accepted packet from a real network lookup. Only then can the existing explicit review/Confirm path allocate and publish its normal Resolve version. No automatic Apply, authentication, user-message retry or extra ticket injection is added. If the lookup still returns an older packet, preserve the conflict and report the unresolved state.

## Authoritative operation guards

Recheck every guard inside the single account actor; UI enablement is only presentation. Conditions must hold at the current validation time:

1. Account is Active, storage/lifecycle request is ready and the submitted displayed hash equals the current accepted packet hash.
2. Accepted and retained conflict exist; publication is Conflict. Accepted expected-account signature/record validity and current/unexpired status are valid under existing rules.
3. Accepted sequence is at or above both durable observed and allocated floors. Never lower either floor. No pending packet intent or pending revoke-notification obligation exists.
4. Retained conflict is strictly below accepted sequence, with identical generation and complete device vector. Equal-sequence equivocation, higher evidence, changed membership/generation, unknown or missing evidence are ineligible.
5. Accepted generation equals local generation and contains this installation with its exact current transport key. The current installation/key is not tombstoned or removed. In addition, **no device in the complete accepted vector may match any local tombstone's installation or transport target**. A tombstone for an unrelated, already absent device is preserved and does not block eligibility; a listed tombstoned device does. Preserve all tombstones. Reuse existing validity checks without weakening ordinary local authorization's retained-conflict rejection.
6. Revalidate hash, lifecycle and time before the publish attempt and after its await. A time/lifecycle change cannot turn the result into authority. As with an interrupted network send, failure after sending does not prove the packet never left the device.

Do not serialize or log keys, recovery words, packet bytes, raw identifiers/hashes/sequences or arbitrary error details. No private-store diagnostic reads or plaintext exports.

## Implementation boundary

Add one explicit account command and ordinary Rust/FFI/Dart action carrying the displayed accepted hash. This is a local app interface addition, not a peer protocol or storage-schema change. The authoritative actor obtains the already stored public signed packet and invokes existing RecordBackend.publish once through the normal lifecycle-cancellable timeout.

Reuse exact packet bytes and sequence. Do not call record_packet, allocate, reconcile or signing for this resend. Do not clear/replace accepted, conflict, pending or tombstones; do not persist an automatic resend intent; do not install a grant or enable otherwise blocked work. A backend ACK is not Observed. Preserve NotMostRecent/RecordConflict, unavailable, timeout and lifecycle errors; do not fabricate lookup success or global absence. Normal periodic Refresh remains unchanged and may separately stage new evidence through existing strict policy.

Keep NetworkOnly, the DHT/relay configuration, message routing and shared observer/control rollback behavior unchanged. No store reset, new account, Android reinstall or force-close is needed for this operation. Previously approved public-network use remains in force; this proposal seeks approval for the new recovery operation, not a second network permission.

This is an explicit addition to existing retry policy: M3 permits same-packet/same-sequence retry, but current code only retries pending packets after reconciliation and refuses this retained-conflict state. It must not be represented as already implemented or covered by the prevention-only approval.

## Verification and limits

- Signed Manager/storage tests: an eligible explicit action sends byte-identical accepted data once; original encoded state/revision/floors/conflict/tombstones remain unchanged; no signing/allocation/notification/grant/message send occurs. Restart retains the conflict.
- Guard matrix: stale dialog hash, missing/expired/future accepted, higher floors, equal/higher conflict, membership/generation difference, current-device removal/key mismatch/tombstone, a listed non-current device targeted by an installation or transport tombstone, pending packets or notification work. An unrelated tombstone whose device/key is absent from accepted remains unchanged on an otherwise eligible resend. Every ineligible request has zero publish side effects and retains original state.
- Backend NotMostRecent/unavailable/timeout/cancellation and clock advancement remain errors without false observation or authentication. No implicit retry loop.
- A subsequent actual lookup of accepted followed by normal explicit Confirm can use the existing resolution path; an unchanged below-floor lookup still cannot. Unknown higher/membership evidence disables resend and retains strict conflict handling.
- Flutter dialog tests bind the displayed accepted hash, cancel safely, enforce eligibility/busy state and distinguish publish ACK from resolution. Regenerate local bridge code with pinned tooling; no dependency/schema/peer-wire changes.
- Cached Release core checks, scoped app tests/analyzer and ordinary Windows Release/fixture exclusion; then a separately observed Windows/phone trial. Do not claim actual recovery until the original pending message resumes once, with exact receipt/read state and no manual message retry or ticket injection.

Rejected shortcuts: automatic stale-conflict deletion; accepting a below-floor response; treating accepted cache as a fresh mutation base; generic publish-from-cache; weakening authenticated control; resetting or replacing the old trial store. A fresh isolated account can test prevention separately, but cannot prove recovery of this saved installation.
