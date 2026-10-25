# M4 stale account lookup prevention

User approved this bounded prevention and immediate subagent execution on 2026-10-25. This supplements M3 account design sections 5.1/5.2; it does not relax authenticated control or resolve saved conflicts.

## Evidence and goal

The physical restart trial encountered an existing own-publication conflict. A signed lookup had the accepted membership and local generation, was current/unexpired, and was below observed/allocated high-water. Its relationship to accepted sequence and the original conflict trigger remain unknown. This change prevents one reproducible failure class; it is not a proven recovery of that installation.

A strictly older account-DHT response with unchanged membership must not create a new conflict when an already accepted, valid authority is available. Classify that response as unavailable for freshness and keep normal fresh-proof authentication against accepted authority.

## Eligibility

At the account lookup consumer boundary only, all must hold:

- The candidate is verified for the expected account, current and unexpired.
- Candidate sequence is strictly less than accepted sequence; generation and complete device list equal accepted.
- Accepted is current/unexpired, meets existing authority/floor checks, and no retained conflict or Conflict state exists.
- Own publication additionally requires existing local authorization and no pending publication intents.

Below observed/allocated alone is insufficient. Exact accepted packets retain their existing path. Equal-sequence equivocation, higher conflicting evidence, membership/generation change, invalid/future/expired evidence, accepted below floor, retained conflicts and own pending intents retain existing behavior. Tombstones and current installation checks remain binding.

## Behavior

Own publication returns RecordUnavailable without allocating, notifying, publishing, accepting a packet, lowering either high-water, holding pending work or mutating the accepted/conflict/tombstone state. Preserve existing valid ordinary local authority; preferably return before state persistence for this classified response. No add/revoke/recovery/confirm operation may use accepted cache as a fresh base.

Remote finish_remote_record treats an eligible DHT candidate as unavailable and uses its existing accepted-cache/provenance route. A new proof is still required to authenticate. Do not call the shared observer with that eligible lookup candidate; do not change the observer itself, control evidence, explicit review, proof validity or grants.

Do not fabricate Found(accepted), CompletedAbsent, global absence or global freshness. Do not alter NetworkOnly/DHT/relay policies, API/FFI/wire/schema/dependencies or error-detail matching. Existing saved conflicts are preserved and may still require normal manual resolution.

## Verification

- Manager-boundary signed-fixture RED/GREEN: own unavailable, complete authority/state/high-water persistence across restart, zero allocation/notification/publication; remote accepted-cache provenance with unchanged floor and subsequent fresh proof requirement.
- Strict-path matrix: equal sequence/different payload, generation/membership differences, future/expired candidate or accepted, invalid signatures/expected account, higher observed floor, retained conflicts, own pending intents, and a candidate above accepted but below observed.
- Existing exact/ready-pending publication and unavailable/absent distinctions remain intact. Tombstones are retained and current-device tombstones prevent fallback.
- Shared observer/control rollback protection stays strict for the same old packet. Preserve the existing debug-gated integration test; add meaningful Release-executable coverage without widening fixture APIs.
- Cached Release core units, scoped lint/check and one ordinary Windows Release build plus fixture-marker exclusion scan. No Android build, device actions, Debug-cache recreation or claim of real automatic recovery.
