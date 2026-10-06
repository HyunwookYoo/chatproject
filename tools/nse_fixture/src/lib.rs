//! Builds spike B's rev3 group and exports receiver R's state plus K new ciphertexts from S.
//! The ciphertexts are at generations after R's stored ratchet, so each one decrypts
//! against the same state (the NSE never persists its writes).

use openmls::prelude::tls_codec::{Deserialize as _, Serialize as _};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_rust_crypto::OpenMlsRustCrypto;

const CS: Ciphersuite = Ciphersuite::MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519;

pub struct Fixture {
    /// `chat_nse::state` snapshot of R.
    pub state: Vec<u8>,
    /// K `MlsMessageOut` wire encodings; index i holds `plaintext(i)`.
    pub ciphertexts: Vec<Vec<u8>>,
}

struct Client {
    provider: OpenMlsRustCrypto,
    signer: SignatureKeyPair,
    credential: CredentialWithKey,
}

impl Client {
    /// `credential_bytes` pads the BasicCredential identity (an InstallationCert is about 200 B).
    fn new(name: &str, credential_bytes: usize) -> Self {
        let provider = OpenMlsRustCrypto::default();
        let signer = SignatureKeyPair::new(CS.signature_algorithm()).unwrap();
        signer.store(provider.storage()).unwrap();
        let mut identity = name.as_bytes().to_vec();
        identity.resize(credential_bytes.max(identity.len()), b'.');
        let credential = CredentialWithKey {
            credential: BasicCredential::new(identity).into(),
            signature_key: signer.public().into(),
        };
        Self { provider, signer, credential }
    }

    fn key_package(&self) -> KeyPackage {
        let bundle = KeyPackage::builder()
            .build(CS, &self.provider, &self.signer, self.credential.clone())
            .unwrap();
        bundle.key_package().clone()
    }

    fn send(&self, group: &mut MlsGroup, plaintext: &[u8]) -> Vec<u8> {
        let message = group.create_message(&self.provider, &self.signer, plaintext).unwrap();
        message.tls_serialize_detached().unwrap()
    }
}

fn process(group: &mut MlsGroup, provider: &OpenMlsRustCrypto, wire: &[u8]) -> ProcessedMessageContent {
    let message = MlsMessageIn::tls_deserialize_exact(wire).unwrap();
    group
        .process_message(provider, message.try_into_protocol_message().unwrap())
        .unwrap()
        .into_content()
}

fn receive_commit(group: &mut MlsGroup, provider: &OpenMlsRustCrypto, commit: &MlsMessageOut) {
    let ProcessedMessageContent::StagedCommitMessage(staged) =
        process(group, provider, &commit.tls_serialize_detached().unwrap())
    else {
        panic!("expected a commit")
    };
    group.merge_staged_commit(provider, *staged).unwrap();
}

/// 1 KiB of readable text, so the notification preview shows which message was decrypted.
pub fn plaintext(seq: usize) -> Vec<u8> {
    let mut p = format!("M1b probe message {seq} ").into_bytes();
    p.resize(1024, b'.');
    p
}

/// N members (S, R and N-2 that never process anything), rev3 config, spike B traffic.
pub fn build(n: usize, k: usize, credential_bytes: usize) -> Fixture {
    // Spike B rev3: max_past_epochs 5, sender ratchet (out_of_order 200, forward 1000).
    let create_config = MlsGroupCreateConfig::builder()
        .ciphersuite(CS)
        .max_past_epochs(5)
        .sender_ratchet_configuration(SenderRatchetConfiguration::new(200, 1000))
        .build();
    let join_config = create_config.join_config().clone();

    // S adds R and the others by value, 50 per commit (spike B).
    let s = Client::new("S", credential_bytes);
    let r = Client::new("R", credential_bytes);
    let mut s_group = MlsGroup::new(&s.provider, &s.signer, &create_config, s.credential.clone()).unwrap();
    let mut key_packages = vec![r.key_package()];
    key_packages.extend((2..n).map(|i| Client::new(&format!("M{i}"), credential_bytes).key_package()));
    let mut r_group: Option<MlsGroup> = None;
    for batch in key_packages.chunks(50) {
        let (commit, welcome, _) = s_group.add_members(&s.provider, &s.signer, batch).unwrap();
        s_group.merge_pending_commit(&s.provider).unwrap();
        match &mut r_group {
            Some(group) => receive_commit(group, &r.provider, &commit),
            None => {
                let welcome = MlsMessageIn::tls_deserialize_exact(welcome.tls_serialize_detached().unwrap()).unwrap();
                let MlsMessageBodyIn::Welcome(welcome) = welcome.extract() else {
                    panic!("expected a welcome")
                };
                let tree = s_group.export_ratchet_tree().into();
                let staged = StagedWelcome::new_from_welcome(&r.provider, &join_config, welcome, Some(tree)).unwrap();
                r_group = Some(staged.into_group(&r.provider).unwrap());
            }
        }
    }
    let mut r_group = r_group.unwrap();

    // Spike B traffic: 6 epochs R processes, then 200 messages of which R decrypts the last.
    for _ in 0..6 {
        let bundle = s_group.self_update(&s.provider, &s.signer, LeafNodeParameters::default()).unwrap();
        s_group.merge_pending_commit(&s.provider).unwrap();
        receive_commit(&mut r_group, &r.provider, bundle.commit());
    }
    let mut last = Vec::new();
    for _ in 0..200 {
        last = s.send(&mut s_group, &plaintext(usize::MAX));
    }
    assert!(matches!(
        process(&mut r_group, &r.provider, &last),
        ProcessedMessageContent::ApplicationMessage(_)
    ));

    // Snapshot R, then K messages that R's snapshot has not seen.
    let snapshot = r.provider.storage().values.read().unwrap().clone();
    let state = chat_nse::state::encode(s_group.group_id().as_slice(), &snapshot);
    let ciphertexts = (0..k).map(|seq| s.send(&mut s_group, &plaintext(seq))).collect();
    Fixture { state, ciphertexts }
}
