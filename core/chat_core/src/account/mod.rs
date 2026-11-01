pub(crate) mod auth;
pub(crate) mod connection;
mod certificate;
pub mod clock;
mod creation;
mod devices;
mod keys;
mod pkarr_backend;
mod publication;
mod record;
mod recovery;
pub mod storage;
mod types;
mod wire;
pub use devices::{RevocationNotifier, RevokeIntent};
pub use pkarr_backend::{Lookup, PkarrBackend, RecordBackend, RecordFuture};
pub use publication::{AccountServices, PublicationState, PublicationView};

pub use certificate::{InstallationCert, sign_cert, verify_cert};
pub use record::{
    IdentityRecord, MAX_ACCOUNT_PACKET_BYTES, RECORD_LIFETIME_SECONDS, RECORD_TTL_SECONDS,
    RecordDevice, parse_record, record_hash, record_packet,
};

pub use keys::{
    AccountKeys, DeviceKeys, decode_recovery, derive_account, derive_device, encode_recovery,
    random_secret,
};
pub use types::{DeviceKind, FlowId, Generation, InstallationId, PeerId, Secret32, StoreId, TxId};

mod manager;
pub use creation::RecoveryDisplay;
pub use manager::{AccountHandle, AccountPhase, AccountView, CandidateInfo, PublishIntent};

mod invite;
mod contacts;
mod safety;
pub use invite::parse_invite;
pub use types::{EndpointHint, InvitePreview, ContactView, SafetyDisplay};
pub use safety::{safety_digest, safety_display};

pub(crate) mod pair;
pub use pair::{PairDirection, PairDisplay, PairPhase, PairView, PairRole, PairResumeView};
#[cfg(all(feature="test-hooks",debug_assertions))]
pub use pair::PairTestStop;

pub use connection::RemoteRecordView;

#[cfg(all(feature="test-hooks",debug_assertions))]
pub use manager::{MessageMutation,MessagePause,MessagePoint};
