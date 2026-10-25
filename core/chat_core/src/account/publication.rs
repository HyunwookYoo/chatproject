//! Durable publication policy. All effects are committed by the account actor.
use super::{
    Generation, IdentityRecord, InstallationId, Lookup, PeerId, PkarrBackend, PublishIntent,
    RecordBackend, RecordDevice, RevocationNotifier, RevokeIntent,
    clock::{Clock, SystemClock},
    record::parse_stored_record,
    record_hash, record_packet,
    storage::schema::LocalAccount,
    wire,
};
use crate::{CoreError, ErrorCode};
use minicbor::{Decoder, Encoder, data::Type};
use pkarr::SignedPacket;
use std::{cmp::Ordering, sync::Arc};

pub struct AccountServices {
    pub(crate) backend: Option<Arc<dyn RecordBackend>>,
    pub(crate) clock: Arc<dyn Clock>,
    pub(crate) notifier: Option<Arc<dyn RevocationNotifier>>,
}
impl AccountServices {
    /// Local bridge/storage fixture only. It never constructs a production backend.
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub fn offline_for_test() -> Self {
        struct Offline;
        impl RecordBackend for Offline {
            fn lookup(&self, _: PeerId) -> super::RecordFuture<'_, Lookup> {
                Box::pin(async { Err(CoreError::new(crate::ErrorCode::RecordUnavailable, "Offline fixture")) })
            }
            fn publish<'a>(&'a self, _: &'a SignedPacket) -> super::RecordFuture<'a, ()> {
                Box::pin(async { Err(CoreError::new(crate::ErrorCode::RecordUnavailable, "Offline fixture")) })
            }
        }
        Self::new(Arc::new(Offline), Arc::new(super::clock::FakeClock::new(SystemClock::default().unix_seconds(), 0)))
    }

    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub fn new(backend: Arc<dyn RecordBackend>, clock: Arc<dyn Clock>) -> Self {
        Self {
            backend: Some(backend),
            clock,
            notifier: None,
        }
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub fn with_notifier(mut self, notifier: Arc<dyn RevocationNotifier>) -> Self {
        self.notifier = Some(notifier);
        self
    }
    pub(crate) fn production() -> Self {
        Self {
            backend: None,
            clock: Arc::new(SystemClock::default()),
            notifier: None,
        }
    }
    pub(crate) fn backend(&mut self) -> Result<Arc<dyn RecordBackend>, CoreError> {
        if self.backend.is_none() {
            self.backend = Some(Arc::new(PkarrBackend::new()?));
        }
        Ok(self.backend.as_ref().unwrap().clone())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PublicationState {
    LocalBlocked,
    Pending,
    Observed,
    Conflict,
}
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PublicationView {
    pub state: PublicationState,
    pub generation: Option<Generation>,
    pub observed_seq: u64,
    pub allocated_seq: u64,
    pub accepted_hash: Option<[u8; 32]>,
    pub accepted: Option<IdentityRecord>,
    pub conflict_hash: Option<[u8; 32]>,
    pub conflict: Option<IdentityRecord>,
    pub tombstones: Vec<RevokeIntent>,
    pub pending_count: usize,
    pub pending_notifications: Vec<RevokeIntent>,
}
impl Default for PublicationView {
    fn default() -> Self {
        Self {
            state: PublicationState::LocalBlocked,
            generation: None,
            observed_seq: 0,
            allocated_seq: 0,
            accepted_hash: None,
            accepted: None,
            conflict_hash: None,
            conflict: None,
            tombstones: vec![],
            pending_count: 0,
            pending_notifications: vec![],
        }
    }
}

#[derive(Clone)]
pub(crate) struct SignedRecord {
    pub(crate) packet: SignedPacket,
    pub(crate) record: IdentityRecord,
    pub(crate) hash: [u8; 32],
    pub(crate) seq: u64,
}
impl SignedRecord {
    pub(crate) fn new(packet: SignedPacket, peer: PeerId) -> Result<Self, CoreError> {
        let record = parse_stored_record(&packet, peer)?;
        let seq = u64::from_be_bytes(packet.as_bytes()[96..104].try_into().unwrap());
        Ok(Self {
            hash: record_hash(&packet),
            packet,
            record,
            seq,
        })
    }
    pub(crate) fn decode(bytes: &[u8], peer: PeerId) -> Result<Self, CoreError> {
        if !(104..=1000).contains(&bytes.len()) {
            return Err(wire::invalid());
        }
        // pkarr deserialize prepends a local last_seen timestamp and does not
        // verify signatures; strict verification is mandatory immediately after.
        let mut serialized = vec![0; 8];
        serialized.extend_from_slice(bytes);
        Self::new(
            SignedPacket::deserialize(&serialized).map_err(|_| wire::invalid())?,
            peer,
        )
    }
    pub(crate) fn current(&self, now: u64) -> bool {
        now.checked_add(300)
            .and_then(|n| n.checked_mul(1_000_000))
            .is_some_and(|max| self.seq <= max && self.record.issued_at <= now + 300)
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PacketOperation {
    First,
    Replacement,
    Renew,
    Revoke,
    Resolve,
    Add,
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PacketStatus {
    Ready,
    AwaitingNotification,
    Held,
}
pub(crate) type Recipient = (InstallationId, [u8; 32]);
#[derive(Clone)]
pub(crate) struct PacketIntent {
    pub(crate) signed: SignedRecord,
    pub(crate) base_hash: Option<[u8; 32]>,
    pub(crate) operation: PacketOperation,
    pub(crate) status: PacketStatus,
    pub(crate) revoke_nonce: Option<[u8; 16]>,
    pub(crate) recipients: Option<Vec<Recipient>>,
}
#[derive(Clone)]
pub(crate) struct RecordState {
    pub(crate) generation: Generation,
    pub(crate) accepted: Option<SignedRecord>,
    pub(crate) conflict: Option<SignedRecord>,
    pub(crate) observed_seq: u64,
    pub(crate) allocated_seq: u64,
    pub(crate) tombstones: Vec<RevokeIntent>,
    pub(crate) pending: Vec<PacketIntent>,
    pub(crate) state: PublicationState,
}
struct OwnRecordComparison {
    retained_before: bool,
    accepted_present: bool,
    accepted_hash_equal: bool,
    seq_vs_observed_at_hold: Ordering,
    seq_vs_allocated_at_hold: Ordering,
    generation_equal: bool,
    devices_equal_to_accepted: bool,
    pending_hash_match: bool,
    pending_ready_hash_match: bool,
    time_valid: bool,
    expired: bool,
}
#[derive(Clone, Copy)]
pub(crate) enum Action {
    Refresh,
    Revoke(InstallationId),
    Confirm([u8; 32]),
}
fn blocked() -> CoreError {
    CoreError::new(
        ErrorCode::AccountRestricted,
        "Account publication requires a valid clock and current membership",
    )
}
fn conflict() -> CoreError {
    CoreError::new(
        ErrorCode::RecordConflict,
        "Device list changed; confirm the current list",
    )
}
fn full() -> CoreError {
    CoreError::new(ErrorCode::StorageFull, "Account publication limit reached")
}

impl RecordState {
    pub(crate) fn stage_add(
        &mut self,
        local: &LocalAccount,
        cert: &super::InstallationCert,
        base_hash: [u8; 32],
        generation: Generation,
        now: u64,
    ) -> Result<[u8; 32], CoreError> {
        let base = self.accepted.as_ref().ok_or_else(blocked)?;
        if self.conflict.is_some()
            || self.state == PublicationState::Conflict
            || !base.current(now)
            || base.record.is_expired(now)
            || !self.owns(&base.record, local)
            || self.removed(local)
            || base.record.generation != generation
        {
            return Err(blocked());
        }
        if base.record.devices.iter().any(|d| {
            d.installation_id == cert.installation_id
                && d.transport_public_key == cert.transport_public_key
                && d.kind == cert.kind
        }) {
            return Ok(base.hash);
        }
        if base.hash != base_hash || !self.pending.is_empty() {
            return Err(conflict());
        }
        if base.record.devices.len() >= 5 {
            return Err(CoreError::new(
                ErrorCode::DeviceLimit,
                "Device limit reached",
            ));
        }
        if local.pairs.unfinished() + self.pending.len() >= 32 {
            return Err(full());
        }
        let mut record = base.record.clone();
        record.devices.push(super::RecordDevice {
            installation_id: cert.installation_id,
            transport_public_key: cert.transport_public_key,
            kind: cert.kind,
        });
        record.devices.sort_by_key(|d| d.installation_id);
        self.allocate(local, record, PacketOperation::Add, None, now)?;
        Ok(self.pending.last().unwrap().signed.hash)
    }
    pub(crate) fn new() -> Result<Self, CoreError> {
        let mut generation = [0; 16];
        getrandom::fill(&mut generation).map_err(|_| wire::invalid())?;
        Ok(Self {
            generation: Generation(generation),
            accepted: None,
            conflict: None,
            observed_seq: 0,
            allocated_seq: 0,
            tombstones: vec![],
            pending: vec![],
            state: PublicationState::LocalBlocked,
        })
    }
    pub(crate) fn view(&self) -> PublicationView {
        PublicationView {
            state: self.state,
            generation: Some(self.generation),
            observed_seq: self.observed_seq,
            allocated_seq: self.allocated_seq,
            accepted_hash: self.accepted.as_ref().map(|r| r.hash),
            accepted: self.accepted.as_ref().map(|r| r.record.clone()),
            conflict_hash: self.conflict.as_ref().map(|r| r.hash),
            conflict: self.conflict.as_ref().map(|r| r.record.clone()),
            tombstones: self.tombstones.clone(),
            pending_count: self.pending.len(),
            pending_notifications: self
                .pending
                .iter()
                .filter(|p| p.status != PacketStatus::Ready)
                .filter_map(|p| {
                    self.tombstones
                        .iter()
                        .find(|t| Some(t.nonce) == p.revoke_nonce)
                        .cloned()
                })
                .collect(),
        }
    }
    pub(crate) fn removed(&self, local: &LocalAccount) -> bool {
        self.tombstones.iter().any(|t| {
            t.target_installation == local.device.installation_id()
                || t.target_transport == local.device.transport_public_key()
        })
    }
    pub(crate) fn can_publish(&self, record: &IdentityRecord, local: &LocalAccount) -> bool {
        self.owns(record, local)
            && !record.devices.iter().any(|d| {
                self.tombstones.iter().any(|t| {
                    t.target_installation == d.installation_id
                        || t.target_transport == d.transport_public_key
                })
            })
    }
    pub(crate) fn restrict_pending(&mut self, local: &LocalAccount) {
        let mut held = false;
        for index in 0..self.pending.len() {
            if !self.can_publish(&self.pending[index].signed.record, local) {
                self.pending[index].status = PacketStatus::Held;
                held = true;
            }
        }
        if held && self.state != PublicationState::Conflict {
            self.state = PublicationState::LocalBlocked;
        }
    }
    fn owns(&self, record: &IdentityRecord, local: &LocalAccount) -> bool {
        record.generation == self.generation
            && record.devices.iter().any(|d| {
                d.installation_id == local.device.installation_id()
                    && d.transport_public_key == local.device.transport_public_key()
            })
            && !self.removed(local)
    }
    fn hold(&mut self, found: SignedRecord, now: u64) -> CoreError {
        use std::io::Write;
        // Hold-entry facts: the future guard in reconcile may already have
        // raised observed_seq. This is not a pre-reconcile snapshot.
        let comparison = OwnRecordComparison {
            retained_before: self.conflict.is_some(),
            accepted_present: self.accepted.is_some(),
            accepted_hash_equal: self.accepted.as_ref().is_some_and(|a| a.hash == found.hash),
            seq_vs_observed_at_hold: found.seq.cmp(&self.observed_seq),
            seq_vs_allocated_at_hold: found.seq.cmp(&self.allocated_seq),
            generation_equal: found.record.generation == self.generation,
            devices_equal_to_accepted: self.accepted.as_ref().is_some_and(|a| a.record.devices == found.record.devices),
            pending_hash_match: self.pending.iter().any(|p| p.signed.hash == found.hash),
            pending_ready_hash_match: self.pending.iter().any(|p| p.signed.hash == found.hash && p.status == PacketStatus::Ready),
            time_valid: found.current(now),
            expired: found.record.is_expired(now),
        };
        let _ = writeln!(
            std::io::stderr().lock(),
            "own_publication hold retained_before={} accepted_present={} accepted_hash_equal={} seq_vs_observed_at_hold={:?} seq_vs_allocated_at_hold={:?} generation_equal={} devices_equal_to_accepted={} pending_hash_match={} pending_ready_hash_match={} time_valid={} expired={}",
            comparison.retained_before, comparison.accepted_present, comparison.accepted_hash_equal,
            comparison.seq_vs_observed_at_hold, comparison.seq_vs_allocated_at_hold,
            comparison.generation_equal, comparison.devices_equal_to_accepted,
            comparison.pending_hash_match, comparison.pending_ready_hash_match,
            comparison.time_valid, comparison.expired,
        );
        let _ = writeln!(
            std::io::stderr().lock(),
            "own_publication retained_context accepted_meets_observed_floor={} accepted_meets_allocated_floor={} retained_older_than_accepted={} retained_membership_equal_accepted={} accepted_current_unexpired={} accepted_generation_equal_local={} pending_empty={}",
            self.accepted.as_ref().is_some_and(|a| a.seq >= self.observed_seq),
            self.accepted.as_ref().is_some_and(|a| a.seq >= self.allocated_seq),
            self.conflict.as_ref().zip(self.accepted.as_ref()).is_some_and(|(c, a)| c.seq < a.seq),
            self.conflict.as_ref().zip(self.accepted.as_ref()).is_some_and(|(c, a)| {
                c.record.generation == a.record.generation && c.record.devices == a.record.devices
            }),
            self.accepted.as_ref().is_some_and(|a| a.current(now) && !a.record.is_expired(now)),
            self.accepted.as_ref().is_some_and(|a| a.record.generation == self.generation),
            self.pending.is_empty(),
        );
        self.observed_seq = self.observed_seq.max(found.seq);
        if self.conflict.as_ref().is_none_or(|c| found.seq >= c.seq) {
            self.conflict = Some(found);
        }
        for pending in &mut self.pending {
            pending.status = PacketStatus::Held;
        }
        self.state = PublicationState::Conflict;
        conflict()
    }
    fn sequence(&self, now: u64) -> Result<u64, CoreError> {
        let seq = now
            .checked_mul(1_000_000)
            .ok_or_else(blocked)?
            .max(self.observed_seq.checked_add(1).ok_or_else(blocked)?)
            .max(self.allocated_seq.checked_add(1).ok_or_else(blocked)?);
        let max = now
            .checked_add(300)
            .and_then(|n| n.checked_mul(1_000_000))
            .ok_or_else(blocked)?;
        if now == 0 || seq > i64::MAX as u64 || seq > max {
            return Err(blocked());
        }
        Ok(seq)
    }
    fn allocate(
        &mut self,
        local: &LocalAccount,
        mut record: IdentityRecord,
        operation: PacketOperation,
        revoke_nonce: Option<[u8; 16]>,
        now: u64,
    ) -> Result<(), CoreError> {
        if self.pending.len() + local.pairs.unfinished() >= 32 {
            return Err(full());
        }
        let seq = self.sequence(now)?;
        record.issued_at = now;
        record.expires_at = now
            .checked_add(super::RECORD_LIFETIME_SECONDS)
            .ok_or_else(blocked)?;
        record.devices.retain(|d| {
            !self.tombstones.iter().any(|t| {
                t.target_installation == d.installation_id
                    || t.target_transport == d.transport_public_key
            })
        });
        if !self.owns(&record, local) {
            return Err(blocked());
        }
        let signed = SignedRecord::new(
            record_packet(&local.keys, &record, seq)?,
            local.keys.peer_id(),
        )?;
        self.pending.push(PacketIntent {
            signed,
            base_hash: self.accepted.as_ref().map(|r| r.hash),
            operation,
            status: if revoke_nonce.is_some() {
                PacketStatus::AwaitingNotification
            } else {
                PacketStatus::Ready
            },
            revoke_nonce,
            recipients: None,
        });
        self.allocated_seq = seq;
        self.state = PublicationState::Pending;
        Ok(())
    }
    /// Stage only. Caller persists this entire state before delivering or publishing.
    pub(crate) fn reconcile(
        &mut self,
        lookup: Lookup,
        local: &LocalAccount,
        now: u64,
        action: Action,
    ) -> Result<(), CoreError> {
        if let Action::Revoke(target) = action
            && target == local.device.installation_id()
        {
            return Err(blocked());
        }
        let found = match lookup {
            Lookup::CompletedAbsent => None,
            Lookup::Found(packet) => Some(SignedRecord::new(packet, local.keys.peer_id())?),
        };
        let absent = found.is_none();
        if absent && matches!(action, Action::Revoke(_)) {
            return Err(blocked());
        }
        // Before its first allocation, confirmed recovery has no local list
        // authority to compete with. Future evidence still requires a fresh,
        // currently usable lookup before consuming that initial consent.
        let initial_replacement = local.intent == PublishIntent::Replacement
            && self.accepted.is_none()
            && self.pending.is_empty()
            && self.allocated_seq == 0
            && self.state == PublicationState::LocalBlocked;
        if let Some(found) = &found
            && !found.current(now)
        {
            self.observed_seq = self.observed_seq.max(found.seq);
            let known = self.accepted.as_ref().is_some_and(|a| a.hash == found.hash)
                || self.pending.iter().any(|p| p.signed.hash == found.hash);
            if !known {
                // Time policy cannot erase a competing signed base or release
                // its dependent work on a later absence/clock correction.
                self.hold(found.clone(), now);
                if initial_replacement {
                    self.state = PublicationState::LocalBlocked;
                }
            } else if self.conflict.is_none() && self.state != PublicationState::Conflict {
                // Local rollback alone must not invent a competing packet.
                self.state = PublicationState::LocalBlocked;
            }
            return Err(blocked());
        }
        if initial_replacement && self.conflict.is_some() {
            if !matches!(action, Action::Refresh)
                || found
                    .as_ref()
                    .is_none_or(|record| record.seq < self.observed_seq)
            {
                return Err(blocked());
            }
            self.conflict = None;
        }
        if let Action::Confirm(hash) = action {
            if self.conflict.as_ref().map(|r| r.hash) != Some(hash) {
                return Err(conflict());
            }
            let Some(found) = found else {
                return Err(conflict());
            };
            if found.hash != hash {
                return Err(self.hold(found, now));
            }
            if !self.owns(&found.record, local)
                || found.record.is_expired(now)
                || found.seq < self.observed_seq
            {
                return Err(blocked());
            }
            // Confirmation explicitly supersedes held packets, never tombstones.
            // Revoke notification must still complete before the rebased packet.
            let notification = self.pending.iter().find_map(|p| p.revoke_nonce);
            let recipients = self
                .pending
                .iter()
                .find(|p| p.revoke_nonce == notification)
                .and_then(|p| p.recipients.clone());
            let old = self.clone();
            self.accepted = Some(found.clone());
            self.conflict = None;
            self.pending.clear();
            if let Err(error) = self.allocate(
                local,
                found.record,
                PacketOperation::Resolve,
                notification,
                now,
            ) {
                *self = old;
                return Err(error);
            }
            self.pending.last_mut().unwrap().recipients = recipients;
            return Ok(());
        }
        if let Some(found) = found {
            let exact = self
                .pending
                .iter()
                .position(|p| p.signed.hash == found.hash && p.status == PacketStatus::Ready);
            if self.conflict.is_some() {
                return Err(self.hold(found, now));
            }
            if found.seq < self.observed_seq {
                return Err(self.hold(found, now));
            }
            if let Some(index) = exact {
                self.observed_seq = self.observed_seq.max(found.seq);
                self.accepted = Some(found);
                self.pending.remove(index);
                self.state = PublicationState::Observed;
            } else if self.accepted.as_ref().is_some_and(|a| a.hash == found.hash) {
                self.observed_seq = self.observed_seq.max(found.seq);
            } else if self.accepted.is_none()
                && self.pending.is_empty()
                && local.intent == PublishIntent::Replacement
            {
                self.observed_seq = self.observed_seq.max(found.seq);
                self.accepted = Some(found);
            } else {
                return Err(self.hold(found, now));
            }
        } else if self.conflict.is_some() {
            use std::io::Write;
            let _ = writeln!(std::io::stderr().lock(), "own_publication hold lookup_present=false retained_before=true");
            return Err(conflict());
        }

        if let Action::Revoke(target) = action {
            if self
                .tombstones
                .iter()
                .any(|t| t.target_installation == target)
            {
                return Ok(());
            }
            if self.tombstones.len() >= 256 || self.pending.len() >= 32 {
                return Err(full());
            }
            if !self.pending.is_empty() {
                return Err(conflict());
            }
            let base = self.accepted.as_ref().ok_or_else(blocked)?;
            if !base.current(now)
                || base.record.is_expired(now)
                || !self.owns(&base.record, local)
                || base.record.devices.len() < 2
            {
                return Err(blocked());
            }
            let target = base
                .record
                .devices
                .iter()
                .find(|d| d.installation_id == target)
                .ok_or_else(blocked)?;
            let revoke = RevokeIntent::sign(&local.keys, self.generation, base.hash, target, now)?;
            let record = base.record.clone();
            let nonce = revoke.nonce;
            self.tombstones.push(revoke);
            if let Err(error) =
                self.allocate(local, record, PacketOperation::Revoke, Some(nonce), now)
            {
                self.tombstones.pop();
                return Err(error);
            }
            return Ok(());
        }
        if !self.pending.is_empty() {
            if self.pending.iter().any(|p| p.status == PacketStatus::Held) {
                return Err(conflict());
            }
            if self.pending.iter().any(|p| !p.signed.current(now)) {
                self.state = PublicationState::LocalBlocked;
                return Err(blocked());
            }
            if self.pending.iter().any(|p| p.signed.record.is_expired(now)) {
                // A completed lookup has reconciled the same base (or absence).
                // Renew only the already-approved exact membership, at a new seq.
                let old = self.clone();
                let pending = std::mem::take(&mut self.pending);
                for intent in pending {
                    if let Err(error) = self.allocate(
                        local,
                        intent.signed.record,
                        intent.operation,
                        intent.revoke_nonce,
                        now,
                    ) {
                        *self = old;
                        return Err(error);
                    }
                    self.pending.last_mut().unwrap().recipients = intent.recipients;
                }
            }
            self.state = PublicationState::Pending;
            return Ok(());
        }
        let (record, operation) = if let Some(base) = &self.accepted {
            if self.owns(&base.record, local) {
                if !base.current(now) {
                    self.state = PublicationState::LocalBlocked;
                    return Err(blocked());
                }
                if !absent
                    && now < base.record.issued_at.saturating_add(23 * 3600)
                    && self.state == PublicationState::Observed
                {
                    return Ok(());
                }
                (base.record.clone(), PacketOperation::Renew)
            } else if local.intent == PublishIntent::Replacement && self.allocated_seq == 0 {
                (self.first_record(local), PacketOperation::Replacement)
            } else {
                self.state = PublicationState::LocalBlocked;
                return Err(blocked());
            }
        } else {
            (
                self.first_record(local),
                if local.intent == PublishIntent::Replacement {
                    PacketOperation::Replacement
                } else {
                    PacketOperation::First
                },
            )
        };
        let result = self.allocate(local, record, operation, None, now);
        if result.is_err() {
            self.state = PublicationState::LocalBlocked;
        }
        result
    }
    fn first_record(&self, local: &LocalAccount) -> IdentityRecord {
        IdentityRecord {
            generation: self.generation,
            issued_at: 0,
            expires_at: 1,
            devices: vec![RecordDevice {
                installation_id: local.device.installation_id(),
                transport_public_key: local.device.transport_public_key(),
                kind: local.cert.kind,
            }],
        }
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let mut e = Encoder::new(Vec::new());
        e.array(9)
            .unwrap()
            .u8(1)
            .unwrap()
            .bytes(&self.generation.0)
            .unwrap()
            .u8(self.state as u8)
            .unwrap()
            .u64(self.observed_seq)
            .unwrap()
            .u64(self.allocated_seq)
            .unwrap();
        for record in [&self.accepted, &self.conflict] {
            if let Some(record) = record {
                e.bytes(record.packet.as_bytes()).unwrap();
            } else {
                e.null().unwrap();
            }
        }
        e.array(self.tombstones.len() as u64).unwrap();
        for intent in &self.tombstones {
            e.bytes(&intent.to_bytes()).unwrap();
        }
        e.array(self.pending.len() as u64).unwrap();
        for intent in &self.pending {
            e.array(6)
                .unwrap()
                .bytes(intent.signed.packet.as_bytes())
                .unwrap();
            if let Some(hash) = intent.base_hash {
                e.bytes(&hash).unwrap();
            } else {
                e.null().unwrap();
            }
            e.u8(intent.operation as u8)
                .unwrap()
                .u8(intent.status as u8)
                .unwrap();
            if let Some(nonce) = intent.revoke_nonce {
                e.bytes(&nonce).unwrap();
            } else {
                e.null().unwrap();
            }
            if let Some(recipients) = &intent.recipients {
                e.array(recipients.len() as u64).unwrap();
                for (installation, transport) in recipients {
                    e.array(2)
                        .unwrap()
                        .bytes(&installation.0)
                        .unwrap()
                        .bytes(transport)
                        .unwrap();
                }
            } else {
                e.null().unwrap();
            }
        }
        e.into_writer()
    }
    pub(crate) fn decode(input: &[u8], peer: PeerId) -> Result<Self, CoreError> {
        if input.len() > 128 * 1024 {
            return Err(wire::invalid());
        }
        let mut d = Decoder::new(input);
        wire::array(&mut d, 9)?;
        if wire::uint(&mut d)? != 1 {
            return Err(wire::invalid());
        }
        let generation = Generation(wire::bytes(&mut d)?);
        let state = match wire::uint(&mut d)? {
            0 => PublicationState::LocalBlocked,
            1 => PublicationState::Pending,
            2 => PublicationState::Observed,
            3 => PublicationState::Conflict,
            _ => return Err(wire::invalid()),
        };
        let observed_seq = wire::uint(&mut d)?;
        let allocated_seq = wire::uint(&mut d)?;
        if observed_seq > i64::MAX as u64 || allocated_seq > i64::MAX as u64 {
            return Err(wire::invalid());
        }
        let mut read_record = || -> Result<Option<SignedRecord>, CoreError> {
            if d.datatype().map_err(|_| wire::invalid())? == Type::Null {
                d.null().map_err(|_| wire::invalid())?;
                Ok(None)
            } else {
                Ok(Some(SignedRecord::decode(
                    d.bytes().map_err(|_| wire::invalid())?,
                    peer,
                )?))
            }
        };
        let accepted = read_record()?;
        let conflict = read_record()?;
        if accepted
            .iter()
            .chain(conflict.iter())
            .any(|r| r.seq > observed_seq)
        {
            return Err(wire::invalid());
        }
        let count = bounded_array(&mut d, 256)?;
        let mut tombstones = Vec::with_capacity(count);
        for _ in 0..count {
            let intent = RevokeIntent::from_bytes(d.bytes().map_err(|_| wire::invalid())?)?;
            intent.verify(peer)?;
            if tombstones.iter().any(|t: &RevokeIntent| {
                t.target_installation == intent.target_installation || t.nonce == intent.nonce
            }) {
                return Err(wire::invalid());
            }
            tombstones.push(intent);
        }
        let count = bounded_array(&mut d, 32)?;
        let mut pending = Vec::with_capacity(count);
        for _ in 0..count {
            wire::array(&mut d, 6)?;
            let signed = SignedRecord::decode(d.bytes().map_err(|_| wire::invalid())?, peer)?;
            if signed.seq > allocated_seq
                || signed.record.generation != generation
                || pending
                    .iter()
                    .any(|p: &PacketIntent| p.signed.seq == signed.seq)
            {
                return Err(wire::invalid());
            }
            let base_hash = optional_bytes(&mut d)?;
            let operation = match wire::uint(&mut d)? {
                0 => PacketOperation::First,
                1 => PacketOperation::Replacement,
                2 => PacketOperation::Renew,
                3 => PacketOperation::Revoke,
                4 => PacketOperation::Resolve,
                5 => PacketOperation::Add,
                _ => return Err(wire::invalid()),
            };
            let status = match wire::uint(&mut d)? {
                0 => PacketStatus::Ready,
                1 => PacketStatus::AwaitingNotification,
                2 => PacketStatus::Held,
                _ => return Err(wire::invalid()),
            };
            let revoke_nonce = optional_bytes(&mut d)?;
            if revoke_nonce.is_some_and(|nonce| !tombstones.iter().any(|t| t.nonce == nonce))
                || (status == PacketStatus::AwaitingNotification && revoke_nonce.is_none())
            {
                return Err(wire::invalid());
            }
            let recipients = if d.datatype().map_err(|_| wire::invalid())? == Type::Null {
                d.null().map_err(|_| wire::invalid())?;
                None
            } else {
                let count = bounded_array(&mut d, 4)?;
                let target = tombstones
                    .iter()
                    .find(|t| Some(t.nonce) == revoke_nonce)
                    .ok_or_else(wire::invalid)?;
                let mut recipients: Vec<Recipient> = Vec::with_capacity(count);
                for _ in 0..count {
                    wire::array(&mut d, 2)?;
                    let recipient = (InstallationId(wire::bytes(&mut d)?), wire::bytes(&mut d)?);
                    if recipient.0 == target.target_installation
                        || recipient.1 == target.target_transport
                        || recipients.last().is_some_and(|last| *last >= recipient)
                        || recipients
                            .iter()
                            .any(|r| r.0 == recipient.0 || r.1 == recipient.1)
                    {
                        return Err(wire::invalid());
                    }
                    recipients.push(recipient);
                }
                Some(recipients)
            };
            pending.push(PacketIntent {
                signed,
                base_hash,
                operation,
                status,
                revoke_nonce,
                recipients,
            });
        }
        let result = Self {
            generation,
            accepted,
            conflict,
            observed_seq,
            allocated_seq,
            tombstones,
            pending,
            state,
        };
        wire::canonical(input, &result.encode())?;
        Ok(result)
    }
}
fn bounded_array(d: &mut Decoder<'_>, max: u64) -> Result<usize, CoreError> {
    let count = d
        .array()
        .map_err(|_| wire::invalid())?
        .ok_or_else(wire::invalid)?;
    if count > max {
        return Err(wire::invalid());
    }
    Ok(count as usize)
}
fn optional_bytes<const N: usize>(d: &mut Decoder<'_>) -> Result<Option<[u8; N]>, CoreError> {
    if d.datatype().map_err(|_| wire::invalid())? == Type::Null {
        d.null().map_err(|_| wire::invalid())?;
        Ok(None)
    } else {
        Ok(Some(wire::bytes(d)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{Secret32, derive_account, recovery};
    fn local() -> LocalAccount {
        recovery::local_account(
            derive_account(&Secret32::from([1; 32])).unwrap(),
            PublishIntent::LocalCreate,
            1,
        )
        .unwrap()
    }

    const OWN_CHILD: &str = "CHAT_CORE_OWN_PUBLICATION_FIXTURE";
    const OWN_TEST: &str = "account::publication::tests::own_publication_diagnostic_preserves_reconcile_outcomes";

    // Missing hold diagnostics, using pre-reconcile rather than hold-entry
    // sequence facts, or changing reconciliation outcomes must fail this test.
    #[test]
    fn own_publication_diagnostic_preserves_reconcile_outcomes() {
        if let Ok(case) = std::env::var(OWN_CHILD) {
            const SEQ: u64 = 1_000_000_000;
            let mut local = local();
            local.device = super::super::derive_device(Secret32::from([7; 32])).unwrap();
            local.cert = super::super::sign_cert(&local.keys, &local.device, super::super::DeviceKind::Desktop, 1000).unwrap();
            let mut records = local.records.clone();
            records.generation = Generation([1; 16]);
            let mut record = records.first_record(&local);
            record.issued_at = 1000;
            record.expires_at = 1600;
            let signed = |record: &IdentityRecord, seq| {
                SignedRecord::new(record_packet(&local.keys, record, seq).unwrap(), local.keys.peer_id()).unwrap()
            };
            let accepted = signed(&record, SEQ);
            records.accepted = Some(accepted.clone());
            records.observed_seq = SEQ;
            records.allocated_seq = SEQ;
            records.state = PublicationState::Observed;
            let mut found = accepted.clone();
            match case.as_str() {
                "accepted" | "retained" | "retained_absent" => {}
                "retained_stale" | "retained_changed_generation" | "retained_changed_devices" => {
                    let mut retained_record = record.clone();
                    retained_record.expires_at = 1602;
                    if case == "retained_changed_generation" { retained_record.generation = Generation([2; 16]); }
                    if case == "retained_changed_devices" { retained_record.devices[0].kind = super::super::DeviceKind::Mobile; }
                    records.conflict = Some(signed(&retained_record, SEQ - 1));
                    records.state = PublicationState::Conflict;
                }
                "accepted_expired" | "accepted_future" | "accepted_other_generation" => {
                    let mut accepted_record = record.clone();
                    match case.as_str() {
                        "accepted_expired" => { accepted_record.issued_at = 400; accepted_record.expires_at = 1000; }
                        "accepted_future" => { accepted_record.issued_at = 1600; accepted_record.expires_at = 2200; }
                        "accepted_other_generation" => accepted_record.generation = Generation([2; 16]),
                        _ => unreachable!(),
                    }
                    records.accepted = Some(signed(&accepted_record, SEQ));
                    found = signed(&record, SEQ - 1);
                }
                "stale" => found = signed(&record, SEQ - 1),
                "later_unknown" => {
                    record.generation = Generation([2; 16]);
                    record.devices[0].transport_public_key = [8; 32];
                    found = signed(&record, SEQ + 1);
                }
                "pending_held" | "pending_ready" | "retained_pending_ready" => {
                    record.expires_at = 1601;
                    found = signed(&record, SEQ + 1);
                    records.pending.push(PacketIntent {
                        signed: found.clone(),
                        base_hash: Some(accepted.hash),
                        operation: PacketOperation::Renew,
                        status: if case == "pending_held" { PacketStatus::Held } else { PacketStatus::Ready },
                        revoke_nonce: None,
                        recipients: None,
                    });
                    records.allocated_seq = SEQ + 1;
                    records.state = PublicationState::Pending;
                }
                "future" => {
                    record.issued_at = 1600;
                    record.expires_at = 2200;
                    found = signed(&record, SEQ + 600_000_000);
                }
                "unaccepted" => {
                    records.accepted = None;
                    records.observed_seq = 0;
                    records.allocated_seq = 0;
                    records.state = PublicationState::LocalBlocked;
                }
                "expired" => {
                    record.issued_at = 400;
                    record.expires_at = 1000;
                    found = signed(&record, SEQ - 1);
                }
                _ => panic!("unknown own-publication child fixture"),
            }
            if matches!(case.as_str(), "retained" | "retained_absent" | "retained_pending_ready") {
                record.expires_at = 1602;
                records.conflict = Some(signed(&record, SEQ + 2));
                records.observed_seq = SEQ + 2;
                records.state = PublicationState::Conflict;
            }
            let mut expected = records.clone();
            match case.as_str() {
                "accepted" | "retained_absent" => {}
                "pending_ready" => {
                    expected.accepted = Some(found.clone());
                    expected.observed_seq = SEQ + 1;
                    expected.pending.clear();
                    expected.state = PublicationState::Observed;
                }
                "retained" | "retained_pending_ready" => {
                    for intent in &mut expected.pending { intent.status = PacketStatus::Held; }
                }
                _ => {
                    expected.conflict = Some(found.clone());
                    expected.state = PublicationState::Conflict;
                    expected.observed_seq = match case.as_str() {
                        "later_unknown" | "pending_held" => SEQ + 1,
                        "future" => SEQ + 600_000_000,
                        _ => SEQ,
                    };
                }
            }
            if std::env::var_os("CHAT_CORE_OWN_PUBLICATION_CLOSED_STDERR").is_some() {
                use std::io::Read;
                std::io::stdin().read_exact(&mut [0]).unwrap();
            }
            let lookup = if case == "retained_absent" { Lookup::CompletedAbsent } else { Lookup::Found(found.packet) };
            let result = records.reconcile(lookup, &local, 1000, Action::Refresh);
            if matches!(case.as_str(), "accepted" | "pending_ready") {
                result.unwrap();
            } else {
                let error = result.unwrap_err();
                if case == "future" {
                    assert_eq!(error.code, ErrorCode::AccountRestricted);
                    assert_eq!(error.detail, "Account publication requires a valid clock and current membership");
                } else {
                    assert_eq!(error.code, ErrorCode::RecordConflict);
                    assert_eq!(error.detail, "Device list changed; confirm the current list");
                }
            }
            assert_eq!(records.encode(), expected.encode(), "reconcile changed the original outcome for {case}");
            assert_eq!(RecordState::decode(&records.encode(), local.keys.peer_id()).unwrap().encode(), expected.encode());
            return;
        }

        let cases = [
            ("accepted", ""),
            ("stale", "retained_before=false accepted_present=true accepted_hash_equal=false seq_vs_observed_at_hold=Less seq_vs_allocated_at_hold=Less generation_equal=true devices_equal_to_accepted=true pending_hash_match=false pending_ready_hash_match=false time_valid=true expired=false"),
            ("later_unknown", "retained_before=false accepted_present=true accepted_hash_equal=false seq_vs_observed_at_hold=Greater seq_vs_allocated_at_hold=Greater generation_equal=false devices_equal_to_accepted=false pending_hash_match=false pending_ready_hash_match=false time_valid=true expired=false"),
            ("retained", "retained_before=true accepted_present=true accepted_hash_equal=true seq_vs_observed_at_hold=Less seq_vs_allocated_at_hold=Equal generation_equal=true devices_equal_to_accepted=true pending_hash_match=false pending_ready_hash_match=false time_valid=true expired=false"),
            ("retained_stale", "retained_before=true accepted_present=true accepted_hash_equal=true seq_vs_observed_at_hold=Equal seq_vs_allocated_at_hold=Equal generation_equal=true devices_equal_to_accepted=true pending_hash_match=false pending_ready_hash_match=false time_valid=true expired=false"),
            ("retained_changed_generation", "retained_before=true accepted_present=true accepted_hash_equal=true seq_vs_observed_at_hold=Equal seq_vs_allocated_at_hold=Equal generation_equal=true devices_equal_to_accepted=true pending_hash_match=false pending_ready_hash_match=false time_valid=true expired=false"),
            ("retained_changed_devices", "retained_before=true accepted_present=true accepted_hash_equal=true seq_vs_observed_at_hold=Equal seq_vs_allocated_at_hold=Equal generation_equal=true devices_equal_to_accepted=true pending_hash_match=false pending_ready_hash_match=false time_valid=true expired=false"),
            ("pending_held", "retained_before=false accepted_present=true accepted_hash_equal=false seq_vs_observed_at_hold=Greater seq_vs_allocated_at_hold=Equal generation_equal=true devices_equal_to_accepted=true pending_hash_match=true pending_ready_hash_match=false time_valid=true expired=false"),
            ("pending_ready", ""),
            ("retained_pending_ready", "retained_before=true accepted_present=true accepted_hash_equal=false seq_vs_observed_at_hold=Less seq_vs_allocated_at_hold=Equal generation_equal=true devices_equal_to_accepted=true pending_hash_match=true pending_ready_hash_match=true time_valid=true expired=false"),
            ("future", "retained_before=false accepted_present=true accepted_hash_equal=false seq_vs_observed_at_hold=Equal seq_vs_allocated_at_hold=Greater generation_equal=true devices_equal_to_accepted=true pending_hash_match=false pending_ready_hash_match=false time_valid=false expired=false"),
            ("unaccepted", "retained_before=false accepted_present=false accepted_hash_equal=false seq_vs_observed_at_hold=Greater seq_vs_allocated_at_hold=Greater generation_equal=true devices_equal_to_accepted=false pending_hash_match=false pending_ready_hash_match=false time_valid=true expired=false"),
            ("expired", "retained_before=false accepted_present=true accepted_hash_equal=false seq_vs_observed_at_hold=Less seq_vs_allocated_at_hold=Less generation_equal=true devices_equal_to_accepted=true pending_hash_match=false pending_ready_hash_match=false time_valid=true expired=true"),
            ("accepted_expired", "retained_before=false accepted_present=true accepted_hash_equal=false seq_vs_observed_at_hold=Less seq_vs_allocated_at_hold=Less generation_equal=true devices_equal_to_accepted=true pending_hash_match=false pending_ready_hash_match=false time_valid=true expired=false"),
            ("accepted_future", "retained_before=false accepted_present=true accepted_hash_equal=false seq_vs_observed_at_hold=Less seq_vs_allocated_at_hold=Less generation_equal=true devices_equal_to_accepted=true pending_hash_match=false pending_ready_hash_match=false time_valid=true expired=false"),
            ("accepted_other_generation", "retained_before=false accepted_present=true accepted_hash_equal=false seq_vs_observed_at_hold=Less seq_vs_allocated_at_hold=Less generation_equal=true devices_equal_to_accepted=true pending_hash_match=false pending_ready_hash_match=false time_valid=true expired=false"),
            ("retained_absent", "lookup_present=false retained_before=true"),
        ];
        for (case, facts) in cases {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", OWN_TEST, "--nocapture"])
                .env(OWN_CHILD, case)
                .output().unwrap();
            assert!(output.status.success(), "child {case} failed: {}", String::from_utf8_lossy(&output.stderr));
            let stderr = String::from_utf8(output.stderr).unwrap().replace("\r\n", "\n");
            let context = match case {
                "accepted" | "pending_ready" | "retained_absent" => "",
                "stale" | "later_unknown" | "expired" => "accepted_meets_observed_floor=true accepted_meets_allocated_floor=true retained_older_than_accepted=false retained_membership_equal_accepted=false accepted_current_unexpired=true accepted_generation_equal_local=true pending_empty=true",
                "retained" => "accepted_meets_observed_floor=false accepted_meets_allocated_floor=true retained_older_than_accepted=false retained_membership_equal_accepted=true accepted_current_unexpired=true accepted_generation_equal_local=true pending_empty=true",
                "retained_stale" => "accepted_meets_observed_floor=true accepted_meets_allocated_floor=true retained_older_than_accepted=true retained_membership_equal_accepted=true accepted_current_unexpired=true accepted_generation_equal_local=true pending_empty=true",
                "retained_changed_generation" | "retained_changed_devices" => "accepted_meets_observed_floor=true accepted_meets_allocated_floor=true retained_older_than_accepted=true retained_membership_equal_accepted=false accepted_current_unexpired=true accepted_generation_equal_local=true pending_empty=true",
                "pending_held" => "accepted_meets_observed_floor=true accepted_meets_allocated_floor=false retained_older_than_accepted=false retained_membership_equal_accepted=false accepted_current_unexpired=true accepted_generation_equal_local=true pending_empty=false",
                "retained_pending_ready" => "accepted_meets_observed_floor=false accepted_meets_allocated_floor=false retained_older_than_accepted=false retained_membership_equal_accepted=true accepted_current_unexpired=true accepted_generation_equal_local=true pending_empty=false",
                "future" => "accepted_meets_observed_floor=false accepted_meets_allocated_floor=true retained_older_than_accepted=false retained_membership_equal_accepted=false accepted_current_unexpired=true accepted_generation_equal_local=true pending_empty=true",
                "unaccepted" => "accepted_meets_observed_floor=false accepted_meets_allocated_floor=false retained_older_than_accepted=false retained_membership_equal_accepted=false accepted_current_unexpired=false accepted_generation_equal_local=false pending_empty=true",
                "accepted_expired" | "accepted_future" => "accepted_meets_observed_floor=true accepted_meets_allocated_floor=true retained_older_than_accepted=false retained_membership_equal_accepted=false accepted_current_unexpired=false accepted_generation_equal_local=true pending_empty=true",
                "accepted_other_generation" => "accepted_meets_observed_floor=true accepted_meets_allocated_floor=true retained_older_than_accepted=false retained_membership_equal_accepted=false accepted_current_unexpired=true accepted_generation_equal_local=false pending_empty=true",
                _ => unreachable!(),
            };
            let mut expected = if facts.is_empty() { String::new() } else { format!("own_publication hold {facts}\n") };
            if !context.is_empty() { expected.push_str(&format!("own_publication retained_context {context}\n")); }
            // Exact whitelist admits no private fixture data, record/packet,
            // identifiers, raw sequence/time values, or arbitrary error text.
            assert_eq!(stderr, expected, "own-publication diagnostic for {case}");
        }
    }

    #[test]
    fn own_publication_diagnostic_ignores_closed_stderr() {
        use std::io::Write;
        use std::process::Stdio;
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", OWN_TEST, "--nocapture"])
            .env(OWN_CHILD, "stale")
            .env("CHAT_CORE_OWN_PUBLICATION_CLOSED_STDERR", "true")
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .spawn().unwrap();
        drop(child.stderr.take().unwrap());
        child.stdin.take().unwrap().write_all(&[1]).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "closed stderr changed reconcile outcome: {}", String::from_utf8_lossy(&output.stdout));
    }
    #[test]
    fn durable_notification_recipients_are_bounded_canonical_and_survive_renew_rebase() {
        let local = local();
        let mut records = local.records.clone();
        let mut base = records.first_record(&local);
        let target = RecordDevice {
            installation_id: InstallationId([4; 32]),
            transport_public_key: [5; 32],
            kind: local.cert.kind,
        };
        base.devices.push(target.clone());
        base.devices.sort_by_key(|d| d.installation_id);
        base.issued_at = 1000;
        base.expires_at = 2000;
        let packet = record_packet(&local.keys, &base, 1_000_000_000).unwrap();
        records.accepted = Some(SignedRecord::new(packet.clone(), local.keys.peer_id()).unwrap());
        records.observed_seq = 1_000_000_000;
        records
            .reconcile(
                Lookup::Found(packet),
                &local,
                1000,
                Action::Revoke(target.installation_id),
            )
            .unwrap();
        let expected = vec![(InstallationId([8; 32]), [9; 32])];
        for recipients in [None, Some(vec![]), Some(expected.clone())] {
            records.pending[0].recipients = recipients.clone();
            let restored = RecordState::decode(&records.encode(), local.keys.peer_id()).unwrap();
            assert_eq!(restored.pending[0].recipients, recipients);
        }
        for invalid in [
            vec![expected[0], expected[0]],
            vec![(InstallationId([10; 32]), [11; 32]), expected[0]],
            vec![(target.installation_id, target.transport_public_key)],
            (10..15)
                .map(|i| (InstallationId([i; 32]), [i; 32]))
                .collect(),
        ] {
            let mut invalid_state = records.clone();
            invalid_state.pending[0].recipients = Some(invalid);
            assert!(RecordState::decode(&invalid_state.encode(), local.keys.peer_id()).is_err());
        }
        let expiry = records.pending[0].signed.record.expires_at;
        records
            .reconcile(Lookup::CompletedAbsent, &local, expiry, Action::Refresh)
            .unwrap();
        assert_eq!(records.pending[0].recipients, Some(expected.clone()));
        // Explicit resolution retains the required receipts even though the
        // changed authority can prevent them from being fulfilled automatically.
        base.issued_at = expiry;
        base.expires_at = expiry + 600;
        let conflict = record_packet(&local.keys, &base, (expiry + 1) * 1_000_000).unwrap();
        let hash = record_hash(&conflict);
        assert!(
            records
                .reconcile(
                    Lookup::Found(conflict.clone()),
                    &local,
                    expiry,
                    Action::Refresh
                )
                .is_err()
        );
        records
            .reconcile(
                Lookup::Found(conflict),
                &local,
                expiry,
                Action::Confirm(hash),
            )
            .unwrap();
        assert_eq!(records.pending[0].recipients, Some(expected));
        assert!(RecordState::decode(&records.encode(), local.keys.peer_id()).is_ok());
    }
    #[test]
    fn received_tombstone_holds_exact_packet_without_losing_receipt_obligation() {
        let local = local();
        let mut records = local.records.clone();
        let mut base = records.first_record(&local);
        let target = RecordDevice {
            installation_id: InstallationId([4; 32]),
            transport_public_key: [5; 32],
            kind: local.cert.kind,
        };
        let removed = RecordDevice {
            installation_id: InstallationId([8; 32]),
            transport_public_key: [9; 32],
            kind: local.cert.kind,
        };
        base.devices.extend([target.clone(), removed.clone()]);
        base.devices.sort_by_key(|d| d.installation_id);
        base.issued_at = 1000;
        base.expires_at = 2000;
        let packet = record_packet(&local.keys, &base, 1_000_000_000).unwrap();
        records.accepted = Some(SignedRecord::new(packet.clone(), local.keys.peer_id()).unwrap());
        records.observed_seq = 1_000_000_000;
        records
            .reconcile(
                Lookup::Found(packet.clone()),
                &local,
                1000,
                Action::Revoke(target.installation_id),
            )
            .unwrap();
        records.pending[0].recipients = Some(vec![(
            removed.installation_id,
            removed.transport_public_key,
        )]);
        let before = records.pending[0].clone();
        records.tombstones.push(
            RevokeIntent::sign(
                &local.keys,
                records.generation,
                record_hash(&packet),
                &removed,
                1000,
            )
            .unwrap(),
        );
        records.restrict_pending(&local);
        assert!(records.pending[0].status == PacketStatus::Held);
        let restored = RecordState::decode(&records.encode(), local.keys.peer_id()).unwrap();
        assert_eq!(
            restored.pending[0].signed.packet.as_bytes(),
            before.signed.packet.as_bytes()
        );
        assert_eq!(restored.pending[0].signed.seq, before.signed.seq);
        assert_eq!(restored.pending[0].recipients, before.recipients);
        assert_eq!(restored.pending[0].revoke_nonce, before.revoke_nonce);
        assert_eq!(restored.view().pending_notifications.len(), 1);
    }
    #[test]
    fn future_competitor_preserves_tombstone_and_holds_notification_in_snapshot() {
        let local = local();
        let mut records = local.records.clone();
        let mut base = records.first_record(&local);
        let target = RecordDevice {
            installation_id: InstallationId([4; 32]),
            transport_public_key: [5; 32],
            kind: local.cert.kind,
        };
        base.devices.push(target.clone());
        base.devices.sort_by_key(|device| device.installation_id);
        base.issued_at = 1000;
        base.expires_at = 2000;
        let packet = record_packet(&local.keys, &base, 1_000_000_000).unwrap();
        records.accepted = Some(SignedRecord::new(packet.clone(), local.keys.peer_id()).unwrap());
        records.observed_seq = 1_000_000_000;
        records.state = PublicationState::Observed;
        records
            .reconcile(
                Lookup::Found(packet),
                &local,
                1000,
                Action::Revoke(target.installation_id),
            )
            .unwrap();
        let tombstones = records.tombstones.clone();
        let allocated = records.allocated_seq;
        base.generation = Generation([9; 16]);
        base.issued_at = 1600;
        let future = record_packet(&local.keys, &base, 1_600_000_000).unwrap();
        assert_eq!(
            records
                .reconcile(Lookup::Found(future), &local, 1000, Action::Refresh)
                .unwrap_err()
                .code,
            ErrorCode::AccountRestricted
        );
        let mut restored = RecordState::decode(&records.encode(), local.keys.peer_id()).unwrap();
        assert!(
            restored
                .pending
                .iter()
                .all(|intent| intent.status == PacketStatus::Held)
        );
        assert_eq!(restored.tombstones, tombstones);
        assert_eq!(restored.view().pending_notifications, tombstones);
        assert_eq!(
            restored
                .reconcile(Lookup::CompletedAbsent, &local, 1600, Action::Refresh)
                .unwrap_err()
                .code,
            ErrorCode::RecordConflict
        );
        assert_eq!(restored.allocated_seq, allocated);
        assert_eq!(restored.tombstones, tombstones);
    }
    #[test]
    fn tombstone_256_and_pending_32_preserve_old_state() {
        let local = local();
        let mut records = local.records.clone();
        let record = records.first_record(&local);
        for _ in 0..32 {
            records
                .allocate(&local, record.clone(), PacketOperation::Renew, None, 1000)
                .unwrap();
        }
        let before = records.encode();
        assert_eq!(
            records
                .allocate(&local, record, PacketOperation::Renew, None, 1000)
                .unwrap_err()
                .code,
            ErrorCode::StorageFull
        );
        assert_eq!(records.encode(), before);
        assert!(RecordState::decode(&before, local.keys.peer_id()).is_ok());
        records.pending.push(records.pending[0].clone());
        assert!(RecordState::decode(&records.encode(), local.keys.peer_id()).is_err());

        let mut records = local.records.clone();
        let mut record = records.first_record(&local);
        let target = RecordDevice {
            installation_id: InstallationId([4; 32]),
            transport_public_key: [5; 32],
            kind: local.cert.kind,
        };
        record.devices.push(target.clone());
        record.devices.sort_by_key(|d| d.installation_id);
        record.issued_at = 1000;
        record.expires_at = 2000;
        let packet = record_packet(&local.keys, &record, 1_000_000_000).unwrap();
        records.accepted = Some(SignedRecord::new(packet.clone(), local.keys.peer_id()).unwrap());
        records.observed_seq = 1_000_000_000;
        for i in 0..256 {
            let mut target = target.clone();
            target.installation_id = InstallationId([0; 32]);
            target.installation_id.0[..2].copy_from_slice(&(i as u16).to_be_bytes());
            records.tombstones.push(
                RevokeIntent::sign(&local.keys, records.generation, [0; 32], &target, 1000)
                    .unwrap(),
            );
        }
        let before = records.encode();
        assert_eq!(
            records
                .reconcile(
                    Lookup::Found(packet),
                    &local,
                    1000,
                    Action::Revoke(target.installation_id)
                )
                .unwrap_err()
                .code,
            ErrorCode::StorageFull
        );
        assert_eq!(records.encode(), before);
        assert!(RecordState::decode(&before, local.keys.peer_id()).is_ok());
        records.tombstones.push(records.tombstones[0].clone());
        assert!(RecordState::decode(&records.encode(), local.keys.peer_id()).is_err());
    }
    #[test]
    fn high_water_overflow_and_invalid_clock_preserve_allocations() {
        let local = local();
        for now in [0, u64::MAX] {
            let mut records = local.records.clone();
            let before = records.encode();
            assert_eq!(
                records
                    .allocate(
                        &local,
                        records.first_record(&local),
                        PacketOperation::First,
                        None,
                        now
                    )
                    .unwrap_err()
                    .code,
                ErrorCode::AccountRestricted
            );
            assert_eq!(records.encode(), before);
        }
        let mut records = local.records.clone();
        records.observed_seq = i64::MAX as u64;
        assert_eq!(
            records.sequence(1000).unwrap_err().code,
            ErrorCode::AccountRestricted
        );
        assert_eq!(records.allocated_seq, 0);
    }
    #[test]
    fn absence_is_only_a_renewal_base_not_revoke_authorization() {
        let local = local();
        let mut records = local.records.clone();
        let mut record = records.first_record(&local);
        let target = RecordDevice {
            installation_id: InstallationId([4; 32]),
            transport_public_key: [5; 32],
            kind: local.cert.kind,
        };
        record.devices.push(target.clone());
        record.devices.sort_by_key(|d| d.installation_id);
        record.issued_at = 1000;
        record.expires_at = 2000;
        records.accepted = Some(
            SignedRecord::new(
                record_packet(&local.keys, &record, 1_000_000_000).unwrap(),
                local.keys.peer_id(),
            )
            .unwrap(),
        );
        records.observed_seq = 1_000_000_000;
        assert_eq!(
            records
                .reconcile(
                    Lookup::CompletedAbsent,
                    &local,
                    1000,
                    Action::Revoke(target.installation_id)
                )
                .unwrap_err()
                .code,
            ErrorCode::AccountRestricted
        );
        assert!(records.tombstones.is_empty());
    }
}
