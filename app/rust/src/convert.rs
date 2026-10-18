//! Conversions between chat_core types and the types Dart sees.

use crate::api::chat::{
    ChatEvent, CoreError, CoreInfo, ErrorCode, NetworkInfo, NetworkState, PeerInfo, PeerPath, PeerPhase,
};
use crate::frb_generated::StreamSink;

impl From<chat_core::CoreInfo> for CoreInfo {
    fn from(info: chat_core::CoreInfo) -> Self {
        Self { version: info.version, instance_id: info.instance_id }
    }
}

impl From<chat_core::ErrorCode> for ErrorCode {
    fn from(code: chat_core::ErrorCode) -> Self {
        match code {
            chat_core::ErrorCode::InvalidConfig => Self::InvalidConfig,
            chat_core::ErrorCode::AlreadyStartedDifferentConfig => Self::AlreadyStartedDifferentConfig,
            chat_core::ErrorCode::CoreNotStarted => Self::CoreNotStarted,
            chat_core::ErrorCode::NetworkNotStarted => Self::NetworkNotStarted,
            chat_core::ErrorCode::InvalidPeerAddress => Self::InvalidPeerAddress,
            chat_core::ErrorCode::SelfConnection => Self::SelfConnection,
            chat_core::ErrorCode::PeerNotConnected => Self::PeerNotConnected,
            chat_core::ErrorCode::MessageTooLarge => Self::MessageTooLarge,
            chat_core::ErrorCode::Busy => Self::Busy,
            chat_core::ErrorCode::NetworkTimeout => Self::NetworkTimeout,
            chat_core::ErrorCode::TransportFailed => Self::TransportFailed,
            chat_core::ErrorCode::NotAvailable => Self::NotAvailable,
            chat_core::ErrorCode::InvalidRecovery => Self::InvalidRecovery,
            chat_core::ErrorCode::StorageLocked => Self::StorageLocked,
            chat_core::ErrorCode::StorageMissing => Self::StorageMissing,
            chat_core::ErrorCode::StorageCorrupt => Self::StorageCorrupt,
            chat_core::ErrorCode::StorageFull => Self::StorageFull,
            chat_core::ErrorCode::AccountExists => Self::AccountExists,
            chat_core::ErrorCode::WriterBusy => Self::WriterBusy,
            chat_core::ErrorCode::InvalidInvite => Self::InvalidInvite,
            chat_core::ErrorCode::InviteExpired => Self::InviteExpired,
            chat_core::ErrorCode::DeviceLimit => Self::DeviceLimit,
            chat_core::ErrorCode::PairMismatch => Self::PairMismatch,
            chat_core::ErrorCode::PairExpired => Self::PairExpired,
            chat_core::ErrorCode::RecordConflict => Self::RecordConflict,
            chat_core::ErrorCode::RecordUnavailable => Self::RecordUnavailable,
            chat_core::ErrorCode::AuthFailed => Self::AuthFailed,
            chat_core::ErrorCode::AccountRestricted => Self::AccountRestricted,
        }
    }
}

impl From<chat_core::CoreError> for CoreError {
    fn from(err: chat_core::CoreError) -> Self {
        Self { code: err.code.into(), detail: err.detail }
    }
}

impl From<chat_core::NetworkState> for NetworkState {
    fn from(value: chat_core::NetworkState) -> Self {
        match value {
            chat_core::NetworkState::Stopped => Self::Stopped,
            chat_core::NetworkState::Starting => Self::Starting,
            chat_core::NetworkState::Online => Self::Online,
            chat_core::NetworkState::Failed => Self::Failed,
        }
    }
}

impl From<chat_core::PeerPhase> for PeerPhase {
    fn from(value: chat_core::PeerPhase) -> Self {
        match value {
            chat_core::PeerPhase::Connecting => Self::Connecting,
            chat_core::PeerPhase::Connected => Self::Connected,
            chat_core::PeerPhase::Reconnecting => Self::Reconnecting,
            chat_core::PeerPhase::Disconnected => Self::Disconnected,
        }
    }
}

impl From<chat_core::PeerPath> for PeerPath {
    fn from(value: chat_core::PeerPath) -> Self {
        match value {
            chat_core::PeerPath::Unknown => Self::Unknown,
            chat_core::PeerPath::Relay => Self::Relay,
            chat_core::PeerPath::Direct => Self::Direct,
        }
    }
}

impl From<chat_core::PeerInfo> for PeerInfo {
    fn from(peer: chat_core::PeerInfo) -> Self {
        Self {
            peer_id: peer.peer_id,
            phase: peer.phase.into(),
            path: peer.path.into(),
            connect_ms: peer.connect_ms,
            direct_ms: peer.direct_ms,
        }
    }
}

impl From<chat_core::NetworkInfo> for NetworkInfo {
    fn from(info: chat_core::NetworkInfo) -> Self {
        Self {
            revision: info.revision,
            state: info.state.into(),
            endpoint_id: info.endpoint_id,
            ticket: info.ticket,
            peers: info.peers.into_iter().map(PeerInfo::from).collect(),
        }
    }
}

impl From<chat_core::CoreEvent> for ChatEvent {
    fn from(event: chat_core::CoreEvent) -> Self {
        match event {
            chat_core::CoreEvent::AccountChanged { revision, view } => Self::AccountChanged { revision, view: Box::new((*view).into()) },
            chat_core::CoreEvent::AccountAuthChanged {peer_id,installation_id,ready} => Self::AccountAuthChanged {peer_id,installation_id,ready},
            chat_core::CoreEvent::AccountCacheUsed {peer_id} => Self::AccountCacheUsed {peer_id},
            chat_core::CoreEvent::ConnectionState(s) => Self::ConnectionState {
                direct_peers: s.direct_peers,
                mailbox_ok: s.mailbox_ok,
                queue_ok: s.queue_ok,
            },
            chat_core::CoreEvent::Error { code, detail } => Self::Error { code: code.into(), detail },
            chat_core::CoreEvent::NetworkChanged(info) => Self::NetworkChanged { info: info.into() },
            chat_core::CoreEvent::PeerChanged { revision, peer } => {
                Self::PeerChanged { revision, peer: peer.into() }
            }
            chat_core::CoreEvent::ProbeTextReceived { peer_id, text } => {
                Self::ProbeTextReceived { peer_id, text }
            }
        }
    }
}

/// Forwards core events into a Dart stream. A closed Dart stream ends the subscription.
pub(crate) struct FrbSink(pub(crate) StreamSink<ChatEvent>);

impl chat_core::EventSink for FrbSink {
    fn send(&self, event: chat_core::CoreEvent) -> bool {
        self.0.add(event.into()).is_ok()
    }
}

// Public account DTO conversion; private storage owners never cross this boundary.
impl From<chat_core::account::AccountPhase> for crate::api::account::AccountPhase {
    fn from(v: chat_core::account::AccountPhase) -> Self {
        match v {
            chat_core::account::AccountPhase::Unconfigured => Self::Unconfigured,
            chat_core::account::AccountPhase::Candidate => Self::Candidate,
            chat_core::account::AccountPhase::PendingImport => Self::PendingImport,
            chat_core::account::AccountPhase::LocalReady => Self::LocalReady,
            chat_core::account::AccountPhase::PublishPending => Self::PublishPending,
            chat_core::account::AccountPhase::Active => Self::Active,
            chat_core::account::AccountPhase::Conflict => Self::Conflict,
            chat_core::account::AccountPhase::Locked => Self::Locked,
        }
    }
}
impl From<chat_core::account::PublishIntent> for crate::api::account::PublishIntent {
    fn from(v: chat_core::account::PublishIntent) -> Self {
        match v {
            chat_core::account::PublishIntent::LocalCreate => Self::LocalCreate,
            chat_core::account::PublishIntent::Replacement => Self::Replacement,
        }
    }
}
impl From<chat_core::account::PublicationState> for crate::api::account::PublicationState {
    fn from(v: chat_core::account::PublicationState) -> Self {
        match v {
            chat_core::account::PublicationState::LocalBlocked => Self::LocalBlocked,
            chat_core::account::PublicationState::Pending => Self::Pending,
            chat_core::account::PublicationState::Observed => Self::Observed,
            chat_core::account::PublicationState::Conflict => Self::Conflict,
        }
    }
}
impl From<chat_core::account::DeviceKind> for crate::api::account::DeviceKind {
    fn from(v: chat_core::account::DeviceKind) -> Self {
        match v {
            chat_core::account::DeviceKind::Desktop => Self::Desktop,
            chat_core::account::DeviceKind::Mobile => Self::Mobile,
        }
    }
}
impl From<chat_core::account::PairDirection> for crate::api::account::PairDirection {
    fn from(v: chat_core::account::PairDirection) -> Self {
        match v {
            chat_core::account::PairDirection::NewDisplays => Self::NewDisplays,
            chat_core::account::PairDirection::ExistingDisplays => Self::ExistingDisplays,
        }
    }
}
impl From<chat_core::account::PairPhase> for crate::api::account::PairPhase {
    fn from(v: chat_core::account::PairPhase) -> Self {
        match v {
            chat_core::account::PairPhase::WaitingQr => Self::WaitingQr,
            chat_core::account::PairPhase::Comparing => Self::Comparing,
            chat_core::account::PairPhase::Approved => Self::Approved,
            chat_core::account::PairPhase::AwaitingApproval => Self::AwaitingApproval,
            chat_core::account::PairPhase::DisclosurePossible => Self::DisclosurePossible,
            chat_core::account::PairPhase::ReceiptReady => Self::ReceiptReady,
            chat_core::account::PairPhase::ReceiptVerified => Self::ReceiptVerified,
            chat_core::account::PairPhase::PublishPending => Self::PublishPending,
            chat_core::account::PairPhase::Active => Self::Active,
            chat_core::account::PairPhase::Observed => Self::Observed,
        }
    }
}
impl From<crate::api::account::PairDirection> for chat_core::account::PairDirection {
    fn from(v: crate::api::account::PairDirection) -> Self {
        match v {
            crate::api::account::PairDirection::NewDisplays => Self::NewDisplays,
            crate::api::account::PairDirection::ExistingDisplays => Self::ExistingDisplays,
        }
    }
}
impl From<chat_core::account::AccountView> for crate::api::account::AccountView {
    fn from(v: chat_core::account::AccountView) -> Self {
        Self {
            revision: v.revision,
            phase: v.phase.into(),
            lock_error: v.lock_error.map(Into::into),
            public_peer_id: v.public_peer_id.map(|p| p.0.to_vec()),
            installation: v.installation.map(|p| p.0.to_vec()),
            devices: v.devices.into_iter().map(Into::into).collect(),
            contacts: v.contacts.into_iter().map(Into::into).collect(),
            pair: v.pair.map(Into::into),
            publish_intents: v.publish_intents.into_iter().map(Into::into).collect(),
            publication: v.publication.into(),
            disclosure_possible: v.disclosure_possible,
            pending_pair_transactions: v
                .pending_pair_transactions
                .into_iter()
                .map(|p| p.0.to_vec())
                .collect(),
            pending_import_requires_remote_revoke: v.pending_import_requires_remote_revoke,
        }
    }
}
impl From<chat_core::account::RecordDevice> for crate::api::account::DeviceView {
    fn from(v: chat_core::account::RecordDevice) -> Self {
        Self {
            installation_id: v.installation_id.0.to_vec(),
            transport_public_key: v.transport_public_key.to_vec(),
            kind: v.kind.into(),
        }
    }
}
impl From<chat_core::account::IdentityRecord> for crate::api::account::RecordView {
    fn from(v: chat_core::account::IdentityRecord) -> Self {
        Self {
            generation: v.generation.0.to_vec(),
            issued_at: v.issued_at,
            expires_at: v.expires_at,
            devices: v.devices.into_iter().map(Into::into).collect(),
        }
    }
}
impl From<chat_core::account::ContactView> for crate::api::account::ContactView {
    fn from(v: chat_core::account::ContactView) -> Self {
        Self {
            peer_id: v.peer_id.0.to_vec(),
            alias: v.alias,
            verified: v.verified,
            new_device: v.new_device,
            device_unverified: v.device_unverified,
        }
    }
}
impl From<chat_core::account::PairView> for crate::api::account::PairView {
    fn from(v: chat_core::account::PairView) -> Self {
        Self {
            flow: v.flow.0.to_vec(),
            direction: v.direction.into(),
            phase: v.phase.into(),
            sas: v.sas,
            expires_at: v.expires_at,
            disclosure_possible: v.disclosure_possible,
            transaction: v.transaction.map(|t| t.0.to_vec()),
        }
    }
}
impl From<chat_core::account::RevokeIntent> for crate::api::account::RevocationView {
    fn from(v: chat_core::account::RevokeIntent) -> Self {
        Self {
            peer_id: v.peer_id.0.to_vec(),
            generation: v.generation.0.to_vec(),
            base_hash: v.base_hash.to_vec(),
            target_installation: v.target_installation.0.to_vec(),
            target_transport: v.target_transport.to_vec(),
            nonce: v.nonce.to_vec(),
            issued_at: v.issued_at,
        }
    }
}
impl From<chat_core::account::PublicationView> for crate::api::account::PublicationView {
    fn from(v: chat_core::account::PublicationView) -> Self {
        Self {
            state: v.state.into(),
            generation: v.generation.map(|g| g.0.to_vec()),
            observed_seq: v.observed_seq,
            allocated_seq: v.allocated_seq,
            accepted_hash: v.accepted_hash.map(|h| h.to_vec()),
            accepted: v.accepted.map(Into::into),
            conflict_hash: v.conflict_hash.map(|h| h.to_vec()),
            conflict: v.conflict.map(Into::into),
            tombstones: v.tombstones.into_iter().map(Into::into).collect(),
            pending_count: v.pending_count as u32,
            pending_notifications: v
                .pending_notifications
                .into_iter()
                .map(Into::into)
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_status_and_cache_are_distinct_public_events() {
        assert_eq!(ChatEvent::from(chat_core::CoreEvent::AccountAuthChanged{peer_id:"account".into(),installation_id:"installation".into(),ready:false}),ChatEvent::AccountAuthChanged{peer_id:"account".into(),installation_id:"installation".into(),ready:false});
        assert_eq!(ChatEvent::from(chat_core::CoreEvent::AccountCacheUsed{peer_id:"account".into()}),ChatEvent::AccountCacheUsed{peer_id:"account".into()});
    }
    #[test]
    fn connection_state_keeps_every_field() {
        let event = chat_core::CoreEvent::ConnectionState(chat_core::ConnectionSnapshot {
            direct_peers: 2,
            mailbox_ok: true,
            queue_ok: Some(false),
        });
        assert_eq!(
            ChatEvent::from(event),
            ChatEvent::ConnectionState { direct_peers: 2, mailbox_ok: true, queue_ok: Some(false) }
        );
    }

    #[test]
    fn network_snapshot_keeps_revision_and_peer() {
        let peer = chat_core::PeerInfo {
            peer_id: "a".repeat(64),
            phase: chat_core::PeerPhase::Connected,
            path: chat_core::PeerPath::Relay,
            connect_ms: Some(12),
            direct_ms: None,
        };
        let info = chat_core::NetworkInfo {
            revision: 7,
            state: chat_core::NetworkState::Online,
            endpoint_id: None,
            ticket: None,
            peers: vec![peer],
        };
        let ChatEvent::NetworkChanged { info } =
            ChatEvent::from(chat_core::CoreEvent::NetworkChanged(info)) else {
                panic!("wrong event")
            };
        assert_eq!(info.revision, 7);
        assert_eq!(info.state, NetworkState::Online);
        assert_eq!(info.endpoint_id, None);
        assert_eq!(info.ticket, None);
        assert_eq!(info.peers, vec![PeerInfo {
            peer_id: "a".repeat(64),
            phase: PeerPhase::Connected,
            path: PeerPath::Relay,
            connect_ms: Some(12),
            direct_ms: None,
        }]);
        let identified = NetworkInfo::from(chat_core::NetworkInfo {
            revision: 8,
            state: chat_core::NetworkState::Starting,
            endpoint_id: Some("b".repeat(64)),
            ticket: Some("ticket".into()),
            peers: vec![],
        });
        assert_eq!(identified.endpoint_id, Some("b".repeat(64)));
        assert_eq!(identified.ticket.as_deref(), Some("ticket"));
        assert!(identified.peers.is_empty());
    }

    #[test]
    fn peer_changed_keeps_revision_and_every_field() {
        let peer = chat_core::PeerInfo {
            peer_id: "c".repeat(64),
            phase: chat_core::PeerPhase::Reconnecting,
            path: chat_core::PeerPath::Direct,
            connect_ms: None,
            direct_ms: Some(25),
        };
        assert_eq!(
            ChatEvent::from(chat_core::CoreEvent::PeerChanged { revision: u64::MAX, peer }),
            ChatEvent::PeerChanged {
                revision: u64::MAX,
                peer: PeerInfo {
                    peer_id: "c".repeat(64),
                    phase: PeerPhase::Reconnecting,
                    path: PeerPath::Direct,
                    connect_ms: None,
                    direct_ms: Some(25),
                },
            }
        );
    }

    #[test]
    fn probe_text_keeps_peer_and_unicode_body() {
        assert_eq!(
            ChatEvent::from(chat_core::CoreEvent::ProbeTextReceived {
                peer_id: "d".repeat(64),
                text: "\u{c548}\u{b155} \u{1f30f}".into(),
            }),
            ChatEvent::ProbeTextReceived {
                peer_id: "d".repeat(64),
                text: "\u{c548}\u{b155} \u{1f30f}".into(),
            }
        );
    }

    #[test]
    fn every_network_enum_keeps_its_variant() {
        for (core, ffi) in [
            (chat_core::NetworkState::Stopped, NetworkState::Stopped),
            (chat_core::NetworkState::Starting, NetworkState::Starting),
            (chat_core::NetworkState::Online, NetworkState::Online),
            (chat_core::NetworkState::Failed, NetworkState::Failed),
        ] {
            assert_eq!(NetworkState::from(core), ffi);
        }
        for (core, ffi) in [
            (chat_core::PeerPhase::Connecting, PeerPhase::Connecting),
            (chat_core::PeerPhase::Connected, PeerPhase::Connected),
            (chat_core::PeerPhase::Reconnecting, PeerPhase::Reconnecting),
            (chat_core::PeerPhase::Disconnected, PeerPhase::Disconnected),
        ] {
            assert_eq!(PeerPhase::from(core), ffi);
        }
        for (core, ffi) in [
            (chat_core::PeerPath::Unknown, PeerPath::Unknown),
            (chat_core::PeerPath::Relay, PeerPath::Relay),
            (chat_core::PeerPath::Direct, PeerPath::Direct),
        ] {
            assert_eq!(PeerPath::from(core), ffi);
        }
    }

    #[test]
    fn every_error_code_keeps_its_variant() {
        use chat_core::ErrorCode as C;
        use crate::api::chat::ErrorCode as F;
        let pairs = [
            (C::InvalidConfig, F::InvalidConfig),
            (C::AlreadyStartedDifferentConfig, F::AlreadyStartedDifferentConfig),
            (C::CoreNotStarted, F::CoreNotStarted),
            (C::NetworkNotStarted, F::NetworkNotStarted),
            (C::InvalidPeerAddress, F::InvalidPeerAddress),
            (C::SelfConnection, F::SelfConnection),
            (C::PeerNotConnected, F::PeerNotConnected),
            (C::MessageTooLarge, F::MessageTooLarge),
            (C::Busy, F::Busy), (C::NetworkTimeout, F::NetworkTimeout),
            (C::TransportFailed, F::TransportFailed),
            (C::NotAvailable, F::NotAvailable),
            (C::InvalidRecovery, F::InvalidRecovery),
            (C::StorageLocked, F::StorageLocked),
            (C::StorageMissing, F::StorageMissing),
            (C::StorageCorrupt, F::StorageCorrupt),
            (C::StorageFull, F::StorageFull),
            (C::AccountExists, F::AccountExists),
            (C::WriterBusy, F::WriterBusy),
            (C::InvalidInvite, F::InvalidInvite),
            (C::InviteExpired, F::InviteExpired),
            (C::DeviceLimit, F::DeviceLimit),
            (C::PairMismatch, F::PairMismatch),
            (C::PairExpired, F::PairExpired),
            (C::RecordConflict, F::RecordConflict),
            (C::RecordUnavailable, F::RecordUnavailable),
            (C::AuthFailed, F::AuthFailed),
            (C::AccountRestricted, F::AccountRestricted),
        ];
        for (core, ffi) in pairs {
            assert_eq!(F::from(core), ffi);
            let converted = CoreError::from(chat_core::CoreError::new(core, "safe detail"));
            assert_eq!(converted.code, ffi);
            assert_eq!(converted.detail, "safe detail");
            assert_eq!(
                ChatEvent::from(chat_core::CoreEvent::Error {
                    code: core, detail: "safe detail".into(),
                }),
                ChatEvent::Error { code: ffi, detail: "safe detail".into() }
            );
        }
    }
}
