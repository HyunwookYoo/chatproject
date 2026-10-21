//! Known offline account fixtures; compiled only in the debug test facade.
use crate::account::{
    connection::RemoteRecord,
    contacts::Contact,
    pair::{
        transaction::{CandidatePinned, ExistingPin, PairTransaction, PendingImport, Stage},
        transfer::{self, Envelope, Payload, Receipt},
    },
    publication::{PacketIntent, PacketOperation, PacketStatus, RecordState, SignedRecord},
    storage::{
        StoredPlaintext,
        schema::{AccountState, LocalAccount},
    },
    *,
};
use subtle::ConstantTimeEq;

fn signed(
    keys: &AccountKeys,
    device: &DeviceKeys,
    generation: Generation,
    seq: u64,
) -> SignedRecord {
    let record = IdentityRecord {
        generation,
        issued_at: 1000,
        expires_at: 1000 + 86400,
        devices: vec![RecordDevice {
            installation_id: device.installation_id(),
            transport_public_key: device.transport_public_key(),
            kind: DeviceKind::Desktop,
        }],
    };
    SignedRecord::new(record_packet(keys, &record, seq).unwrap(), keys.peer_id()).unwrap()
}

pub fn migration_states() -> Vec<Option<StoredPlaintext>> {
    let keys = AccountKeys::from_identity(Secret32::from([17; 32]));
    let device = derive_device(Secret32::from([18; 32])).unwrap();
    let other = derive_device(Secret32::from([19; 32])).unwrap();
    let cert = sign_cert(&keys, &device, DeviceKind::Desktop, 1000).unwrap();
    let imported_cert = sign_cert(&keys, &other, DeviceKind::Desktop, 1000).unwrap();
    let pin = ExistingPin {
        peer: keys.peer_id(),
        installation: device.installation_id(),
        transport: device.transport_public_key(),
    };
    let generation = Generation([20; 16]);
    let tx = TxId([21; 16]);
    let flow = FlowId([22; 16]);
    let payload = Payload::encode_parts(tx, &keys, &imported_cert, generation, [23; 32], &[]);
    let receipt = Receipt::sign(tx, transfer::hash(&payload), &other, flow, [24; 32]);
    let records = RecordState {
        generation,
        accepted: None,
        conflict: None,
        observed_seq: 0,
        allocated_seq: 0,
        tombstones: vec![],
        pending: vec![],
        state: PublicationState::LocalBlocked,
    };
    let candidate =
        AccountState::Candidate(derive_device(Secret32::from([18; 32])).unwrap()).encode();
    let pinned = AccountState::CandidatePinned(Box::new(CandidatePinned {
        device: derive_device(Secret32::from([19; 32])).unwrap(),
        revision: 7,
        pin,
        envelope: Envelope {
            tx,
            hash: transfer::hash(&payload),
            total: payload.len(),
        },
    }))
    .encode();
    let pending = AccountState::PendingImport(Box::new(PendingImport {
        device: derive_device(Secret32::from([19; 32])).unwrap(),
        revision: 8,
        pin,
        payload,
        receipt,
        records,
    }))
    .encode();
    let mut records = RecordState {
        generation,
        accepted: Some(signed(&keys, &device, generation, 10)),
        conflict: Some(signed(&keys, &device, Generation([25; 16]), 11)),
        observed_seq: 11,
        allocated_seq: 12,
        tombstones: vec![],
        pending: vec![],
        state: PublicationState::Conflict,
    };
    records.pending.push(PacketIntent {
        signed: signed(&keys, &device, generation, 12),
        base_hash: Some(records.accepted.as_ref().unwrap().hash),
        operation: PacketOperation::Renew,
        status: PacketStatus::Held,
        revoke_nonce: None,
        recipients: None,
    });
    let target = RecordDevice {
        installation_id: other.installation_id(),
        transport_public_key: other.transport_public_key(),
        kind: DeviceKind::Desktop,
    };
    records
        .tombstones
        .push(RevokeIntent::sign(&keys, generation, [23; 32], &target, 1000).unwrap());
    let remote_keys = AccountKeys::from_identity(Secret32::from([26; 32]));
    let remote = RemoteRecord {
        peer: remote_keys.peer_id(),
        accepted: signed(&remote_keys, &other, generation, 15),
        floor: 19,
        conflict: Some(signed(&remote_keys, &other, Generation([27; 16]), 18)),
    };
    let contact = Contact {
        peer: remote_keys.peer_id(),
        alias: "fixture contact".into(),
        verified_digest: Some(safety_digest(keys.peer_id(), remote_keys.peer_id())),
        known_installation: Some(other.installation_id()),
        new_device: true,
    };
    let mut pairs = crate::account::pair::transaction::PairState::default();
    for (index, stage) in [
        (0, Stage::DisclosurePossible),
        (1, Stage::ReceiptVerified),
        (2, Stage::Observed),
    ] {
        let tx = TxId([30 + index; 16]);
        let payload = Payload::encode_parts(tx, &keys, &imported_cert, generation, [23; 32], &[]);
        let receipt = if stage == Stage::DisclosurePossible {
            None
        } else {
            Some(Receipt::sign(
                tx,
                transfer::hash(&payload),
                &other,
                flow,
                [24; 32],
            ))
        };
        pairs.existing.push(PairTransaction {
            tx,
            pin,
            payload,
            approved_flow: flow,
            approved_hash: [24; 32],
            stage,
            receipt,
            publication: if stage == Stage::Observed {
                Some([28; 32])
            } else {
                None
            },
        });
    }
    let active = AccountState::Active(Box::new(LocalAccount {
        keys,
        device,
        cert,
        revision: 29,
        intent: PublishIntent::Replacement,
        records,
        remote: vec![remote],
        contacts: vec![contact],
        pairs,
    }))
    .encode();
    let states = vec![
        None,
        Some(candidate),
        Some(pinned),
        Some(pending),
        Some(active),
    ];
    for (index, s) in states.iter().enumerate() {
        if let Some(s) = s {
            assert!(
                AccountState::decode(s).is_ok(),
                "invalid fixture phase {index}"
            );
        }
    }
    states
}

pub fn assert_account_preserved(expected: &StoredPlaintext, actual: &StoredPlaintext) {
    let before = AccountState::decode(expected).unwrap();
    let after = AccountState::decode(actual).unwrap();
    assert_eq!(expected.installation, actual.installation);
    assert_eq!(
        before.device().installation_id(),
        after.device().installation_id()
    );
    assert_eq!(
        before.device().transport_public_key(),
        after.device().transport_public_key()
    );
    assert_eq!(
        iroh::SecretKey::from_bytes(before.device().transport().as_bytes()).public(),
        iroh::SecretKey::from_bytes(after.device().transport().as_bytes()).public()
    );
    assert!(bool::from(
        before
            .device()
            .seed()
            .as_bytes()
            .ct_eq(after.device().seed().as_bytes())
    ));
    assert!(bool::from(
        before
            .device()
            .transport()
            .as_bytes()
            .ct_eq(after.device().transport().as_bytes())
    ));
    assert!(bool::from(
        before
            .device()
            .installation()
            .as_bytes()
            .ct_eq(after.device().installation().as_bytes())
    ));
    match (&before, &after) {
        (AccountState::Active(a), AccountState::Active(b)) => {
            assert_eq!(a.keys.peer_id(), b.keys.peer_id());
            assert_eq!(a.revision, b.revision);
            assert_eq!(a.cert.to_bytes(), b.cert.to_bytes());
            assert!(bool::from(
                a.keys
                    .identity()
                    .as_bytes()
                    .ct_eq(b.keys.identity().as_bytes())
            ));
            assert_eq!(a.records.observed_seq, 11);
            assert_eq!(b.records.observed_seq, 11);
            assert_eq!(b.remote[0].floor, 19);
            assert!(b.remote[0].conflict.is_some());
            assert_eq!(b.records.tombstones.len(), 1);
            assert_eq!(b.records.pending.len(), 1);
            assert_eq!(b.pairs.existing.len(), 3);
            assert!(b.pairs.existing[0].receipt.is_none());
            assert!(b.pairs.existing[1].receipt.is_some());
            assert_eq!(b.pairs.existing[2].stage as u8, Stage::Observed as u8);
        }
        (AccountState::CandidatePinned(a), AccountState::CandidatePinned(b)) => {
            assert_eq!(a.revision, b.revision);
            assert_eq!(a.pin.peer, b.pin.peer);
        }
        (AccountState::PendingImport(a), AccountState::PendingImport(b)) => {
            assert_eq!(a.revision, b.revision);
            assert_eq!(a.receipt.tx, b.receipt.tx);
        }
        (AccountState::Candidate(_), AccountState::Candidate(_)) => {}
        _ => panic!("migration promoted or changed account phase"),
    }
    // Includes exact private bytes, contacts/safety, records/history/receipts;
    // only the boolean can enter an assertion diagnostic.
    assert!(bool::from(
        expected.payload.as_slice().ct_eq(actual.payload.as_slice())
    ));
}
