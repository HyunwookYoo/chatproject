# M3 controller ruling appendix

Verbatim ruling-bearing paragraphs, preserving order within the A, B and C controller ledgers. Ledger groups follow the approved A→B→C sequence; overlapping B1 work is identified in the main result chronology. Rationale and cost-if-wrong language are retained exactly. Original ledgers remain preserved; this tracked copy prevents loss on later authorized scratch cleanup. Paragraph numbering is distinct from the number of individual `Ruling:` occurrences.

## A ledger

Source: `.superpowers/sdd/2026-10-18-m3-a-keys-storage/progress.md`. 13 ruling-bearing paragraphs, 17 individual `Ruling:` occurrences. Source SHA256 `a938da24720a5470331971123f065bcc683a1afbbc7c265b8655152211a04cfe`.

### A-01

Ruling: continue in the verified existing linked worktree, keeping M3 commits distinct and leaving M2 document edits untouched — using-git-worktrees forbids nested creation in existing isolation; the approved plan makes extra checkout conditional — cost if wrong: M3 commits may later need cherry-pick to a dedicated branch.
Ruling: normalize task headings to Task N: A/B/CN for the skill extractor without changing task content — parser requires numeric Task headings — cost if wrong: documentation-only relabeling.
Ruling: only the current serial task implementer owns required common files during its turn — no parallel implementers, as mandated by SDD — cost if wrong: less concurrency, no change to product scope.

### A-02

Ruling: A4 adds the minimal canonical Candidate payload [1,0,device_seed32] to the same typed AccountState codec A5 later extends — A4 requires persisted candidate keys while A2 intentionally supplies only opaque plaintext; cost if wrong: a small internal schema extension/rework, no temporary format or migration framework.
Ruling: A4 verifies actual candidate restart/diagnostic transition; A5 must add the active-account restart half of the shared acceptance test once real account lifecycle exists — otherwise A4 would need to fabricate future active states; cost if wrong: active-state evidence remains a tracked A5 obligation until provided.

### A-03

Ruling: A5 may minimally extend account/keys.rs with a crate-private consuming constructor from already-derived IdentityKey signing Secret32 that recomputes PeerId — A1's only public constructor derives from RecoverySeed entropy, whereas approved persistence stores signing seed, never recovery entropy/words — cost if wrong: private constructor misuse must be caught by restart identity equality and B6 expected-PeerId validation; no second HKDF or public secret API.

### A-04

Ruling: Store::discard_pending stays crate-private physical cleanup; B6 must prove never-active state before calling it — A2 deliberately cannot decode future typed account payload, while approved spec prohibits active deletion — cost if wrong: later coordinator checks must be reviewed as the security gate.

### A-05

Task 1 Ruling: MAX_RECOVERY_INPUT_BYTES=4096 UTF-8 bytes before normalization, InvalidRecovery without echo — design requires pre-parse length bound but omitted the value; allows ordinary24 words and bounded pasted whitespace — cost if wrong: unusually whitespace-heavy pasted phrases need trimming. Secret32 borrowing access allowed to trusted Rust/native implementations, never FFI/Debug/Serialize/Clone; secret field getters crate-private unless actual cross-crate need.

### A-06

Task 2 Ruling: Store::discard_pending is crate-private cleanup and must allow removal of a never-active pending import even when account.bin exists. Presence of opaque ciphertext cannot distinguish pending from active; AccountManager/B6 must establish never-active authorization. OS/file deletion failure is explicit incomplete/error and must not trigger fresh keys. No public/reset/FFI command now. iOS open/load/commit use finite locks; suspend blocks operations and clears cached secret, resume revalidates ownership/snapshot. — why: approved B6 requires discarding durable pending imports; cost if wrong: B6 caller authorization must prevent active deletion and incomplete cleanup must reconcile explicitly.

### A-07

Task 3 Ruling: Allow a surgical prerequisite update to app/lib/network/network_model.dart and app/test/network_model_test.dart during A3: A1 shared ErrorCode variants broke the existing exhaustive Dart switch and Android app compilation. Preserve old M2 cases, group new account errors into safe Korean generic text without raw detail/enum, add focused no-detail-leak regression and rebuild. Full per-code account UI mapping remains C1/C2; no native build exclusions. — why: real Android RED was blocked by earlier enum integration regression, not native behavior — cost if wrong: generic account message in diagnostic screen until proper C1/C2 mapping. A1 Rust gate passed but this Dart build boundary gap was discovered now; do not conceal it.

### A-08

Ruling: A4 activate_key must support serial transitions to a different key for legitimately never-active candidates; unconditional installed-key immutability is not valid active-account protection. A5 actor must reject create/recover/link for established/active states, B6 may transition only after proved never-active cleanup. — why: spec4 candidate is not an account, A5 recovery requires fresh DeviceSeed, spec7.3 permits pending discard->unconfigured->ordinary recovery without general Core reset/new writer — cost if wrong: every production key-transition caller must stay actor-authorized and A5 active-folder rejection/B6 never-active guard require explicit tests. Root confirmed this cross-task requirement to reviewer; no fix started until full review completes. Reviewer also investigating separate lifecycle intent/revision interleaving.

### A-09

Ruling: account integration tests that bind endpoints run under test-hooks+debug with configure_local(None) before any Start/prepare/recover, updating plan example commands with --features test-hooks as needed — existing local transport profile is deliberately double-gated and public-network tests are prohibited — cost if wrong: default-only test runs omit these gated tests, so C4/CI must explicitly run the debug feature suite and never count an omitted module as exercised. No production local-mode API or public probe is added for testing.

### A-10

Ruling: A5 adds real Active(LocalAccount) established-local payload with truthful LocalReady/PublishPending but defers the stored PendingImport transaction variant to B6; public phase already exists and unknown stored tags reject — plan says import path stays closed until B6 and an empty placeholder would invent invalid state — cost if wrong: B6 must explicitly extend codec with real durable receipt/transaction and never-active authorization, no existing Active/Candidate silent reinterpretation.
Ruling: RecoveryDisplay positions are human-facing1-based1..=24,3distinct ascending, answers same order; each confirmation answer rawUTF8 cap64B before single-word NFKD/trim/lowercase normalization, safe InvalidRecovery/no echo — plan left index base/answer bound unspecified, human positions avoid a0th-word UI and bounded normalization avoids untrusted allocation — cost if wrong: all Rust word indexing must subtract1 and C1/C2 preserve numbering; long whitespace answers must be trimmed to fit. Root supplied values to implementer and brief before contract consumers exist.

### A-11

Ruling: A5 commit Err fails lifecycle closed until explicit suspend/resume validates disk, with cfg(test)-only after_replace error injection in existing storage/atomic.rs to prove actual completed-new-write uncertainty — A2 atomic replace can complete before a durability error, so Err cannot justify a fresh overwrite — cost if wrong: transient save errors require an explicit reload/retry cycle; test helper must restore prior thread-local FAIL_AT on normal/Err/panic and never alter production/crash-boundary behavior. Root allowed this one extra file beyond A5 initial scope, no public/FFI/Release fault API/framework.

### A-12

Ruling: explicit new begin_create invalidates/replaces any prior never-confirmed creation flow (single actor flow, old secrets promptly dropped/zeroized), while established/PendingImport/failed/suspended states reject first; stale confirm/cancel cannot affect the replacement — plan's lost_flow_cannot_approve_new_flow and UI reentry should not require a process restart to recover a lost transient ID — cost if wrong: accidental duplicate begin invalidates the prior challenge, so C1 busy/request revisions must suppress duplicate taps and late responses. An already committed confirm wins actor order and next begin returns AccountExists. Root gave this policy before final checks; direct new-flow regression required.

### A-13

Ruling: extend A5 prerequisite scope only to transport/manager.rs Control large-key variant boxing and the flagged ConfigureLocal collapsible_if, preserving behavior and avoiding lint suppression/adjacent refactor — existing required CI strict clippy would fail on our A4 additions — cost if wrong: small message allocation/layout change needs covering manager/endpoint tests; full148 suite need not be redundantly repeated for these focused changes. Required strict workspace clippy and covering outputs must be clean before report/commit.

## B ledger

Source: `.superpowers/sdd/2026-10-18-m3-b-identity-pairing/progress.md`. 39 ruling-bearing paragraphs, 39 individual `Ruling:` occurrences. Source SHA256 `c5e9eb4014d5ba44f1dc5ea66b0567ab109c48379cca2e2483acba093e2db387`.

### B-01

Ruling: signed-but-expired records remain parsable renewal bases; expiration still denies ordinary auth — approved spec5.1/5.2 distinguishes renewal from authorization — cost if wrong: expiry classification must remain in B2/B3 tests.

### B-02

Ruling: For B5 use the approved spec7.1/7.2 over three conflicting plan transcription details: pair QR decoded cap2048B (IdentityInvite stays4096B), SAS HKDF info ASCII ChatProject/pair-sas/v1, and include QR expiry in Noise prologue. Exact prologue array is [1,direction,session16,E_peer32,E_transport32,E_installation32,N_transport32,N_installation32,resume_tx16_or_null,expires_at_seconds]. — why: plan omitted expiry, dropped the SAS namespace and broadened the pair-only QR cap; spec remains binding — cost if wrong: fixture/wire incompatibility with an implementation of the plan typos; no M3 pair protocol has shipped yet. Carry this override to B5 brief/review and update tracked plan in a separate documentation commit when no implementer is staging. Preflight rows previously marked consistent missed these three cross-document conflicts; now explicitly corrected before B5 code.

### B-03

Ruling: B2 owns durable tombstone/signed revoke intent and a real pending-notification boundary; B3 must wire authenticated same-PeerId peer fanout/receipt validation and test the actual connected-peer order before this cross-task requirement is complete — B2 explicitly requires authenticated peer transport while B3 depends on B2 and creates that transport — cost if wrong: connected-peer revoke delivery remains an explicit B3 acceptance obligation, never a fake B2 transport-success claim. B2 tests may inject peers/backend to prove persistence-before-notification-before-publication; no ordinary M2 probe may count as trusted auth. The two-task cycle was not caught by the initial shared-pair table and is now recorded before B2.

### B-04

Ruling: Approve narrow runtime.rs AccountServices injection through a shared private open path, with the external test constructor double-gated by test-hooks AND debug_assertions; update existing actor integration helpers to configure local transport and inject unavailable fake services before opening/restoring — automatic foreground reconciliation must never race public DHT bootstrap in tests, and the same actor/Store remains the sole writer — cost if wrong: Release API isolation and pre-spawn injection require explicit review; no alternate account writer/reset is authorized. Production backend may initialize lazily, uses SystemClock, and notifier=None means durable PendingNotification, never delivery success. B3 real authenticated peer fanout remains mandatory. Minimal mod/recovery integration is allowed only where actually required, without lifecycle redesign.

### B-05

Ruling: Add a truthful public PublicationView carrying verified record device/hash/accepted/conflict state beside the existing local cert view; use an internal packet-intent type distinct from the A5 public PublishIntent view-kind — public records contain RecordDevice entries, not remote certs — cost if wrong: C1 consumers must use the correct view and cannot infer certificates from record entries. Automatic establishment/resume/foreground refresh is allowed with <=24h scheduling, gate/cancellation checks before persistence/network handoff, and no Store lock across await. DHT lookup failure preserves durable local create/recovery and pending status rather than erasing keys or claiming observation.

### B-06

Ruling: confirm_record_conflict may rebase only onto the latest explicitly displayed same-generation record containing this exact local installation AND transport. A different recovery generation or exclusion of this writer returns AccountRestricted and retains held pending/tombstones — spec5.2 suspends own activity after removal/generation change; the initial replacement confirmation does not authorize repeated takeover after competition — cost if wrong: an excluded/older writer needs a separately authorized recovery/pairing path, not a conflict-confirm shortcut. Add focused same-generation resolution plus different-generation/exclusion refusal coverage.

### B-07

Ruling: Account control kind3 uses exactly [1,3,bytes(canonical RevokeIntent)] behind the bounded u32-BE frame, retaining strict canonical decoding of the signed inner array; B3 must consume this same representation and reject alternate/noncanonical wrapping — the plan names a payload but does not explicitly fix nested-array versus serialized bytes for this control variant; no signed fields/domain change — cost if wrong: an alternate nested-array implementation would be wire-incompatible, but no M3 protocol is shipped. Carry the exact representation to B3 brief/review and add explicit envelope shape coverage.

### B-08

Ruling: B2 may append the real bounded RecordState to the unshipped established snapshot and reject legacy A5-only established array7 as StorageCorrupt, without deleting/overwriting/recreating anything; Candidate37B stays byte-identical — no A5 account creation FFI/build has shipped and automatic fresh generation during decode would silently alter durable authority — cost if wrong: an early internal A5 established snapshot needs an explicit migration tool/policy before reuse. This is not authorization to erase a failed load or make a new account automatically. Tests must cover legacy rejection and file preservation; current-schema restart/high-water tests remain required.

### B-09

Ruling: B3 owns remote PeerId record resolution/accepted-cache/high-water and grant invalidation needed for connect_account(peer), while B4 adds contact metadata and consumes this authentication; B2 currently has only own-account RecordState — B3 must authenticate other contacts per spec5.2 before B4, and the B2 report's B4 lookup extension note cannot defer B3's mandatory auth — cost if wrong: B3 may require narrow account manager/schema/runtime integration beyond the initial file list, to be raised before coding; no rollback-unsafe ephemeral auth, second writer or cache-only mutation permission is accepted. Task3 brief now carries the actual B2 interfaces and this gate, pending any B2 review fixes.

### B-10

Ruling: For fix1, distinguish initial unallocated confirmed recovery from an already competing writer. Only setup intent Replacement with accepted=None, pending empty, allocated_seq=0 may retain unknown future signed evidence in conflict slot with LocalBlocked (provisional blocked evidence). Absence must preserve it and cannot allocate. Later fresh Found must pass current-time/signature checks and seq>=observed before initial consent can consume that base and allocate its one replacement; it must not clear an actual held Conflict. Once accepted authority or any allocation exists, unknown future competing packets durably hold and absence/clock catch-up never clears them. Exact known accepted/pending wrappers made future by rollback remain blocked without inventing a new conflict — initial recovery was explicitly approved to replace a usable base, while repeated takeover was not — cost if wrong: provisional-vs-held evidence must survive canonical reload and has focused transition tests; a blanket hold would strand initial recovery, whereas an overbroad exception would reopen takeover. No new schema field is required if existing state+setup+allocation conditions enforce this distinction.

### B-11

Ruling: B3 may make narrow necessary integration edits to account manager/mod/storage schema/runtime/event in addition to its listed auth/connection and transport files. A typed auth-status CoreEvent needs an exhaustive thin FRB projection and standard generated binding update now if workspace compilation requires it; allow only that public status bridge plus direct tests, while full account commands/model/UI remain C1 — B3 explicitly requires independent account-auth-ready events, and the present From<CoreEvent> match is exhaustive, so hiding/dropping the event or leaving workspace uncompilable is not acceptable — cost if wrong: B3 will regenerate a small public status binding before C1's broader generation, adding review surface; preserve single stream, use only tools/gen_bindings.sh, no manual generated edits or NSE account functionality. Raise any larger ownership/architecture change before coding. The shared schema/manager remains one actor/writer; remote caches/auth state must be real/bounded/durable per spec.

### B-12

Ruling: Approve B3 real durable remote auth cache append to unshipped established schema (array8->array9), bounded <=1000 peer entries with no silent eviction/reset of observed high-water. Use same Store actor, persist before grant, strict bounded/signature/canonical reload, existing whole-file4MiB and snapshot-wide pending32/tombstones256 caps; tombstone256 is not multiplied per remote peer. Local account keeps B2 RecordState. Missing capacity/failed persistence leaves old state and grants fail closed. Old unshipped schema may reject without modifying/deleting/resetting files, with preservation test, or raise an explicit deterministic migration proposal; never manufacture fresh authority on decode — B3 needs durable per-peer auth freshness before B4 contacts — cost if wrong: added public cache state expands schema/review surface and internal B2 snapshots need explicit migration to reuse, no shipped M3 account compatibility is claimed.

### B-13

Ruling: For durable revoke confirmation use a dedicated post-auth bidirectional control stream: request exact kind3 RevokeIntent frame, receiver validates actual authenticated same-PeerId live grant + generation/base/target and commits in same actor, then returns byte-identical kind3 frame as receipt. Request/response stream direction and exact outstanding request distinguish receipt from command; no recursive processing or generic signed echo counts as authority. Source validates exact frame/current authorized receiver and keeps timeout/disconnect/error pending; no new kind5 wire amendment — current wire already supplies signed kind3 bytes and authenticated stream binding can acknowledge durable processing — cost if wrong: receipt correlation/direction/live-grant checks need focused adversarial/order tests, and write-complete alone must never count as durable ACK.

### B-14

Ruling: Do NOT delay revoked target's authority/connection termination to obtain its notification ACK. Select notification recipients from authenticated same-account peers that remain authorized after the local tombstone, immediately revoke/close target, then deliver to remaining connected trusted peers and await durable confirmations before public mutation. An actually installed, empty trusted-peer registry may prove no recipients; absent notifier never does. If a received notice removes self, persist block/stop own activity and close immediately; inability to send ACK must remain sender pending, not a reason to preserve ordinary rights. No required remote-erasure/offline-receipt claim exists — spec5.2 requires immediate termination for removal/tombstone and spec5.1 only promises delivery to connected trusted devices — cost if wrong: removed target may receive no final notice and receiver-own-removal can lack ACK, explicitly truthful; use three local participants if needed to prove remaining-peer persistence-before-publication while target is already closed. This resolves proposed pre-tombstone-recipient/drain ordering against the binding immediate-close requirement.

### B-15

Ruling: Approve crate-private bounded account registration/dial controls on the existing endpoint, and account ALPN dispatch before M2 prepare_session; account actor borrows its secret owners for proof signing, no cloned keys/new identity/writer. Runtime EventBus may become production-held to support real status events; narrow integration scope is already approved — existing transport only had M2 entry points — cost if wrong: cross-actor cancellation/admission/connection-registry races need actual loopback tests and review, M2 metric/session behavior must stay unchanged.

### B-16

Ruling: NetworkModel._event may explicitly ignore AccountAuthChanged and AccountCacheUsed with documented cases, because it owns the M2 diagnostic projection; preserve both typed events unchanged in the shared ChatEvent/CoreClient stream for C1 AccountModel. Do not add account state to this unrelated M2 model or silently drop events in conversion/stream forwarding — new ChatEvent makes its exhaustive match compile-fail, but every consumer need not project unrelated event kinds — cost if wrong: C1 must consume/snapshot the real auth/cache status; add affected coverage showing M2 model stability and typed shared conversion/stream preservation now.

### B-17

Ruling: B3 may minimally update inherited B2 tests for the real schema array9 and an actually installed empty authenticated-peer registry completing a notification with zero recipients. Preserve the missing/unavailable transport distinction and fake notifier failure/persistence-before-publication tests; never change expectations just to bless an absent/constant-success notifier — pre-code B3 integration explicitly replaces production None with real authenticated registry, and schema append is authorized — cost if wrong: a weak empty-registry assertion could hide unwired delivery, so actual three-peer order/target-close tests plus source review remain mandatory, and old absence/failure coverage must not be weakened.

### B-18

Ruling: Preserve the notification barrier across retry/restart by durably capturing the expected recipients in the same initial tombstone/pending-intent commit. Approve narrow PacketIntent array5->array6 storage extension, bounded <=4 deduplicated exact installation/transport recipient pairs (target excluded), and notifier expected-set integration; no new public wire. Explicit captured-empty differs from uncaptured/absent transport. Missing/disconnected/unconfirmed captured recipients keep AwaitingNotification; a later empty live registry cannot erase a failed delivery obligation. New operations may capture a truly empty installed registry. Recheck current authorization, never accept an unrelated/new key as an old recipient; authority/conflict changes hold work, not fabricate receipts. Do not silently replace the expected set during ordinary retry/renewal/rebase or decode; raise explicit authority-resolution ambiguity if it arises. Partial ACK persistence is optional only if implemented simply in the same actor; absent partial progress may conservatively require repeat ACKs, never false success — spec requires durable connected-peer confirmation before public mutation, and B3 self-review demonstrated that resampling only current peers loses this decision after failure/restart — cost if wrong: publication can remain pending until a captured still-authorized peer reconnects/confirms; no implicit waiver or new UI override is authorized. C1 must describe waiting/pending truthfully. Add actual failure->disconnect->retry/restart regression and fresh-empty success distinction. No need repeat unchanged broad suites; rerun amended auth/publication/schema coverage and relevant strict checks after fix.

### B-19

Ruling: B4 may append bytes(canonical contacts) to Active array9 -> array10, version1, same encrypted Store writer. Contacts sorted distinct by PeerId, <=1000, strict alias/verified digest validation; legacy unshipped array9 may reject without any file mutation/deletion/reset, with preservation regression. Keep AccountView.contacts Vec<PeerId> for existing consumers and add contacts()->Vec<ContactView> for full metadata; no alternate model/writer — task explicitly requires durable local contacts and public views while predecessor only has empty PeerId list — cost if wrong: another unshipped schema increment rejects pre-B4 snapshots and later C1/B6 must consume actual metadata API; native/shipped compatibility is not claimed. B6 transfer wire stays the specified four-field contact tuple, never this private metadata codec.

### B-20

Ruling: New-device metadata must be based on actual successful B3 fresh proof, never invitation cert, record lookup, or transport alone. Persist prior authenticated installation and a minimal durable new-device indicator so observing a changed installation cannot immediately erase its own notice by overwriting the baseline; offline-added contact initially has no authenticated device and must remain device-unverified. First authenticated installation establishes baseline without a change notice; later different authenticated installation raises notice while safety verified/digest remain tied only to the same PeerId. No new acknowledge/reset API is introduced — spec6 requires changed-device notice independent of stable safety identity — cost if wrong: notice remains conservatively present until a later explicitly designed acknowledgement flow; private contact metadata needs one extra boolean beyond implementer's proposed five fields, and actual authenticated event/commit ordering must be tested rather than inferred from lookup.

### B-21

Ruling: Clarify/supersede the preceding baseline-only-from-proof detail before implementation: persist known_installation_or_null as a public comparison hint, initialized from the verified signed invite cert, but never as proof/readiness. Only actual successful B3 proof sets new_device when its installation differs from that hint (or establishes a null baseline, needed for later imported contacts). This supports detecting an already-stale invite's device change on its first successful connection. Private tuple is [peer32,alias,verified_0_or_1,verified_digest32_or_null,known_installation32_or_null,new_device_0_or_1]; runtime device_unverified derives solely from a currently authorized live grant, always unverified offline/after restart until fresh proof. Preserve baseline/notice/alias/verification on duplicate invites and keep sticky notice across restart — worker's concrete signed-invite baseline avoids losing a real pre-first-connection recovery notice, without granting auth from a hint — cost if wrong: hint can cause a conservative new-device notice after first proof; it can never grant rights and imported contacts without a hint cannot detect a pre-import change. Previous no-ack/reset, same-Peer safety and durable ordering rulings remain.

### B-22

Ruling: Permit narrow B4 amendments to inherited account_auth/publication fixture outer array9 assertions/builders for new outer array10 and empty contacts; nested RecordState array9 remains unchanged, all original behavioral expectations retained — real storage append requires fixture compatibility — cost if wrong: weakening inherited assertions could hide regression, so review exact fixture diff and include affected auth/publication/storage coverage. No broad replacement of nested array lengths.

### B-23

Ruling: B5 may narrowly extend account/manager.rs for the existing sole actor's pair runtime, public AccountHandle commands/local approval and pair_view() returning the public PairView; AccountView.pair may remain Option<FlowId>. Pair public types may live pair/mod.rs. Also permit required bounded pair route/dial enum variants in transport/types.rs and session.rs only where the existing dispatch needs them, alongside planned endpoint/manager integration. Preserve pair ALPN separation before M2 prepare_session, actual Endpoint binding and bounded cancellation/admission; no M2 timing or session semantic redesign — planned AccountHandle pair commands and real channel cannot be integrated solely by adding standalone pair modules, and transport's existing typed routing may span these files — cost if wrong: additional cross-actor lifecycle/enum surface must be reviewed with actual loopback plus M2 wrongALPN/magic regressions, not hidden by unconnected unit-only channel. No schema/key/runtime/events/bridge expansion approved; raise if actually required. PSK/QR must never enter ordinary PairView/AccountView/events/logs.

### B-24

Ruling: B5 may fill the otherwise unspecified ordered pair channel framing with bounded canonical prefaces, the two exact Noise handshake messages, encrypted exact PairProof frames, then an encrypted existing-certificate-or-null frame (E sends existing canonical cert array, N sends canonical null), and encrypted canonical [1,0] meaning only local code-confirmed. Roles/session/channel are already pinned by the transcript and actual endpoint; no approval wire message exists. Every step must reject unexpected shape/order/duplicate/role/endpoint and enforce existing frame bounds. No new production kind1+ transfer protocol is reserved here; B6's exact envelope/chunk/receipt contract remains binding — the approved wire table defines objects but not these ordered carrier steps, and N confirmation must be communicated independently of E local approval — cost if wrong: new framing needs precise interoperability/security review and malformed-order tests, and later B6 must integrate without silently changing the approved payload format.

### B-25

Ruling: Allow debug_assertions+test-hooks-only bounded synthetic bytes/observability to exercise the actual B5 Noise send gate with a fixture substitute for the future B6 durable permit. The fixture permit is issued only after the same real local E approval/N confirmation/channel pins and cannot restore stale/cancelled/expired sessions or invoke an automatic UI approval. Production has no durable provider/permit and sends zero gated bytes until B6 supplies real commit-before-first-byte; no private-key getter or new production transfer kind. Count actual encrypted writes and decrypted fixture receipt, not a fabricated zero counter — B5 must prove real channel gate behavior before B6 exists, while B6 owns actual durable identity disclosure — cost if wrong: a test-only substitute proves gate integration but never actual transaction durability; B6 must replace it with real Store failure/order evidence and Release must exclude injection. Any temporary fixture permit must be clearly identified in the B5 report and review.

### B-26

Ruling: B5 encodes/pins optional resume TxId exactly but fails closed for pair_begin(Some(tx)) or scanning a resume QR until B6 supplies durable transaction/candidate-pin validation; no ephemeral session may invent a resume right — B6 explicitly owns persisted resume receipts/pins and no such transaction exists yet — cost if wrong: B5-only source cannot offer usable resume, and B6 must replace this explicit boundary with tested same-tx/fresh-approval behavior before full M3 acceptance.

### B-27

Ruling: Prefer a minimal opaque internal disclosure permit tied to the existing exact flow/session and Noise handshake hash over a naked durable=true argument. Only debug+test-hooks fixture construction exists in B5; real session/role/local approval/both-code checks still run independently at send time and stale/cancelled/expired sessions reject. No production constructor/provider until B6 proves durable disclosure commit; no generic permission framework or public API — an unbound bool is easy to carry across the wrong session when B6 integrates, while the permit's few identity fields use already-known pins — cost if wrong: small extra private type/interface must be wired by B6, but test permit cannot establish actual durability and must remain excluded from Release.

### B-28

Ruling: Retain approved pinned snow0.10.0 with explicit residual upstream-memory-erasure limitation; zeroize every app-owned QR/PSK/plaintext buffer, close/drop on cancellation/rejection/AEAD failure, and use safe public best-effort overwrite of transport keys (plus set_psk slot0 to zeros before handshake drop/conversion if valid for completed/error state). Do not use unsafe memory access, silently vendor/fork cryptography, or claim optimizer-resistant wiping of upstream handshake/chaining/ephemeral copies. This remains an explicit security limitation for B5 review and final M3 evidence, not a proven full-memory-erasure guarantee — root inspected primary local snow0.10.0 HandshakeState.psks plain arrays and default CipherChaChaPoly.key array; no Drop/Zeroize implementation or zeroize feature exists, public transport rekey_manually supports replacement only — cost if wrong: freed upstream memory may retain handshake/PSK/key material until reuse, and stronger post-cancellation RAM-erasure threat requirements need a separately reviewed upstream/library hardening change. No new private-key export/storage/crypto protocol is authorized. Root checked official https://docs.rs/snow/0.10.0/snow/struct.TransportState.html for public overwrite API; docs feature page failed, feature/source facts came from exact local pinned crate. Final reviewer must see this ruling and assess the actual implementation/claims.

### B-29

Ruling: B6 must explicitly convey N's platform claim before issuing N cert. Current fixed B5 QR/preface/proof have no N DeviceKind and N encrypted cert carrier is null; root asked original implementer read-only and confirmed no alternate field/API. For B6 replace that N-only null carrier with encrypted canonical [1,kind] (desktop0/mobile1), keep E existing canonical cert carrier and QR/preface/proof unchanged, role/order/canonical/size validate and pin candidate kind alongside exact installation/transport. Bind resumed transaction/cert/payload to the same claim; no physical-OS attestation claim. This supersedes the N-null carrier detail only when B6 integrates, not the currently reviewed B5 head — exact N cert kind cannot be guessed from direction or E platform and the public metadata is already Noise/authenticated-key-bound — cost if wrong: one unshipped carrier incompatibility and B5 raw fixture amendments are needed in B6; claiming platform truth beyond authenticated candidate claim would be false. No current B5 source mutation or new account ALPN kind is authorized by this future ruling.

### B-30

Ruling: Active pair-state carries E transaction list and separate optional completed N import-origin metadata, preserving exact tx/E pins/Ncert or hash/payloadhash/generation/base/verifiedreceipt for fresh-channel receipt/status resume; do not duplicate N secret payload unnecessarily or treat N origin as an E disclosure transaction. Initializers creation.rs/recovery.rs and direct schema fixtures may change to initialize this real state — N after activation needs exact durable provenance without inheriting E role or restoring old approval — cost if wrong: one unshipped private codec extension with mandatory canonical/size/pin tests; resumed role must be derived from exact transaction role, not merely Active state.

### B-31

Ruling: Add narrow private certificate.rs sign_cert_public(keys,installation,transport,kind,at), existing sign_cert delegates unchanged. Minimal inherited B5 fixture/assertion changes may account for automatic B6 production progression while retaining actual preapproval0B/SAS/mismatch/expiry and doublegate boundaries — E must issue N cert using authenticated public keys without knowing forbidden N DeviceSeed; production transfer changes later phases — cost if wrong: helper must preserve identical signing domain/canonical cert checks, and old synthetic tests must not hide production durability/parser failures.

### B-32

Ruling: B6 keeps unpinned Candidate37B; adds canonical CandidatePinned tag2 array8 [1,2,device_seed,revision,Epin,tx,payload_hash,total_bytes], committed after approved E envelope but before accepting first chunk; PendingImport tag3 array8 [1,3,device_seed,revision,Epin,exactpayload,verifiedreceipt,recordstate] commits validated import+receipt atomically. Active pair-state [1,Etx_list,origin|null], Origin [1,tx,Epin,Ncert,generation,base_hash,payload_hash,receipt]. Reject unknown/old unshipped layouts without mutation/defaults — spec requires N durable E/tx pin separate from candidate before import, and real never-active marker plus completed provenance — cost if wrong: more private codec states need exact canonical/own-key/size/failure/restart tests, CandidatePinned counts pending and blocks ordinary setup until proven discard; E and N persisted hashes never restore local approval.

### B-33

Ruling: No new publication-status wire. PendingImport uses bounded fresh-backend refresh while live pair and explicit refresh_record after restart; only latest durable current accepted membership activates N — exact B wire table has no status object and a notification would not establish authority — cost if wrong: pending display can wait for another bounded lookup/explicit refresh; unavailable/conflict/time rollback must remain pending/restricted rather than spin or imply success. All final acceptance time checks sample Clock after awaits.

### B-34

Ruling: B6 public AccountView adds durable disclosure_possible, pending_pair_transactions:Vec<TxId>, pending_import_requires_remote_revoke; PairView adds optional tx and real transaction phases. Direct struct-literal consumers may adapt for compile, final FRB projection remains C1. E disclosure derives durable history even without ephemeral flow; pending list excludes Observed while retaining history, N CandidatePinned/PendingImport conservatively warn publication may require remote revoke — cancelled/restarted UI must not invent restored flow/QR/approval merely to display durable security state, and absence of receipt is not proof of global absence — cost if wrong: C1/C2 must distinguish history warning from active pending operation and never describe local discard as public removal. PendingImport live-flow polling interval1s, bounded by actual session lifetime, explicit refresh on restart.

### B-35

Ruling: Extend B6 narrowly to transport/manager.rs crate-private account-only forget_pending_key control, after actor proves retained never-active state and synchronously blocks readiness/quiesces endpoint/sessions; replace/drop retained transport key, clear endpoint/peers, keep blocked until verified cleanup/normal next setup. Permit failed cleanup retry only from retained live known-never-active+explicit cleanup state; arbitrary failed-load/Active deletion stays forbidden — quiesce alone leaves transport Manager.key retained and NetworkOp::Start could reuse discarded candidate secret — cost if wrong: extra cross-actor control needs actual old-endpoint-not-startable/same-Core recovery/failure ordering tests; partial-delete restart without provable state remains fail-closed and must be candidly reported. No public reset/NetworkOp or active-delete scope.

### B-36

Ruling: Same-tx reconciliation must cover Edisclosurecommit before N receives envelope. Plain durable Candidate with NO E/tx/import pin may scan a verified ExistingDisplays resume QR as a first-delivery offer, not restored transaction authority. Require real fresh Ecert/currentauthority/actual endpoint/proof/Noise/SAS/Nlocalconfirm plus E durable exacttx/payload/Ninstallation+transport+kind and fresh Elocalapproval. Only envelope then durably pins N before chunks. CandidatePinned/PendingImport/Active still require exact stored E/tx; unknown tx alone grants nothing. No new pre-payload wire or new tx/history loss. This explicitly narrows earlier blanket stored-tx-only rule for unpinned Candidate first delivery — crash can occur after Ecommit but before N ever learns tx, yet same candidate/transaction must remain recoverable — cost if wrong: C3 resume for this boundary requires E-display direction, code must not apply exception to partly pinned/imported/active N, and focused bothrestart/newapproval/identicaltx+payload/0preapproval test is mandatory. E durable history stays conservative even when no byte was sent.

### B-37

Ruling: B6 fix1 may narrowly extend contacts.rs helper ownership/capacity and direct tests/callers necessary to cover newly used imported-contact encode/decode error paths. No unsafe freedheap inspection, blanket contact subsystem refactor, or weakened app-owned erasure claim — B6 new import/PairState fallible decode currently depends on a pre-existing helper that can free unwiped contact buffers — cost if wrong: Contact Drop/ownership amendments need all direct compile consumers and codec/error regressions, plus B4invite coverage; original public UI/Dart lifetime limits remain distinct.

### B-38

Ruling: I1 postactivation Store-success/current-time failure returnsErr, closes grants, blocks network and locks actor until actual reload; already-durable Active remains conservatively established/non-discardable. Reload with still-invalid authority must project non-Active/LocalBlocked and deny normal auth/discard, later normal current Active refresh may restore authority. No second ambiguous corrective commit or silent rollback to never-active — commit can finish before process death and durable established state cannot prove no prior authorization was issued; a second rollback commit creates another uncertainty — cost if wrong: rare clock movement during activation permanently loses never-active discard eligibility, although authority remains blocked until current signed membership is valid. Report and regression must expose this exact public/durable consequence, including PairView.

### B-39

Ruling: I5 may add per-instance debug+test-hooks hold_next_key_transition_for_test inside actual transport Activate after stop await/before assignment+ticket branch; I4 may add deterministic reached-stop query/completion. Hooks release on explicit release/drop or bounded timeout and never alter production contract. Release probe covers added APIs — held bind cancellation returns Err already and cannot reproduce reviewed Ok-after-stop QR bug; exact reached crash boundary is mandatory evidence — cost if wrong: hook itself must not mask old behavior by introducing cancellation rejection, leak blocked workers on failed tests, or cross-contaminate actors/tests.

## C ledger

Source: `.superpowers/sdd/2026-10-18-m3-c-app-validation/progress.md`. 51 ruling-bearing paragraphs, 51 individual `Ruling:` occurrences. Source SHA256 `ed4f21868247bda3a2b52cf82af5ab569716be131165f4d37800029b84e01e87`.

### C-01

Ruling: C1 may narrowly amend core account/manager.rs and public account/types.rs/pair public view definitions plus direct compile-time consumers for atomic public projection and one monotonic AccountChanged revision. Do not add an alternate writer, duplicate shadow authorization state, separately sampled contact/pair joins, or serialize private owners — the C1 plan names event/error but explicitly requires revision inside AccountHandle and complete Account/Contact/Pair views, so sole actor must emit the coherent public projection — cost if wrong: unshipped public type changes need direct source consumers adapted and exact subscription/order/late-response regressions; any additional native/schema/crypto/helper expansion needs a concrete proposal before code. This is preparation only; B6 still must be accepted first.

### C-02

Ruling: Approve actor AccountView.contacts/pair realviews and devices RecordDevice from acceptedrecord with localcert-only fallback when noacceptedrecord, public-only record/revoke/ID/view serde minimalhelpers/directconsumers, separate checked monotonic public_revision+cached projection stamped at actor boundaries and before snapshotreply. Pure parse_invite/safety wrappers allowed — C1 needs complete coherent UI projection without independent getter joins or fabricated remote certs — cost if wrong: source consumers/types change unshipped public API, event.revision must equal view.revision/fullu64BigInt, cache must never hold secretresponse/privateowners, event/order/late reply tests mandatory. No wire/schema/dependency expansion.

### C-03

Ruling: Locked/error first-open uses realprovider and safe AccountModel code; retryopen only when actor absent, existing lockedactor uses actual lifecycle reload/resume respecting native foreground/suspend intent. Minimal C1 locked/retry host, fullscreens C2 — CoreSlot.open_account returns existing handle and cannot itself reload failedactor — cost if wrong: background retry must not override native intent; no overwrite/newaccount/reset or fake unsupportedOS protector.

### C-04

Ruling: C1 nativeintegration may add minimal debug+test-hooks local-storage setup retaining realprovider + fakebackend/Clock BEFORE open; prefer existing CoreSlot.start then configure_local(None), same B6fixture pattern, over new startLocalhelper unless exact invariant requires it. Reject setup after account initialized/network Publicstarted; no Publicfallback. Existing FRB stableunavailablestub pattern allowed if codegen requires, actual injection path must be doublegated and Release cannot configure fakes. Write/read/locked/missing use explicit phases/separate owned processes/path/publiconly receipts; decorated realprovider error phases clearly INJECTED status/UI evidence, not actualOSlock — realDPAPI/bridge persistence needs localservices without Public DHT or genericreset — cost if wrong: runtime Release refusal and helperexclusion must be proved, and nativeproof cannot be inferred from fakewidget/decorator alone. No CoreSlot.start_local source extension approved yet; worker checking simpler reuse.

### C-05

Ruling: Explicit failed-foreground retry uses actor Retry command, validates nativeforeground/currentrequest and rejects cleanup_pending before Store changes, allocates fresh network.block ticket and atomically replaces lifecycle request only while captured request remains current; existing biased watch executes lifecycle once. No lifecycle_gate override/direct duplicate reload/automaticretry on snapshot. Same-ticket retry is invalid because complete.fetch_max cannot turn failed2 into ready1 and queued old commands need a newepoch — existing request_resume returns cached foreground failure — cost if wrong: new retry must not strand status or override native suspend, and ready/suspend-race/stalequeued/cleanup tests required. Minimal code-only actual failure projection or typed retry outcome allowed; do not lose Missing/Corrupt distinction or retain raw detail. Worker outline approved.

### C-06

Ruling: EventBus may cache latest public AccountChanged and replay under the existing subscribe lock after Connection. Dart listen-before-snapshot alone cannot establish FRB worker registration ordering; actor publish-before-snapshot plus same-lock cached replay closes capture/registration gap. Preserve one underlying subscription and revision filtering, no private/secret response cache or unrelated event replay. Verify both registration/publication orders and actual bridge receipt; connection and account revisions remain independent.

### C-07

Ruling: Tiny feature+debug AccountServices::offline_for_test in existing publication.rs may encapsulate RecordBackend returning RecordUnavailable and current-anchored FakeClock, consumed only by gated realprovider integration fixture. No new pkarr dependency, no general test framework or prod fallback — FFI cannot name trait's pkarr::SignedPacket without a direct dependency, and native storage/restart needs offline services beforeopen — cost if wrong: Release core factory exclusion and runtime stub refusal must be verified; nativewrite/read proves local-ready identity/endpoint persistence, never publication/authready. Worker preserves exact lock_errorcode.

### C-08

Ruling: Minimal PairTransaction.tx cached validatedpublicmetadata allowed in transaction.rs/twoexistingconstructors/decode; pending_pairs maps t.tx rather than envelope()->private Payload::decode on every publicprojection. Existing decode already validates p.tx==tx and canonical11fields; root inspected exactpaths. Preserve serializedbytes/envelopevalidation/corruptionrejection, no broad B6 searchrewrite — diagnostic no-op publish_view made isolatedresumePASS, exactsourceSHA1edc4ba7b95335e71b4ad64315ce603b579c67df9da25eabc1992d7143ea8365 restored. Costifwrong: stale duplicate metadata or unproven timinghypothesis; focusedresume must substantiate improvement then affectedpair suite, otherwise traceexactAuthFailed branch rather than relaxingassertions. No new wire/schema/authority.

### C-09

Task1fix1 exactRED reported0.02s: emittedrevision2 Locked(StorageLocked), snapshotrevision2 Unconfigured, nottimeout. Existinglifecycle_gate changes no watch so no self-triggerloop. Ruling: tinyprivate snapshot_view helper directlyused by Command::Snapshot/unit one-shot EventSink boundary is allowed if simpler than existingpublish_view returningcachedprojection — deterministic test of actualpath without publicfixture/API/schema — costifwrong negligibleinternalextraction, avoidgenericabstraction/duplicatedsnapshotlogic.

### C-10

Ruling: C2 compactplan approved: samegateownedmodel intoChatApp/AccountRoutes; shortexactflowrouteownership/back/backgroundcancel, sensitivityserializedrefcount/withheldchildonfailure, boundedexistingtheme/semantics. Minimaldirectconsumer model.beginCreate may cancel exact late reply.flow on stale-secretgeneration while sameappgenerationlive; model previously discarded ID so route could not Rustcancel. Stale cleanup cannot clear/errornewroute or cancelreplacement; errors codeonly/no successclaim, coreprojection authority. Windows Dart setSensitiveScreen may callchannel (currently no-op) to surface missinghandler until C3nativeimplementation — no falseprivacy success — costifwrong pendingflowleak or staleUIcleanup; exactlatecancel/replacement/nativefailure tests required. No native/API/schema/duplicate-subscription expansion.

### C-11

Ruling: Task2fix1 minimumapproved: route captures confirmationFuture andtransfers copiedexactflow+future to narrow AccountModel.cancelAbandonedCreate(afterConfirmation); waits, skipsCorecancel onsuccessfulconfirmation, otherwiseexactcancel onlywhile sameappgenerationlive; no newerroute secret/error mutation, normalbusyguardunchanged. I2 selectionticket+copied local/peer/display binding andconfirmguard. Publicfixture deferredInvalidRecovery Back/dispose andsuccess-no-rollback proofs required — fixes ownership only, not broader authority/API/schema — costifwrong abandonedflowleak or stalecleanupaffectsreplacement, covered byfocusedregressions.

### C-12

Ruling: C3actualgaps warrant publiccore/FRB directextensions: PairRole+PairResumeView{transaction,role}, PairView.localRole, distinctstoredresume targetlist includingActiveNorigin+pendingEtx/CandidatePinned/PendingImportNew, preserveexistingpendingIDs/warningsmeaning. pair_scan success returns exactFlowId throughthinFRB/client/model+compileconsumers/generatedscript; no privatepayloaddecode/storage/wirechange. Needed ActiveNpublicrole/resume and exactabandonedscan cleanup were genuinelyunavailable — costifwrong unshippedtypedAPI/directfixtureadaptation, coherentrevision/role/lifetimeregressions+strict/generatorproof required. Nativecode/metadata last-confirmation unavailable truthful, issuance labelledissuance. Latebegin/scan/pendingconfirm/approve exactoldflowcleanup allowed, no generalbusy bypass/newrouteerror/secret mutation, no disclosure rollback/cancel-success beforecoreack.

### C-13

Ruling: C3Windows DWM savedattrs+checkedseed/HRESULT/invalidation/rollback/restore design approved. TinyDebug-only publicattrs/injectedWM/forcedactivationfailure fixture allowed (not key/approval getter), integration-onlyDartcaller, Release exclusion required; accountcore/FFIfakes still feature+debugdoublegate. ActualShellvisualproof separatelylabelled becauseS_OKcan discardnonpreviewbitmap. Android/iOSexistingchannel publicidentityURI pendingtake/delivery+schemeregistration and WindowspublicURIargvintake approved; directMainActivity/AppDelegate/SceneDelegate/narrowDartadapter allowed, sameRustparse→preview/noautosave/connect, no newdeps/pairPSKURI/hostregistrymutation. WindowsOSregistration/warmforwarding unprovenpackaginggate, prepareconcreteruninstructionsC4; nogenericinstanceframework. Costs platformsourceverificationgaps remainhonest, no fakecompletion.

### C-14

Ruling: Actualpair_scan createsinitiatorSession beforedial; dialErrclears but handshakeErr retainslive no-channel scanner untilfailure3/expiry, hidingflowonErr. Allow narrow scan-only exactcapturedflow cleanup onhandshakeErr usingexistingdrop/close semantics (partialtasks/channelscanceled), scannerpairNone regression via existingmismatchedPSK test, QRownerretrybudgetunchanged. No globalreset/wirechange — required unknownflowerror fix fornewreturncontract — costifwrong prematurelyclosedowner/replacement, exactrole/flow/resourcechecks required.

### C-15

Ruling: C3may narrowbootstrap AccountGate Locked→DevicesScreen action whendurablepending/importwarnings exist, sameownedmodel/insideNavigator, explicitonly. ExistingGate retryonly blocks approved same-live cleanup_pending discardretry (retryOpenrightlydenies), so neededroute not authoritychange. Failedactualdiscard remainsLocked/error/retryable; successful followscoreprojection, no reset/automaticrefresh. CandidatePinned/pendingwelcome getsresume/devices/discard, no create/recoveroverwrite; plainCandidate coreallowedflows preserved — costifwrong unreachablecleanup or misleadingdeleteUI, actualLockedbutton/discardfailure testsrequired. No modelduplication/provider/native/coreauthorityextension.

### C-16

Task3actualWindowsintegrationcaught originalproposal GetWindowAttribute(FORCE_ICONIC/HAS_ICONIC)E_INVALIDARG-2147024809; childwithheldcorrectly, retainedc3-windows-privacy-diagnostic.log. Ruling: officialenumdocs explicitlySet-only(rootbrowsed https://learn.microsoft.com/en-us/windows/win32/api/dwmapi/ne-dwmapi-dwmwindowattribute). Replaceunsupportedgetter withcheckedfalsebaseline duringnewwindowOnCreate/soleappsetter, trackONLYsuccessfulsetvalues/status (unknown/partialonfailure), restoreintendedpresensitivebaseline notfailedpartialtarget. No allegedOSreadback/proof; debugfixture labelssetter/handlerstatuses; activationfailwithholdschild, actualPeekseparate. Costifwrong partialrestore/falseprivacyclaim, actualnativefailure+updatedintegration required. Rootalsoaskedconcreteforegrounddispose→lastFlutterraster→nativerestore ordering audit; proposalpending, no genericfenceframeworkpreemptively.

### C-17

Ruling: ActualC2SensitiveScreen.dispose restorednativebefore replacementraster. C3small sharedfence approved: capturestrictlylaterreplacementFrameData.frameNumber inpostframe/scheduledframe, waitsameFrameTiming rastercompletion viaaddTimingsCallback, serializedfalse onlycurrentreleaseticket/refszero/foreground; newownerinvalidatesrelease, backgroundholdsuntilresume, unknownnegativeIDs failclosed/no timerfallback. RootverifiedofficialFlutterdocs+installed3.44testwindowgetter(noframeData setter). Narrowtestbinding/metadata source allowed, no productdisablefence/testSDKmutation; nativeintegration actualtimings. Observercleanup/minimalmechanismrequired. Evidence is rastercompletion, NOTOScompositor/Peekguarantee — costifwrong lingeringpreviewprotection or prematureunprotect, oldvsnewframe/newowner/background tests+nativeproof required.

### C-18

Ruling: Spec7.2 requires identifiable peer platform beside SAS; minimal PairView.remoteInstallation?/remoteKind? from verified Channel approved. Existing kind requires exact validated record installation+transport match; New kind from verified encrypted carrier; no invented names, OS-attestation claim, private payload decode or wire/schema change. Waiting/no-channel projection absent, exact flow replacement clears fields; bridge/direct consumers/generated files only. Cost if wrong: stale/wrong peer identification next to SAS, both roles and stale-binding regressions required. Final builds/generation must reflect amended source.

### C-19

Ruling: Expose resume scan/paste using unchanged Rustpair_scan pinnedauthority. Selectedresume tx requires exactreturnedflow + sameflow public PairView.transaction match before localconfirm/approve UI action; obtain coherent snapshot through existingpath if needed. Mismatch/absentprojection cancel only returnedexactflow, no QRsecret parsing or inferredapproval; newerreplacement unaffected. Cost if wrong: strandedresume or wrongselectedtransaction misleadingUI, focused selectedresume scan success/wrongtxcleanup route assertions required. No storage/wire/coreauthority expansion.

### C-20

Ruling: C3 missing CMakeLists/strings hunks require no no-op edits: unchanged CMakeLists alreadylinks dwmapi.lib and Manifest literalChatProject has no missingresource reference. Specfunctionalrequirements satisfied, userAGENTS surgicalchanges overrides template mechanicalfilelist expectation — costifwrong undiscoveredlink/resourceissue, actualbothReleasebuilds+reviewerfocusedexistingfilecheck substantiate scopeexception. Preserve originalreviewfinding forfinalreview.

### C-21

Ruling: C3fix1 approved conservative disclosure-risk latch BEFORE Eapproval or Nmatching-code consent begins, live model-listening modal and neutral cancellation-request wording. _abandon returnsoutcome; serialized exactpublicflow-only retryhandle retainedonfailure, no QR/secretrevival, failurekeepsroute/errorvisible andblocksfreshpair untilresolved; ticket/coreexactflow guards protectreplacement. Costifwrong misleadingwarning or orphanretry/cancellation ofreplacement; delayedEapproval/Nconsentmodal + actualroutefailure/retry testsrequired. 200%/keyboard namedgap tested before anylayoutchange; measuredDevicesdefect onlyminimaldirectfix afterevidence.

### C-22

Ruling: Approve only measured pair-close/pending-discard dialog content scrolling, Devices SingleChildScrollView matches existing revoke dialog. No speculativedeviceListTile rewrite; verify fulltitle/subtitle/tilebounds beforeanylistchange. Costifwrong inaccessiblewarning/control atlargefont; exactsameviewport200% overflow0+keyboardreachability GREEN required.

### C-23

Ruling: C4reuse ownedController47000 listener bounded /records WebSocket sharedsignedpacketinmemory, existingtokio-tungstenite/serde_json nonewdeps. Approve runtime.rs accountslotArcsharing and puppet/account.rs+records.rs fordirectscope, sameAccountHandle localUIcommands/nativeappprovider; node staysrelay/M2, no nodefakeprotectorneeded. Installimmutablelocalservice/profile atomicallyvsaccountopen BEFOREhookready; late/already-openreject, failedopen/retry retainsfixtureconfig andNEVERfallsbackproductionDHT. Cloneactorhandle/releasemutexbeforeawait, no strongRunningCore capture inownedtask/shutdowncycle. Exactliteralownedloopbackport/path/noDNSquerypublic; boundedframes/requests/timeouts andwholeSignedPacket<=1000B perexistingrecordcontract, actualsignature/key/highwaterverification remainscore. SystemClock adequate, fakeClocknotneeded. Why: acceptedofflinefixturealwaysRecordUnavailable cannotsharepublication acrossapps; existingownedchannel avoidsnewbackendframework/dependency. Costifwrong: fixturecouldescapeisolation/loseauthority/shutdown ormaskactivation; URL/lateinstall/Release/realmultippub/redactiontests+actualAndroidscenario required.

### C-24

Ruling: C4twoownedappparticipants lifecycle: initialA+Bcreation/contact/safety, switchBtofreshownedstore forcandidate+NewDisplayspair/restart, anotherfreshstore forExistingDisplays, revokefirstBinstallation, freshstore recoverinitialAphrase incontrollerRAMonly. Storespreserved/noreset/deletion. Approve AndroidDEBUGGABLE chat_data_dir extra restrictedcanonical filesDir/e2e dedicatedownedprefix (rejectescape/symlink/arbitraryproductionstore), publicpath/nameonly. Restartcallbacks onlyownedapp/AVD, Windowsalreadyperlaunchdir, M2defaultanonymousrestartunchanged. Why: twoactualnativeproviders satisfyplanwithoutnodefakeprotector oractiveaccountoverwrite. Costifwrong: wrongstore/ownerrestart coulddestroyuserstate orfakepersistence; ownedresourcecleanup+publicidentity/installation/endpoint stable,newinstance andactualmembership/recovery assertions required. No physical/public/signed/registry scope expansion.

### C-25

Ruling: Allow controller-owned sharedrecordpublication pause toexercise realPendingImport + normalaccountcommandrefusal, no newcorecrash/approval/privatehook. Lookup keepsactualsignedoldrecord/membership; receiptalone cannotactivate, unpause usesexistingactorpublication/explicitrefresh/freshresume contract asapplicable. Why: requiredpending-importisolation shouldbeexercisedthroughactualnormalactor ratherthanforcedteststate. Costifwrong: fakeactivation or misleadingmembership proof, sameaccountcommand refusal/publicphaseassertions andnormalverifiedactivation pathrequired.

### C-26

Ruling: Approve CIexplicitnativewrite/read/locked/missing phases+newaccount/privacyintegration andsmalle2e.yml M3afterM2steps, bothexistingOSprofiles asapplicable. KeepM2commands/assertions/summary; distinctM3exitstatus/diagnostic/sanitizedsummary naming, norawlogupload/overwriting/mislabeling. iOSroot mustabsoluteownedSIMULATORAPPCONTAINER path fromactualnativeStorageRoot/simctl, nothostRUNNER_TEMP; sameinstalledappdata acrossfreshphaseprocesses, narrowtest-onlynative-run-labelproposallowedifneeded. No no-opchat_node mainchange ifrelay/M2unchanged. Why: existingloops nowinclude phase-requiringstoragefile, newM3scenario needs actualCIinvocation. Costifwrong: inaccessibledevicepath/falsepersistence or maskedM2/M3failure; CItests/staticchecks+localavailableOSexecutionsrequired, Appleconfigurationnotexecutionproof.

### C-27

Ruling: Concrete _Run._interrupt orderbug permits minimalownedemulator deferraluntilOwnedResources reverse-dependency activity/reversecallbacks finish, preserving boundedactivecommand/node interruption+remainingprocesscleanup/primaryerrorprecedence. Onlyacquiredownedhandles, no arbitraryPID/executableheuristics/unownedkills. Why: interruptcurrentlystopsallprocessesincludingAVD beforeownedcallbacks, actual6secondaryadbnodeviceerrors. Costifwrong: hangingacquisition or leaked/incorrectprocesscleanup; behavioralacquisition-cancel andpostactivity/reverse registration order regressions required beforefix. Bootstrapproblem remainsseparateinvestigation, no bundledguessfix.

### C-28

Ruling: Fix production Android ownership only using libc::flock(Fileborrowedfd,LOCK_EX|LOCK_NB), WouldBlock→WriterBusy, allothererrorsStorageLocked, existingFile lifetime/close release andpersistentAndroidpolicy. Approve Android-target-only libc='=0.2.189', alreadyCargo.lockversion/checksum(no crate/versionupgrade), Cargo.lockedgeonly. Rootreadactualownership.rs plusindependentlybrowsed official Rust1.92 unix.rs1277–1330 showingAndroidexcludedandUnsupported; AOSP bionic sys/file.h declaresflock. Sources https://github.com/rust-lang/rust/blob/1.92.0/library/std/src/sys/fs/unix.rs#L1277-L1330 and https://raw.githubusercontent.com/aosp-mirror/platform_bionic/master/libc/include/sys/file.h . Why: stdtry_lockmapsUnsupported toStorageLocked beforeactoropen; priorA3AndroidKeystoreproofdidNOTexerciseRustStorewriterlock, actualC4surfacedgap. Costifwrong: concurrentwriter/keyaccess/lifetimefailure; actualnativeaccountRED→GREEN+busy/releaseproof+candidate/activeprocessrestart required, minimaldocumentedunsafe/no unlockedfallback/Windows+iOSstdpathunchanged. AnynewgatedprobeAPI/genneedsconcreteproposal, preferexistingpuppetscope. No userpublic/physicalscope expansion.

### C-29

Ruling: Approve scratch nativeRustprobe linkingfreshproductionchat_core samepinnedlockeddeps/NDK, no newpublichelper/FRB. Dedicatedowned /data/local/tmp/chatproject-writer-probe-<validatedrunid> onframeworkownedchat_e2e_1 only, no keys/accounts; protectortripwire checksnosecretaccessbeforeownershipfailure. Requireseparateopenbusy/drop/reopen ANDcrossprocesscontenderdenial, preserveexactsource/compiler/rlibprovenance/commands/exitassertions inreport+trackedresultsappendixbeforecleanup. Why: Androidlockproductionpathneedsactualtargetproofwithoutnewsecret/testAPI. Costifwrong: wrongartifactorplatformproof/unsafeownershipscope; actualprobe+nativeaccountREDGREEN+M3restarts required. Scratchsourceisvalidationartifact, notnewgenerictestframework.

### C-30

Ruling: Approve direct core/chat_core/tests/puppet_account.rs reusingexistingtests/support/account_store.rs, no newprodAPI/commonfixture. Separatebinary andsequentialfixed47000 casesavoidexistingpuppetparallelport conflict. ProveactualfailedStoreopenWriterBusy→release→normalopenretainslocalservice viaactualsubsequentsignedrecordrequest, sameactorhook/publichandle snapshot, and validFIRSTlocalconfig AFTERalreadyopen accountrejectswithoutprofilemutation (notmerelysecondduplicateconfig). Why: approvedsharedprofile/slotchanges loadbearing andactualAndroidhappyrestartaloneinsufficient. Costifwrong: hiddenpublicfallback/lateprofileoverwrite/duplicateactors; focusedbehavioralREDGREEN+existingpuppet+Release testsrequired. Publicdeterministicfixturesonly/no generatedsecretlogs, nativebuild/timingtestserialization retained.

### C-31

Ruling: Addnarrow puppet account_local_route PUBLICownernetwork_info ticket only→existingLocal-onlyaccount_target_for_test. QR/PSK/wire/proof/authorityunchanged; actualscannerendpoint/signatures/pins remaincore, Releasegated/no publicroutingfallback. Why: isolatedLocalprofile hasno publicaddresslookup, prioracceptedpairfixtures explicitlyprimepublicroute. Costifwrong: misboundtarget/localprofileescape, actualM3routepin/Release/isolationcoverage required. AuthFailed diagnosticseparate: existingpublicexpiresAt+ownedadbdate numeric only, no QRdecode/rawargslogs; reportexactdelta+branchbeforeanyclockpolicychange, no expiryrelaxation/timeoutincrease/repeatuntilpass.

### C-32

Ruling: Approve narrow hook-ownedSecretJson RAII inpuppet/account.rs: guardrecursiveStringvalues beforependingadmission/reject/await, borrowedtypeddeserialization plusguardedmnemonic/answers/QR cancellation/error, resultValue dropcleanup andzeroizingserializationscratch inpuppetsend. No dep/API/FRB/privatehook/genericframework; no additionalsecretClone/Debug/rawserdeerror formatting, preservecode-onlyredaction. Why: hookJSON adds app-ownedsecretcopies beyondalreadyzeroizingcoreDTOs. Costifwrong: cancel/errorbufferretention or falseerasurepromise; practicalPUBLICfixture admission/cancel/errorcoverage andcodeaudit required, opaqueTungstenite/serdeinternalcopies+Dartimmutablestrings remainexplicitresidual, no freedRAMinspection/fullRAMwipe claim.

### C-33

Ruling: Harnesspauseflow unpause→normalexplicitErefresh→Observedacceptedrecord exactNinstallation+transportmembership (notEActive/localfallbackalone)→normalexplicitNrefresh→actualNActive. Why: establishedB6PendingImport manualrefresh path afterephemeralchannelended; expectingautomaticrefresh contradictedapprovedpolicy. Costifwrong: falseactivation/membership or resume bypass; keepactualnormal-command refusalbeforeunpause andcoreverifiedNActive afterexplicitrefresh, no activationhook/corepolicy/clockchange.

### C-34

Task4 attempt5 reached both actual pairing directions, PendingImport normal-command denial then exactObservedmembership and NActive, plus stableCandidate/Active processrestarts; revocation-observation timedout. Rootread manager1139–1145/1566: normalownpeerRefresh performs publicationlookup, publishACK deliberatelynotobservation, periodic60s exceedsfixture45s. Ruling: invoke normalownpeerRefresh after create/revoke/recover before observed-state assertions, not timerphase/ACK; no corepolicy/timeoutchange or activationhook. Why: actualverifiedobservation is assertioncontract. Costifwrong: false revoke/recovery completion; actualsignedlookup/membership and NActive/Revoked assertions required, failedattempt5 retained. Scenarioflowfixtures9/9 reported, notcrypto proof.

### C-35

Ruling: C4existingpuppet::start/run noweach8arguments aftersameactoraccountslot addition mayaddONLYfunction-scoped documented allow(clippy::too_many_arguments). Rootreadactualsignatures/callpass; ownership/borrowedlifetimes remainexplicit, newcontextstruct solelyforlint wouldaddunrequiredstructuralchurn. Why: sameownedslot isrequiredandseventhlimitstylistic. Costifwrong: lifecycleargumentmaintainability burden; narrowexceptions/exactstrictlint+independentreview required, nomodule/globalallow. Nativeaccounttestdispose→awaitmodel.close cleanupamendment usesplannedcoveringnativecheck; no M3runtimefeaturechangeclaimed.

### C-36

Ruling: C4 Androiddebugownedpathguard currentlycanonicalizesbothfilesDir/e2e boundaryandrequestedpath, permittinge2eleafsymlinkoutside. RootreadactualMainActivity50–57 confirmsconcretegap. Approve boundaryFile(filesDir.canonicalFile,e2e) WITHOUTresolvinge2eleaf, requestedabsolute/canonicalmustbeneathliteralboundary+separator. ExtractONLYinternalownedTestDataDir(File,String) insameMainActivity, existingAccountKeyStoreTest nativeinstrumentationfreshscratchbase validchild/childsymlinkescape/e2erootsymlinkescape+exactboundary/relative rejection; no deps/FRB/publicAPI/productionstoremutation/genericfixture. Why: ownedteststorecontainment mustnotacceptescapedroot. Costifwrong: debugscenario couldselectoutsideownedstore; actualtargetREDGREEN+finalAndroidbuild required. EarlierM3nativePASS remainslifecycleevidence; nofullrepeatrequiredforguardonlyamendment unlessnewbehavioralconcern.

### C-37

Task4 Androidnativeaccount/writePASS butreadFAILreceiptmissing ataccount_storage_test115 BEFOREidentitycomparison; originalreadlogretained. Workeractualownedadb saysunknownpackage afterread, notdecryptfailure. RootindependentlyreadinstalledFlutter3.44.6 test.dart300–305 defaultsuninstalltrue and489passesbool; integration_test_device.kill147–150 uninstallsafterprocess, android_deviceinstall uses-r. Ruling: add--no-uninstall ALLAndroid/iOSstoragephasecommands(inclwrite/finalread); precedingiOSaccount/privacy retainsinstalledappforsimctlget_app_container. Why: crossprocesspersistencefixturemustpreserveappdata, currentdefaultdestroysit. Costifwrong: falsepersistenceproof/staleappdatafixture; freshownedroot+actualfivefreshprocesssameidentity/newinstanceassertions andCIregressioncheckrequired. No productionprovider/fixture/APIchange, no substitutionwithpriorM3PASS/timeout/skip. SourcefreezenowCLI/CIonlyamended; finalgeneration/Releaseprovenanceafterward.

### C-38

Ruling: C4ordinaryAndroidRelease smoke1792411550997007 readinessdeadlinefailedwhileownedAVDsystemBluetoothcrashdialogoccludedapp; workerretainsUIXMLfocusedApplicationErrorcom.google.android.bluetooth/dumpsys6crashrestarts, cleanupempty, appalive/noFatalreported, MLKitregistrarNoSuchMethodExceptionwarningsseparateresidualnotcamera proof. Approve ONLYverifiedframeworkchat_e2e_1/emulator5554 svcbluetoothdisable + amforce-stopcom.google.android.bluetooth toclearobservedcrashedcomponent, verifydialogremoved, samepreservedAPKsmokeONCE/samedeadline/config. Why: owneddisposabletestOSdialog preventsreadinessobservation. Costifwrong: modifiedtestAVDsystemenvironmentcouldmaskappissue; requirebefore/afterpublicOSreceipts+sameAPKhash and actualwelcome assertion, preservefirstfailure/MLKitwarning, nohostUI/userAVD/phone/source/timeoutchange. Thisislocalfixtureenvironmentrepair, no camera/OSpreview/publictrafficproof. Rootactualproofreadpendingexactpaths.

### C-39

Ruling: C4ownedBluetoothrepaircommandsboth0butexistingdialogremained; rootreadactualreceipt verifieschat_e2e_1ownership/OFF/6crashes/currentFocusBluetoothApplicationErrorafterforce-stop. ApproveONEadbinputtap onONLYfreshXMLexactBluetoothApplicationError+uniqueenabledclickableandroid:id/aerr_close Closeapp, centerderivedfreshbounds, verifiedemulator5554ownedchat_e2e_1; verifydialogabsentBEFOREsameAPKsmoke. Why: Androidalreadyshownsystemerrorwindowoutlivesserviceprocess. Costifwrong: wrongdialogactioncouldaltertestOSstate; freshmatching/beforeafterreceipt/one-tapcap required, nohost/userAVD/phoneinput/no blindretry or source/deadlinechange. Ifmatchfailsortapdoesnotclear, stopinputandreconcile. Workeractualtap/proofpending; firstsmokefailurepreserved.

### C-40

Ruling: C4actualmergedR8oldfirebase-components16.1.0class-onlykeep andusage.txt removespublicdefaultconstructorall3warnedregistrars; pinnedanalyzeImage directlyInputImage.fromFilePath→BarcodeScanningfactory withoutcamera. ApproveONLY2files: existingintegration_test/release_contract_test.dart Android-onlyPUBLICQRQrPainterwhitemargin/tempPNG/MobileScannerController(autoStart:false).analyzeImage(qrCode)/EXPLICITexactpublicpayload BEFOREexistingnonce/finallydispose+deleteownfixture; FIRSTactualAOTbehavioralREDoldrulespreserveAPK/config/usage/mapping. THENapp/android/app/proguard-rules.pro appendofficialComponentRegistrar noargconstructorkeep (-keep class * implements com.google.firebase.components.ComponentRegistrar { void <init>(); }) onlyifREDconfirms. Why: potentialrequiredReleaseQRfunctionalitycannotbeexcusedbywelcomePASSorphysicalcameragate, officialFirebasePR6044matchesconcreteconstructorremoval. Costifwrong: testimagefixturecouldmisdiagnoseoradditionalconstructorkeepincreaseAPKslightly; actualsamefixtureREDGREEN/nativefactory/pathproof+scans+reviewrequired, no blanketMLKitkeep/dependencyupgrade/R8disable/API/camerapermission. CoveraffectedDartanalyze+AndroidGREENcontract/ordinarynewAPKallABIscans/welcome+Windowsconditionalcontractcompile/runtime, nosourcerelevantbroadsuite rerun. Refreshfreeze/artifacts/results andlabelallpre-keepAPKproofsourceearlier; actualopticalcamera/OSprivacyremainsunexecuted.

### C-41

WholeM3review NOTREADY:0Critical/6Important(I1remoteconflictpermanent,I2ownconflictUI,I3terminalcancelroute,I4handshakeevidencedrop,I5auth/cacheUIconsumer,I6recoverywarning)/2Minor(M1expirycode,M2lifetimependingquota). RootreadENTIREreport inclallruling/deferredtriage+10declined-tojudgegroups. ONEcombinedfreshfixwaveprepared at whole-m3-fix-brief.md withVERBATIMcompleteI1-I6/M1-M2; no sourceimplementationyet. Ruling: permitnarrowper-peerconflictconfirm/publicsnapshot+normalcontactauthconsumer, preserveforeignvsownauthority/floors/tombstones/freshproof/terminalflowisolation andbeforeconsentwarning; detailedrationaleandcostsverbatimfollow.

### C-42

Ruling: I1 is a genuine missing remote-contact recovery consumer of §5.1/§5.2, and may add a narrow per-peer public conflict projection and explicit exact-current-record confirmation command through core/FRB/model/contact UI. Foreign record acceptance is local trust in the peer's signed membership only, never own-account publication or local membership authority. Re-read current backend evidence, preserve durable floors/conflicts/tombstones, close old grants and require fresh proof. Why: a stable PeerId cannot become permanently unreachable after ordinary recovery. Cost if wrong: incorrect per-peer authority or rollback acceptance; exact stale/future/floor/revoked-endpoint/restart regressions and scoped review are mandatory.

### C-43

Ruling: I2 uses the existing own confirmation command and actual conflict projection; a displayed competing list/generation/hash must remain distinct from accepted/pending state. A writer absent from that list may see recovery guidance but cannot regain authority through confirm. Why: existing core authority controls are correct, their product consumer is missing. Cost if wrong: stale-list consent or resurrection of an excluded writer; exact-hash stale-backend and excluded-local tests required.

### C-44

Ruling: I3 terminal cancellation of an exact already-ended flow may return successful abandonment without removing a different replacement or changing durable disclosure/import/history. Keep genuine live-operation failures visible/retryable and negative-SAS secret cleanup. Why: terminal absence is already successful ephemeral cleanup, unlike storage/transport failure. Cost if wrong: cancellation of replacement or misleading rollback assurance; actual core response plus UI terminal/remote-close/lifecycle/delayed replacement regressions required.

### C-45

Ruling: I4 must carry fully identity-signature/canonical-verified public record evidence out of the handshake into the existing durable observation/floor/conflict policy even when the handshake is denied. It grants no rights before fresh valid installation proof. Invalid/unsigned/wrong-peer data never updates floors; noncurrent but otherwise verified evidence follows existing refresh policy rather than being silently erased by a generic error. Why: known signed authority changes forbid old-cache grants under §5.2. Cost if wrong: forged cache poisoning or stale-grant continuation; actual-handshake changed-record/unavailable-backend and invalid-evidence tests required, persistence failure must restrict activity.

### C-46

Ruling: I5 adds a deliberate normal contact-route account proof action and coherent public authorization/cache status, using the existing single event stream and snapshot ownership. M2 metrics/readiness remain independent; safety verification is not device proof; restart/disconnect/expiry/failure cannot retain ready and a later peer's state cannot overwrite selected contact. Why: B3->C1/C2 consumer is explicitly required (B-16). Cost if wrong: false-ready/cache concealment or event ordering regression; real command/bridge/model/widget flow and stale-event/contact-switch checks required. A narrow public DTO/API extension is allowed where actual authoritative snapshot state is needed; do not build a new event stream or message/chat feature.

### C-47

Ruling: I6 and M1 fit the same wave: truthful shared-signing-key warning before recovery, and stable invalid/expiry error mapping at public boundaries after adequate validation, with no input echo. Why: the decision must reflect retained IdentityKey copies and an expired signed invite is not retryable. Cost if wrong: misleading consent or distinguishable invalid evidence; production-path core/UI assertions required.

### C-48

Ruling: M2 separates unfinished-intent admission (non-Observed Existing transactions + pending publications <=32, reserving the later publication slot for a fresh transfer) from retained exact-resume/disclosure provenance. Retain every existing tx/payload/receipt/pin unchanged, with a separate total Existing-history capacity256 AND the unchanged4MiB snapshot bound. Refuse ONLY a new transaction at that history capacity with a distinct truthful PairHistoryFull public error/Korean guidance; existing resume, auth, refresh and legitimate publication must not consume that history-admission check. Keep current private encoding layout; decoder validates the separate total256 and unfinished32 budgets, canonical/size/pin/duplicate protections, accepting earlier valid snapshots. Why: Observed E publication does not establish N activation, so dropping/compacting payloads would violate pending exact resume and disclosure warnings; a finite256 record provenance budget is explicit and independent of the intended32 unfinished operations. Cost if wrong: completed encrypted contact snapshots consume the4MiB bound before256, and installations eventually hit a documented finite history capacity; this is a visible bounded-retention product limit, not erasure/indefinite-capacity promise. Required tests exceed31 successful observed/revoke cycles, preserve exact old pending/resume/disclosure, enforce32 unfinished/256 history/4MiB distinctly and verify old-snapshot decode; no protocol/ack/automatic-pruning change. Worker proposed and controller approved this smallest bounded alternative over new N-activation receipt/terminal compaction protocol.

### C-49

Ruling: I5 native consumer proof may add/select a minimal authentication-only path in the existing owned M3 scenario/runner, reusing existing account_local_connect_account and account_local_route and the same native app AccountHandle. Existing normal contact-route widget->real AccountModel/client and generated bridge checks compose with actual two-app signed proof/status. Reuse existing local signed-record availability control, or add only a narrow owned record-service lookup-unavailable toggle if needed, with M2 default unchanged. Why: prior native M3 scenario invoked only negative PendingImport connect and cannot prove the positive app auth/cache consumer. Cost if wrong: new fixture selection/availability behavior could mask backend routing or false readiness; require real signatures/fresh proof/publicprojection assertions, fresh vs cached status, actual failure/disconnect clearance, fixture failclosed/no publicfallback and ownedcleanup. No new app authority/reset/approval/privategetter hook or unrelated full scenario replay; serialize after core timing test and generator/build ownership.

### C-50

Ruling: After M2 retained-history33-cycle trial completed allceremonies but failed existing10s lifecycle restore (actual371.32s retained), permit only replacing redundant Payload::decode calls used solely to recover the already validated cached PairTransaction.tx in bytes/envelope/duplicate checks with t.tx. Root read decode: full Payload::decode + p.tx==tx/pin/receipt/stage/canonical checks precede constructing tx; production constructors assign matching exacttx/payload and no later tx mutation found. Keep full input payload/cert/key/receipt/canonical verification ONCE at decode, compute actual payload hash for envelopes and preserve all bounds/timeouts/resume bytes. Why: quadratic repeated certificate verification in preceding-entry duplicate checks makes the newly correct bounded history unusable at restart. Cost if wrong: cachedtx invariant mismatch could hide malformed transaction; retain/add mismatch/duplicate rejection regressions and same real33-cycle/restart/oldresume proof at unchanged10s deadline. No broad transaction refactor or validation removal.

### C-51

Ruling: NativeI5run1792417477281626 exposed existing protocol whitelist refusing actual core serde kinds account_auth_changed/account_cache_used (protocol.dart121). Root read exact whitelist and CoreEvent snake_case variants, controller pre-write parser and _publicEvent default redaction. Admit ONLY those2 existing diagnostic event kinds through unchanged instance/seq/event delivery, keep their complete payload redacted before log, preserve strict unknown-kind/top-level rejection and no authority inference from events. Add real WebSocket public-marker delivery+redaction and unknown-kind negative regressions, then rerun ONLY auth scenario. Why: actual positive native account authentication emits these already-designed events; refusing them aborts harness rather than testing I5. Cost if wrong: log leakage or stale/malformed-event acceptance; retain I1 logger policy and sequence/identity checks, cover nested markers and unchanged unknown denial, sameapp/corerouting/proof stays unchanged. This is tools-only; amend350-source provenance explicitly with only tool/test differences, no relabeling changed app inputs. Original native attempt failed and cleanup ALSO failed Android2force-stop30s; preserve ownership's uncertain activity record. Actual subsequent adb/emulator/qemu/chat_node inventories empty, processes/reverses empty; never signal stalePID or claim complete cleanupexit0. Independent serial Release build may proceed with unchanged app/core sources while tool correction runs.
