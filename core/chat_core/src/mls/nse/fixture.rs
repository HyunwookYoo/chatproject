//! Builds a conversation the way the app would hold it, for tools and tests: a sender S and
//! a receiver R in one group with `n` members, R's `mls_kv` rows in an in-memory database,
//! and ciphertexts from S that R's state has not seen.
//!
//! The group is the one spike B measured (rev 3): S creates it with R, then adds the other
//! members 50 per commit, R follows through six epochs and 200 messages. Everything S and R
//! do goes through the same `group` functions the app uses. Only the members that never
//! process anything are made directly, so they can carry a padded credential.

use openmls::prelude::tls_codec::Serialize as _;
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use rusqlite::{Connection, params};

use super::export_snapshot;
use crate::account::{PeerId, Secret32, derive_device};
use crate::message::types::{ConversationId, MessageId};
use crate::mls::MlsProvider;
use crate::mls::group::{self, Opened};
use crate::mls::identity::{MlsCredential, mls_signer};
use crate::mls::kv::KvStore;
use crate::mls::payload::{ChatMessage, Content};

const CS: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;
/// The one conversation the fixture builds.
pub const CONVERSATION: ConversationId = ConversationId([5; 32]);
/// Members are added this many per commit (spike B).
const ADD_BATCH: usize = 50;
/// Text bodies are this long, so a sealed message is one 1 KiB bucket.
const BODY_BYTES: usize = 800;

struct Party {
    provider: MlsProvider,
    signer: SignatureKeyPair,
    credential: MlsCredential,
}

impl Party {
    fn new(seed: u8, store: KvStore) -> Self {
        let device = derive_device(Secret32::from([seed; 32])).expect("device keys");
        Self {
            provider: MlsProvider::new(store),
            signer: mls_signer(&device),
            credential: MlsCredential::issue(PeerId([seed; 32]), &device),
        }
    }

    fn with_group<T>(&self, group_id: &[u8], f: impl FnOnce(&mut MlsGroup) -> T) -> T {
        let mut group = MlsGroup::load(self.provider.storage(), &GroupId::from_slice(group_id))
            .expect("group state")
            .expect("group exists");
        f(&mut group)
    }

    fn seal(&self, group_id: &[u8], seq: usize) -> Vec<u8> {
        group::seal(&self.provider, &self.signer, group_id, &message(seq)).expect("seal")
    }

    /// The receiver processes a Commit.
    fn follow(&self, group_id: &[u8], commit: &[u8]) {
        assert!(matches!(
            group::open(&self.provider, group_id, commit),
            Ok(Opened::Commit { .. })
        ));
    }
}

/// The text message with number `seq`; its body starts `M1b probe message {seq} `.
pub(super) fn message(seq: usize) -> ChatMessage {
    let mut body = format!("M1b probe message {seq} ");
    while body.len() < BODY_BYTES {
        body.push('.');
    }
    ChatMessage {
        msg_id: MessageId([(seq % 251) as u8; 16]),
        lamport: seq as u64,
        prev_hash: [0; 32],
        sent_at_ms: 1_700_000_000_000 + (seq as u64 % 1_000_000),
        content: Content::Text { body },
    }
}

/// A member that never processes anything. Its credential identity is padded to
/// `credential_bytes`, which sizes the ratchet tree like real credentials do.
fn member_key_package(index: usize, credential_bytes: usize) -> KeyPackage {
    let provider = MlsProvider::new(KvStore::from_rows(std::iter::empty()).expect("empty store"));
    let signer = SignatureKeyPair::new(CS.signature_algorithm()).expect("signature key");
    let mut identity = format!("M{index}").into_bytes();
    identity.resize(credential_bytes.max(identity.len()), b'.');
    let credential = CredentialWithKey {
        credential: BasicCredential::new(identity).into(),
        signature_key: signer.public().into(),
    };
    KeyPackage::builder()
        .build(CS, &provider, &signer, credential)
        .expect("member key package")
        .key_package()
        .clone()
}

/// The receiver's database and what the sender produced after the receiver's state.
pub(super) struct Built {
    pub receiver: Connection,
    pub group_id: Vec<u8>,
    /// `k` messages sealed after R's state; message `i` is `message(i)`.
    pub ciphertexts: Vec<Vec<u8>>,
    /// A message sealed before R's last six epochs: outside the retained window.
    pub stale: Vec<u8>,
    /// S's Commit that moves the group one epoch past R's state.
    pub commit: Vec<u8>,
    /// A message sealed in the epoch after that Commit.
    pub next: Vec<u8>,
}

pub(super) fn build_parties(n: usize, k: usize, credential_bytes: usize) -> Built {
    assert!(n >= 2, "a group needs S and R");
    let conn = Connection::open_in_memory().expect("in-memory database");
    conn.execute_batch(include_str!("../../storage/schema.sql"))
        .expect("schema");
    let s = Party::new(
        1,
        KvStore::from_rows(std::iter::empty()).expect("empty store"),
    );
    let r = Party::new(
        2,
        KvStore::load(&conn, CONVERSATION.0).expect("empty store"),
    );

    let key_package =
        group::new_key_package(&r.provider, &r.signer, &r.credential).expect("key package");
    let created = group::create_group(
        &s.provider,
        &s.signer,
        &s.credential,
        CONVERSATION,
        &[key_package],
        &|_, _| Ok(()),
    )
    .expect("create group");
    let group_id = created.group_id.clone();
    let joined = group::join(&r.provider, &created.welcome, &|_, _| Ok(())).expect("join");
    assert_eq!(joined.group_id, group_id);

    let members: Vec<KeyPackage> = (2..n)
        .map(|index| member_key_package(index, credential_bytes))
        .collect();
    for batch in members.chunks(ADD_BATCH) {
        let commit = s.with_group(&group_id, |group| {
            let (commit, _welcome, _) = group
                .add_members(&s.provider, &s.signer, batch)
                .expect("add members");
            group.merge_pending_commit(&s.provider).expect("merge");
            commit.tls_serialize_detached().expect("commit encoding")
        });
        r.follow(&group_id, &commit);
    }

    let self_update = |s: &Party| {
        s.with_group(&group_id, |group| {
            let bundle = group
                .self_update(&s.provider, &s.signer, LeafNodeParameters::default())
                .expect("self update");
            group.merge_pending_commit(&s.provider).expect("merge");
            bundle
                .commit()
                .tls_serialize_detached()
                .expect("commit encoding")
        })
    };
    let stale = s.seal(&group_id, usize::MAX);
    for _ in 0..6 {
        r.follow(&group_id, &self_update(&s));
    }
    // 200 messages, of which R opens the last: its ratchet is then 199 generations ahead.
    let mut last = Vec::new();
    for _ in 0..200 {
        last = s.seal(&group_id, usize::MAX - 1);
    }
    assert!(matches!(
        group::open(&r.provider, &group_id, &last),
        Ok(Opened::Application { .. })
    ));

    // R's database as the app would have left it.
    let epoch = r.with_group(&group_id, |group| group.epoch().as_u64());
    let tx = conn.unchecked_transaction().expect("transaction");
    tx.execute(
        "INSERT INTO conversation VALUES(?1,?2,0,?3,0,0)",
        params![
            CONVERSATION.0.as_slice(),
            [2u8; 32].as_slice(),
            [0u8; 32].as_slice()
        ],
    )
    .expect("conversation row");
    r.provider
        .storage()
        .flush(&tx, CONVERSATION.0)
        .expect("flush");
    tx.execute(
        "INSERT INTO mls_conversation(conversation,group_id,first_commit_hash,epoch,own_commit_unacked,rejoin_count,rejoin_after_ms,peer_mls) VALUES(?1,?2,?3,?4,0,0,0,1)",
        params![
            CONVERSATION.0.as_slice(),
            group_id,
            created.first_commit_hash.as_slice(),
            epoch as i64
        ],
    )
    .expect("mls_conversation row");
    tx.commit().expect("commit");

    let ciphertexts = (0..k).map(|seq| s.seal(&group_id, seq)).collect();
    let commit = self_update(&s);
    let next = s.seal(&group_id, k);
    Built {
        receiver: conn,
        group_id,
        ciphertexts,
        stale,
        commit,
        next,
    }
}

/// R's snapshot and what S produced after it.
pub struct Fixture {
    /// [`export_snapshot`] of R's conversation.
    pub snapshot: Vec<u8>,
    /// `k` messages, each one decrypting against `snapshot`; message `i` is `message(i)`.
    pub ciphertexts: Vec<Vec<u8>>,
    /// A message from an epoch R's retained window no longer holds.
    pub stale: Vec<u8>,
    /// A Commit from S: not an application message.
    pub commit: Vec<u8>,
    /// A message from the epoch after that Commit, which `snapshot` has not reached.
    pub next: Vec<u8>,
}

/// A group of `n` members, R's snapshot and `k` ciphertexts from S that the snapshot has not
/// seen, so each decrypts against it, plus one ciphertext of each kind that does not.
/// Panics when the group cannot be built; this is for tools and tests.
pub fn build_full(n: usize, k: usize, credential_bytes: usize) -> Fixture {
    let built = build_parties(n, k, credential_bytes);
    Fixture {
        snapshot: export_snapshot(&built.receiver, CONVERSATION).expect("snapshot"),
        ciphertexts: built.ciphertexts,
        stale: built.stale,
        commit: built.commit,
        next: built.next,
    }
}

/// [`build_full`] without the ciphertexts that do not decrypt: `(snapshot, ciphertexts)`.
pub fn build(n: usize, k: usize, credential_bytes: usize) -> (Vec<u8>, Vec<Vec<u8>>) {
    let fixture = build_full(n, k, credential_bytes);
    (fixture.snapshot, fixture.ciphertexts)
}
