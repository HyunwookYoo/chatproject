//! The group engine: KeyPackages, group creation and Welcome, and padded seal/open.
//!
//! Every function runs on one `MlsProvider` and knows nothing about SQL. They only change the
//! provider's in-memory `KvStore`; the caller flushes it in its SQL transaction when the whole
//! unit of work succeeded, and drops the provider (then reloads) when it did not.
use openmls::framing::errors::{MessageDecryptionError, SecretTreeError};
use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::identity::MlsCredential;
use super::kv::KvError;
use super::payload::ChatMessage;
use super::{MAX_FORWARD, MAX_PAST_EPOCHS, MlsProvider, OUT_OF_ORDER};
use crate::account::InstallationId;
use crate::message::types::ConversationId;
use crate::{CoreError, ErrorCode};

type R<T> = Result<T, CoreError>;

const CIPHERSUITE: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;
/// A serialized KeyPackage is about 450 bytes; the wire limit is generous.
pub(crate) const MAX_KEY_PACKAGE_BYTES: usize = 4_096;
/// Whole-frame limit shared with the M4 transport; nothing larger is parsed.
pub(crate) const MAX_MLS_MESSAGE_BYTES: usize = 61_440;
/// Group ids are the 32-byte conversation id plus this many random bytes, so two devices that
/// create a group for the same conversation at the same time still get different groups.
const GROUP_ID_RANDOM_BYTES: usize = 16;

/// `OpenFailure::class`: the receiver's epoch window no longer holds the message (past).
pub(crate) const CLASS_PAST_EPOCH: u8 = 1;
/// A Commit this device has not seen yet is needed first (message from a future epoch).
pub(crate) const CLASS_FUTURE_EPOCH: u8 = 2;
/// Same epoch, but outside the out-of-order or forward window.
pub(crate) const CLASS_OUT_OF_ORDER: u8 = 3;
/// No group with that id.
pub(crate) const CLASS_NO_GROUP: u8 = 4;
/// Anything else (malformed, own message, tampered, not an application message or Commit).
pub(crate) const CLASS_OTHER: u8 = 5;

fn invalid(detail: &'static str) -> CoreError {
    CoreError::new(ErrorCode::InvalidMessage, detail)
}

/// The one group configuration (design 5.1 / Global Constraints): five past epochs, a 200-message
/// out-of-order window, 1000 forward, and the ratchet tree inside the Welcome.
pub(crate) fn group_config() -> MlsGroupCreateConfig {
    MlsGroupCreateConfig::builder()
        .ciphersuite(CIPHERSUITE)
        .max_past_epochs(MAX_PAST_EPOCHS)
        .sender_ratchet_configuration(SenderRatchetConfiguration::new(OUT_OF_ORDER, MAX_FORWARD))
        .use_ratchet_tree_extension(true)
        .build()
}

fn credential_with_key(
    credential: &MlsCredential,
    signer: &SignatureKeyPair,
) -> R<CredentialWithKey> {
    if credential.mls_public.as_slice() != signer.public() {
        return Err(invalid("mls credential does not match the signing key"));
    }
    Ok(CredentialWithKey {
        credential: BasicCredential::new(credential.encode()).into(),
        signature_key: signer.public().into(),
    })
}

fn group_id_of(bytes: &[u8]) -> GroupId {
    GroupId::from_slice(bytes)
}

/// A fresh KeyPackage (TLS, at most `MAX_KEY_PACKAGE_BYTES`) for this installation.
///
/// The KeyPackage's private keys are written to the provider's `KvStore`. The caller must
/// `flush` that store with the **global** scope `[0; 32]` (not a conversation's): the Welcome
/// that consumes this KeyPackage arrives later, for a conversation not yet known here, and is
/// processed with a store loaded for that conversation plus the global rows.
pub(crate) fn new_key_package(
    p: &MlsProvider,
    signer: &SignatureKeyPair,
    credential: &MlsCredential,
) -> R<Vec<u8>> {
    let bundle = KeyPackage::builder()
        .build(
            CIPHERSUITE,
            p,
            signer,
            credential_with_key(credential, signer)?,
        )
        .map_err(|_| invalid("mls key package creation failed"))?;
    let bytes = bundle
        .key_package()
        .tls_serialize_detached()
        .map_err(|_| invalid("mls key package encoding failed"))?;
    if bytes.len() > MAX_KEY_PACKAGE_BYTES {
        return Err(invalid("mls key package too large"));
    }
    Ok(bytes)
}

fn parse_key_package(p: &MlsProvider, bytes: &[u8]) -> R<KeyPackage> {
    if bytes.len() > MAX_KEY_PACKAGE_BYTES {
        return Err(invalid("mls key package too large"));
    }
    KeyPackageIn::tls_deserialize_exact(bytes)
        .map_err(|_| invalid("invalid mls key package"))?
        .validate(p.crypto(), ProtocolVersion::Mls10)
        .map_err(|_| invalid("invalid mls key package"))
}

pub(crate) struct Created {
    pub group_id: Vec<u8>,
    /// SHA-256 of the first Commit's TLS bytes; picks the winner when two groups were created
    /// for one conversation.
    pub first_commit_hash: [u8; 32],
    /// `MlsMessageOut` TLS of the Add Commit, for members that were already in the group.
    pub commit: Vec<u8>,
    /// `MlsMessageOut` TLS of the Welcome for `members`.
    pub welcome: Vec<u8>,
}

/// Creates the group and adds `members` (peer KeyPackages) with one by-value Add Commit,
/// which the creator merges immediately (epoch 1). The caller has already checked each
/// KeyPackage's leaf against the peer's account record.
///
/// The group id is `conversation || 16 random bytes`.
pub(crate) fn create_group(
    p: &MlsProvider,
    signer: &SignatureKeyPair,
    credential: &MlsCredential,
    conversation: ConversationId,
    members: &[Vec<u8>],
) -> R<Created> {
    if members.is_empty() {
        return Err(invalid("mls group needs a member"));
    }
    let key_packages = members
        .iter()
        .map(|bytes| parse_key_package(p, bytes))
        .collect::<R<Vec<_>>>()?;
    let mut group_id = conversation.0.to_vec();
    group_id.extend(
        p.rand()
            .random_vec(GROUP_ID_RANDOM_BYTES)
            .map_err(|_| invalid("mls randomness unavailable"))?,
    );
    let mut group = MlsGroup::new_with_group_id(
        p,
        signer,
        &group_config(),
        group_id_of(&group_id),
        credential_with_key(credential, signer)?,
    )
    .map_err(|_| invalid("mls group creation failed"))?;
    let (commit, welcome, _) = group
        .add_members(p, signer, &key_packages)
        .map_err(|_| invalid("mls add commit failed"))?;
    group
        .merge_pending_commit(p)
        .map_err(|_| invalid("mls commit merge failed"))?;
    let commit = commit
        .tls_serialize_detached()
        .map_err(|_| invalid("mls commit encoding failed"))?;
    let welcome = welcome
        .tls_serialize_detached()
        .map_err(|_| invalid("mls welcome encoding failed"))?;
    Ok(Created {
        group_id,
        first_commit_hash: Sha256::digest(&commit).into(),
        commit,
        welcome,
    })
}

#[derive(Debug)]
pub(crate) struct Joined {
    pub group_id: Vec<u8>,
    pub epoch: u64,
}

/// Joins the group a Welcome (`MlsMessageOut` TLS) describes. `verify(credential, signature_key)`
/// is called for every leaf in the group's tree, including this device's own; the first error
/// aborts the join and is returned.
///
/// Staging the Welcome consumes the KeyPackage in the provider's in-memory store, so after an
/// `Err` the caller must drop the provider without flushing it (the SQL rows are untouched
/// and the KeyPackage stays usable).
pub(crate) fn join(
    p: &MlsProvider,
    welcome: &[u8],
    verify: &dyn Fn(&[u8], &[u8]) -> R<()>,
) -> R<Joined> {
    if welcome.len() > MAX_MLS_MESSAGE_BYTES {
        return Err(invalid("mls welcome too large"));
    }
    let MlsMessageBodyIn::Welcome(welcome) = MlsMessageIn::tls_deserialize_exact(welcome)
        .map_err(|_| invalid("invalid mls welcome"))?
        .extract()
    else {
        return Err(invalid("not an mls welcome"));
    };
    let staged = StagedWelcome::new_from_welcome(p, group_config().join_config(), welcome, None)
        .map_err(|_| invalid("mls welcome could not be processed"))?;
    for member in staged.members() {
        let credential = BasicCredential::try_from(member.credential)
            .map_err(|_| invalid("unsupported mls credential"))?;
        verify(credential.identity(), &member.signature_key)?;
    }
    let group = staged
        .into_group(p)
        .map_err(|_| invalid("mls welcome could not be joined"))?;
    Ok(Joined {
        group_id: group.group_id().to_vec(),
        epoch: group.epoch().as_u64(),
    })
}

/// Pads and encrypts `message` for the group (`MlsMessageOut` TLS).
pub(crate) fn seal(
    p: &MlsProvider,
    signer: &SignatureKeyPair,
    group_id: &[u8],
    message: &ChatMessage,
) -> R<Vec<u8>> {
    let plaintext = message.encode_padded()?;
    let mut group = MlsGroup::load(p.storage(), &group_id_of(group_id))
        .map_err(|_| invalid("mls group state unreadable"))?
        .ok_or_else(|| CoreError::new(ErrorCode::NotAvailable, "mls group not found"))?;
    group
        .create_message(p, signer, &plaintext)
        .map_err(|_| invalid("mls seal failed"))?
        .tls_serialize_detached()
        .map_err(|_| invalid("mls message encoding failed"))
}

pub(crate) enum Opened {
    Application {
        sender: InstallationId,
        message: ChatMessage,
    },
    /// A Commit was applied; the group is now at `epoch`.
    Commit { epoch: u64 },
}

impl std::fmt::Debug for Opened {
    /// Never prints message content.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Application { sender, message } => f
                .debug_struct("Application")
                .field("sender", sender)
                .field("message", message)
                .finish(),
            Self::Commit { epoch } => f.debug_struct("Commit").field("epoch", epoch).finish(),
        }
    }
}

/// Why `open` failed; `class` is the kind 17 `DecryptFailed.class` number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpenFailure {
    pub class: u8,
}

const fn fail(class: u8) -> OpenFailure {
    OpenFailure { class }
}

fn classify(error: &ProcessMessageError<KvError>, message_epoch: u64, current: u64) -> u8 {
    if message_epoch > current {
        return CLASS_FUTURE_EPOCH;
    }
    // OpenMLS reports an epoch that has fallen out of the retained window as a too-old
    // generation of the current ratchet, so the window is checked here.
    if message_epoch.saturating_add(MAX_PAST_EPOCHS as u64) < current {
        return CLASS_PAST_EPOCH;
    }
    match error {
        ProcessMessageError::ValidationError(
            ValidationError::NoPastEpochData | ValidationError::WrongEpoch,
        ) => CLASS_PAST_EPOCH,
        ProcessMessageError::ValidationError(ValidationError::UnableToDecrypt(
            MessageDecryptionError::GenerationOutOfBound
            | MessageDecryptionError::SecretTreeError(
                SecretTreeError::TooDistantInThePast
                | SecretTreeError::TooDistantInTheFuture
                | SecretTreeError::SecretReuseError,
            ),
        )) => CLASS_OUT_OF_ORDER,
        _ => CLASS_OTHER,
    }
}

/// Decrypts an application message, or applies a Commit, for `group_id`.
///
/// A Commit is merged without vetting its new members; callers that must vet them use
/// `membership::apply_commit`. A message this device sealed itself cannot be opened
/// (`CLASS_OTHER`), and a failed call leaves nothing for the caller to keep: drop the provider.
pub(crate) fn open(
    p: &MlsProvider,
    group_id: &[u8],
    ciphertext: &[u8],
) -> Result<Opened, OpenFailure> {
    if ciphertext.len() > MAX_MLS_MESSAGE_BYTES {
        return Err(fail(CLASS_OTHER));
    }
    let protocol: ProtocolMessage = MlsMessageIn::tls_deserialize_exact(ciphertext)
        .ok()
        .and_then(|message| message.try_into_protocol_message().ok())
        .ok_or(fail(CLASS_OTHER))?;
    let mut group = MlsGroup::load(p.storage(), &group_id_of(group_id))
        .map_err(|_| fail(CLASS_OTHER))?
        .ok_or(fail(CLASS_NO_GROUP))?;
    if protocol.group_id() != group.group_id() {
        return Err(fail(CLASS_OTHER));
    }
    let (message_epoch, current) = (protocol.epoch().as_u64(), group.epoch().as_u64());
    let processed = group
        .process_message(p, protocol)
        .map_err(|error| fail(classify(&error, message_epoch, current)))?;
    let sender = processed.credential().clone();
    match processed.into_content() {
        ProcessedMessageContent::ApplicationMessage(application) => {
            let plaintext = Zeroizing::new(application.into_bytes());
            let message = ChatMessage::decode_padded(&plaintext).map_err(|_| fail(CLASS_OTHER))?;
            let installation = BasicCredential::try_from(sender)
                .ok()
                .and_then(|basic| MlsCredential::decode(basic.identity()).ok())
                .map(|credential| credential.installation)
                .ok_or(fail(CLASS_OTHER))?;
            Ok(Opened::Application {
                sender: installation,
                message,
            })
        }
        ProcessedMessageContent::StagedCommitMessage(staged) => {
            group
                .merge_staged_commit(p, *staged)
                .map_err(|_| fail(CLASS_OTHER))?;
            Ok(Opened::Commit {
                epoch: group.epoch().as_u64(),
            })
        }
        _ => Err(fail(CLASS_OTHER)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{
        DeviceKeys, DeviceKind, Generation, IdentityRecord, PeerId, RecordDevice, Secret32,
        derive_device,
    };
    use crate::message::types::MessageId;
    use crate::message::wire::ReceiptKind;
    use crate::mls::identity::{mls_signer, verify_leaf};
    use crate::mls::kv::KvStore;
    use crate::mls::payload::{Content, MAX_BUCKET, PUSH_BUCKET};
    use rusqlite::Connection;

    const CONV: ConversationId = ConversationId([5; 32]);
    const GLOBAL: [u8; 32] = [0; 32];

    fn database() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        c.execute_batch(include_str!("../storage/schema.sql"))
            .unwrap();
        c
    }

    fn record(device: &DeviceKeys) -> IdentityRecord {
        IdentityRecord {
            generation: Generation([1; 16]),
            issued_at: 10,
            expires_at: 20,
            devices: vec![RecordDevice {
                installation_id: device.installation_id(),
                transport_public_key: device.transport_public_key(),
                kind: DeviceKind::Mobile,
            }],
        }
    }

    /// One installation: its signing key, credential and a `KvStore`-backed provider over its
    /// own SQLite database.
    struct Party {
        conn: Connection,
        provider: MlsProvider,
        device: DeviceKeys,
        signer: SignatureKeyPair,
        credential: MlsCredential,
        peer: PeerId,
    }
    impl Party {
        fn new(seed: u8) -> Self {
            let device = derive_device(Secret32::from([seed; 32])).unwrap();
            let peer = PeerId([seed; 32]);
            let conn = database();
            Self {
                provider: MlsProvider::new(KvStore::load(&conn, CONV.0).unwrap()),
                signer: mls_signer(&device),
                credential: MlsCredential::issue(peer, &device),
                device,
                peer,
                conn,
            }
        }
        fn key_package(&self) -> Vec<u8> {
            new_key_package(&self.provider, &self.signer, &self.credential).unwrap()
        }
        fn installation(&self) -> InstallationId {
            self.device.installation_id()
        }
        fn create(&self, members: &[Vec<u8>]) -> Created {
            create_group(
                &self.provider,
                &self.signer,
                &self.credential,
                CONV,
                members,
            )
            .unwrap()
        }
        fn seal(&self, group_id: &[u8], message: &ChatMessage) -> Vec<u8> {
            seal(&self.provider, &self.signer, group_id, message).unwrap()
        }
        fn open(&self, group_id: &[u8], ciphertext: &[u8]) -> Result<Opened, OpenFailure> {
            open(&self.provider, group_id, ciphertext)
        }
        /// Commits the key package to its own database under `scope`.
        fn flush(&self, scope: [u8; 32]) {
            let tx = self.conn.unchecked_transaction().unwrap();
            self.provider.storage().flush(&tx, scope).unwrap();
            tx.commit().unwrap();
        }
    }

    /// A verifier that checks each leaf with `verify_leaf` against the record of its own peer.
    fn verifier(parties: &[&Party]) -> impl Fn(&[u8], &[u8]) -> R<()> {
        let records: Vec<(PeerId, IdentityRecord)> = parties
            .iter()
            .map(|party| (party.peer, record(&party.device)))
            .collect();
        move |credential, key| {
            let peer = MlsCredential::decode(credential)?.peer;
            let (_, record) = records
                .iter()
                .find(|(candidate, _)| *candidate == peer)
                .ok_or_else(|| invalid("unknown peer"))?;
            verify_leaf(credential, key, peer, record).map(|_| ())
        }
    }

    /// A creates the group and invites B; B joins.
    fn pair() -> (Party, Party, Vec<u8>) {
        let (a, b) = (Party::new(1), Party::new(2));
        let created = a.create(&[b.key_package()]);
        let joined = join(&b.provider, &created.welcome, &verifier(&[&a, &b])).unwrap();
        assert_eq!(joined.group_id, created.group_id);
        assert_eq!(joined.epoch, 1);
        (a, b, created.group_id)
    }

    fn chat(n: u8) -> ChatMessage {
        ChatMessage {
            msg_id: MessageId([n; 16]),
            lamport: u64::from(n),
            prev_hash: [n; 32],
            sent_at_ms: 1_700_000_000_000 + u64::from(n),
            content: Content::Text {
                body: format!("message {n}"),
            },
        }
    }
    fn text(body_len: usize) -> ChatMessage {
        ChatMessage {
            content: Content::Text {
                body: "x".repeat(body_len),
            },
            ..chat(1)
        }
    }

    fn opened(party: &Party, group_id: &[u8], ciphertext: &[u8]) -> ChatMessage {
        match party.open(group_id, ciphertext).unwrap() {
            Opened::Application { message, .. } => message,
            other => panic!("expected an application message, got {other:?}"),
        }
    }

    /// `committer` self-updates, producing a Commit it has already merged.
    fn commit_only(committer: &Party, group_id: &[u8]) -> Vec<u8> {
        let mut group =
            MlsGroup::load(committer.provider.storage(), &GroupId::from_slice(group_id))
                .unwrap()
                .unwrap();
        let bundle = group
            .self_update(
                &committer.provider,
                &committer.signer,
                LeafNodeParameters::default(),
            )
            .unwrap();
        group.merge_pending_commit(&committer.provider).unwrap();
        bundle.commit().tls_serialize_detached().unwrap()
    }
    /// One epoch step: A commits, B applies the Commit.
    fn advance(a: &Party, b: &Party, group_id: &[u8]) {
        let commit = commit_only(a, group_id);
        assert!(matches!(
            b.open(group_id, &commit).unwrap(),
            Opened::Commit { .. }
        ));
    }

    #[test]
    fn two_devices_create_join_seal_open() {
        let (a, b, gid) = pair();

        let ct = a.seal(&gid, &chat(1));
        match b.open(&gid, &ct).unwrap() {
            Opened::Application { sender, message } => {
                assert_eq!(sender, a.installation());
                assert_eq!(message, chat(1));
            }
            other => panic!("{other:?}"),
        }
        let reply = ChatMessage {
            content: Content::Receipt {
                kind: ReceiptKind::Read,
                targets: vec![MessageId([1; 16])],
            },
            ..chat(2)
        };
        let ct2 = b.seal(&gid, &reply);
        match a.open(&gid, &ct2).unwrap() {
            Opened::Application { sender, message } => {
                assert_eq!(sender, b.installation());
                assert_eq!(message, reply);
            }
            other => panic!("{other:?}"),
        }
        // A device cannot open the message it sealed itself (OwnPrivateMessage).
        assert_eq!(a.open(&gid, &ct).unwrap_err().class, CLASS_OTHER);
        assert_eq!(b.open(&gid, &ct2).unwrap_err().class, CLASS_OTHER);
    }

    #[test]
    fn group_id_starts_with_the_conversation() {
        let (_a, _b, gid) = pair();
        assert_eq!(&gid[..32], &CONV.0);
        assert_eq!(gid.len(), 32 + GROUP_ID_RANDOM_BYTES);
        // Two groups for one conversation never share an id.
        let (_, _, other) = pair();
        assert_ne!(gid, other);
    }

    #[test]
    fn first_commit_hash_is_sha256_of_the_commit() {
        let (a, b) = (Party::new(1), Party::new(2));
        let created = a.create(&[b.key_package()]);
        assert_eq!(
            created.first_commit_hash,
            <[u8; 32]>::from(Sha256::digest(&created.commit))
        );
    }

    #[test]
    fn key_package_is_small_and_checked() {
        let a = Party::new(1);
        let kp = a.key_package();
        assert!(kp.len() <= MAX_KEY_PACKAGE_BYTES, "{} B", kp.len());
        // A credential for another key is refused.
        let other = Party::new(2);
        assert!(new_key_package(&a.provider, &a.signer, &other.credential).is_err());
        // create_group refuses garbage, an oversized blob and an empty member list.
        assert!(
            create_group(
                &a.provider,
                &a.signer,
                &a.credential,
                CONV,
                &[vec![1, 2, 3]]
            )
            .is_err()
        );
        assert!(
            create_group(
                &a.provider,
                &a.signer,
                &a.credential,
                CONV,
                &[vec![0; MAX_KEY_PACKAGE_BYTES + 1]]
            )
            .is_err()
        );
        assert!(create_group(&a.provider, &a.signer, &a.credential, CONV, &[]).is_err());
    }

    #[test]
    fn join_verifies_every_leaf() {
        let (a, b) = (Party::new(1), Party::new(2));
        let created = a.create(&[b.key_package()]);
        let seen = std::cell::RefCell::new(Vec::new());
        let record_leaf = |credential: &[u8], key: &[u8]| -> R<()> {
            seen.borrow_mut().push((credential.to_vec(), key.to_vec()));
            Ok(())
        };
        join(&b.provider, &created.welcome, &record_leaf).unwrap();
        let seen = seen.into_inner();
        assert_eq!(seen.len(), 2);
        for party in [&a, &b] {
            assert!(seen.contains(&(
                party.credential.encode(),
                party.credential.mls_public.to_vec()
            )));
        }
    }

    #[test]
    fn join_rejects_unverified_leaf() {
        let (a, b) = (Party::new(1), Party::new(2));
        let created = a.create(&[b.key_package()]);
        b.flush(GLOBAL);
        let a_peer = a.peer;
        let reject_a = move |credential: &[u8], _key: &[u8]| -> R<()> {
            if MlsCredential::decode(credential)?.peer == a_peer {
                return Err(CoreError::new(ErrorCode::AuthFailed, "rejected"));
            }
            Ok(())
        };
        let attempt = MlsProvider::new(KvStore::load(&b.conn, CONV.0).unwrap());
        let error = join(&attempt, &created.welcome, &reject_a).unwrap_err();
        assert_eq!(error.code, ErrorCode::AuthFailed);
        assert!(
            MlsGroup::load(attempt.storage(), &GroupId::from_slice(&created.group_id))
                .unwrap()
                .is_none(),
            "no group may exist after a rejected join"
        );
        // The rejected attempt wrote nothing: with a fresh load the same Welcome still joins
        // with the same KeyPackage.
        let retry = MlsProvider::new(KvStore::load(&b.conn, CONV.0).unwrap());
        let joined = join(&retry, &created.welcome, &verifier(&[&a, &b])).unwrap();
        assert_eq!(joined.group_id, created.group_id);
    }

    #[test]
    fn welcome_finds_key_package_flushed_globally() {
        let (a, b) = (Party::new(1), Party::new(2));
        // B's KeyPackage is created before any conversation exists; its rows go to the global
        // scope. Processing the Welcome later loads only the conversation scope plus global.
        let kp = b.key_package();
        b.flush(GLOBAL);
        let created = a.create(&[kp]);

        let scoped = MlsProvider::new(KvStore::load(&b.conn, CONV.0).unwrap());
        let joined = join(&scoped, &created.welcome, &verifier(&[&a, &b])).unwrap();
        assert_eq!(joined.group_id, created.group_id);

        // Persist the joined group under the conversation scope and reload: the state that is
        // split between the global rows and the conversation's rows is complete.
        let tx = b.conn.unchecked_transaction().unwrap();
        scoped.storage().flush(&tx, CONV.0).unwrap();
        tx.commit().unwrap();
        let reloaded = MlsProvider::new(KvStore::load(&b.conn, CONV.0).unwrap());
        let m = a.seal(&created.group_id, &chat(1));
        match open(&reloaded, &created.group_id, &m).unwrap() {
            Opened::Application { message, .. } => assert_eq!(message, chat(1)),
            other => panic!("{other:?}"),
        }

        // Control: a KeyPackage that was never flushed is not there for the Welcome.
        let c = Party::new(3);
        let created = a.create(&[c.key_package()]);
        let scoped = MlsProvider::new(KvStore::load(&c.conn, CONV.0).unwrap());
        assert!(join(&scoped, &created.welcome, &verifier(&[&a, &c])).is_err());
    }

    #[test]
    fn join_rejects_bad_welcome_input() {
        let (a, b) = (Party::new(1), Party::new(2));
        let created = a.create(&[b.key_package()]);
        let accept = |_: &[u8], _: &[u8]| -> R<()> { Ok(()) };
        assert!(join(&b.provider, &[], &accept).is_err());
        assert!(join(&b.provider, &created.commit, &accept).is_err());
        let mut truncated = created.welcome.clone();
        truncated.pop();
        assert!(join(&b.provider, &truncated, &accept).is_err());
        assert!(join(&b.provider, &vec![0; MAX_MLS_MESSAGE_BYTES + 1], &accept).is_err());
    }

    #[test]
    fn delayed_within_five_epochs_opens() {
        let (a, b, gid) = pair();
        let m = a.seal(&gid, &chat(1));
        for _ in 0..5 {
            advance(&a, &b, &gid);
        }
        assert_eq!(opened(&b, &gid, &m), chat(1));

        // One more epoch and the sender's secrets for that epoch are gone.
        let (a, b, gid) = pair();
        let m = a.seal(&gid, &chat(1));
        for _ in 0..6 {
            advance(&a, &b, &gid);
        }
        assert_eq!(b.open(&gid, &m).unwrap_err().class, CLASS_PAST_EPOCH);
    }

    #[test]
    fn out_of_order_within_200_opens() {
        let (a, b, gid) = pair();
        let sealed: Vec<Vec<u8>> = (0..=200).map(|n| a.seal(&gid, &chat(n as u8))).collect();
        // The newest first, then every older one back to 200 behind it.
        assert_eq!(opened(&b, &gid, &sealed[200]), chat(200));
        for n in (1..200).rev() {
            assert_eq!(opened(&b, &gid, &sealed[n]), chat(n as u8), "message {n}");
        }
        assert_eq!(
            b.open(&gid, &sealed[0]).unwrap_err().class,
            CLASS_OUT_OF_ORDER
        );
    }

    #[test]
    fn future_epoch_reports_missing_commit() {
        let (a, b, gid) = pair();
        let commit = commit_only(&a, &gid);
        let m = a.seal(&gid, &chat(1));
        assert_eq!(b.open(&gid, &m).unwrap_err().class, CLASS_FUTURE_EPOCH);
        // Once the Commit arrives the same ciphertext opens.
        assert!(matches!(
            b.open(&gid, &commit).unwrap(),
            Opened::Commit { epoch: 2 }
        ));
        assert_eq!(opened(&b, &gid, &m), chat(1));
    }

    #[test]
    fn open_failures_are_classified() {
        let (a, b, gid) = pair();
        let m = a.seal(&gid, &chat(1));
        assert_eq!(b.open(&[7; 48], &m).unwrap_err().class, CLASS_NO_GROUP);
        assert_eq!(b.open(&gid, &[]).unwrap_err().class, CLASS_OTHER);
        assert_eq!(b.open(&gid, &[1, 2, 3, 4]).unwrap_err().class, CLASS_OTHER);
        let mut tampered = m.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(b.open(&gid, &tampered).unwrap_err().class, CLASS_OTHER);
        assert_eq!(
            b.open(&gid, &vec![0; MAX_MLS_MESSAGE_BYTES + 1])
                .unwrap_err()
                .class,
            CLASS_OTHER
        );
        // None of that disturbed the group: the original still opens.
        assert_eq!(opened(&b, &gid, &m), chat(1));
    }

    fn app_frame(ciphertext: &[u8]) -> (usize, usize) {
        let mut e = minicbor::Encoder::new(Vec::new());
        e.array(4)
            .unwrap()
            .u8(1)
            .unwrap()
            .u8(0)
            .unwrap()
            .bytes(&CONV.0)
            .unwrap()
            .bytes(ciphertext)
            .unwrap();
        let inner = e.into_writer();
        (
            inner.len(),
            crate::account::auth::envelope(17, &inner).len(),
        )
    }

    #[test]
    fn push_bucket_ciphertext_fits_2900() {
        let (a, b, gid) = pair();
        let message = text(PUSH_BUCKET - 150);
        assert_eq!(message.encode_padded().unwrap().len(), PUSH_BUCKET);
        let ct = a.seal(&gid, &message);
        eprintln!(
            "push bucket: {PUSH_BUCKET} B plaintext -> {} B ciphertext",
            ct.len()
        );
        assert!(ct.len() <= 2_900, "{} B", ct.len());
        assert_eq!(opened(&b, &gid, &ct), message);
    }

    #[test]
    fn max_bucket_frame_fits_61440() {
        let (a, b, gid) = pair();
        let message = text(MAX_BUCKET - 150);
        assert_eq!(message.encode_padded().unwrap().len(), MAX_BUCKET);
        let ct = a.seal(&gid, &message);
        let (inner, frame) = app_frame(&ct);
        eprintln!(
            "max bucket: {MAX_BUCKET} B plaintext -> {} B ciphertext, {inner} B inner, {frame} B frame",
            ct.len()
        );
        assert!(inner <= 61_434, "inner {inner} B");
        assert!(frame <= 61_440, "frame {frame} B");
        assert_eq!(opened(&b, &gid, &ct), message);
    }

    #[test]
    fn every_bucket_seals_within_limits() {
        let (a, b, gid) = pair();
        for bucket in crate::mls::payload::BUCKETS {
            let message = text(bucket - 150);
            assert_eq!(message.encode_padded().unwrap().len(), bucket);
            let ct = a.seal(&gid, &message);
            eprintln!(
                "bucket {bucket} B -> ciphertext {} B (+{})",
                ct.len(),
                ct.len() - bucket
            );
            assert!(app_frame(&ct).1 <= 61_440);
            assert_eq!(opened(&b, &gid, &ct), message);
        }
    }

    /// Release-only measurement (run by Task 11): `cargo test --release -p chat_core --lib
    /// one_kib -- --ignored`.
    #[test]
    #[ignore]
    fn one_kib_seal_open_under_one_ms() {
        let (a, b, gid) = pair();
        let message = text(800);
        assert_eq!(message.encode_padded().unwrap().len(), 1_024);
        const ROUNDS: u32 = 200;
        let (mut seal_total, mut open_total) =
            (std::time::Duration::ZERO, std::time::Duration::ZERO);
        for _ in 0..ROUNDS {
            let start = std::time::Instant::now();
            let ct = a.seal(&gid, &message);
            seal_total += start.elapsed();
            let start = std::time::Instant::now();
            opened(&b, &gid, &ct);
            open_total += start.elapsed();
        }
        let (seal_avg, open_avg) = (seal_total / ROUNDS, open_total / ROUNDS);
        eprintln!("1 KiB seal {seal_avg:?}, open {open_avg:?} (average of {ROUNDS})");
        assert!(
            seal_avg < std::time::Duration::from_millis(1),
            "seal {seal_avg:?}"
        );
        assert!(
            open_avg < std::time::Duration::from_millis(1),
            "open {open_avg:?}"
        );
    }
}
