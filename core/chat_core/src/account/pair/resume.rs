//! Exact transaction authority, independent of ordinary account membership.
use super::{
    transaction::ExistingPin,
    transfer::{Envelope, Payload},
    wire::{Preface, Role},
};
use crate::{
    CoreError,
    account::{TxId, storage::schema::AccountState, wire::invalid},
};

pub(crate) fn role(state: &AccountState, tx: TxId) -> Result<Role, CoreError> {
    match state {
        AccountState::CandidatePinned(p) if p.envelope.tx == tx => Ok(Role::New),
        AccountState::PendingImport(p) if p.receipt.tx == tx => Ok(Role::New),
        AccountState::Active(local) => {
            if local.pairs.origin.as_ref().is_some_and(|o| o.tx == tx) {
                return Ok(Role::New);
            }
            if local
                .pairs
                .existing
                .iter()
                .any(|t| t.envelope().is_ok_and(|e| e.tx == tx))
            {
                return Ok(Role::Existing);
            }
            Err(invalid())
        }
        _ => Err(invalid()),
    }
}
pub(crate) fn existing(remote: &Preface) -> Result<ExistingPin, CoreError> {
    if remote.role != Role::Existing {
        return Err(invalid());
    }
    Ok(ExistingPin {
        peer: remote.existing.ok_or_else(invalid)?,
        installation: remote.installation,
        transport: remote.transport,
    })
}
pub(crate) fn validate(
    state: &AccountState,
    tx: TxId,
    remote: &Preface,
    kind: Option<crate::account::DeviceKind>,
) -> Result<(), CoreError> {
    // A never-pinned candidate may accept E's resume QR as its first offer.
    // Only pair_scan of ExistingDisplays admits this state; E independently
    // matches its durable transaction to these exact N keys/kind before gating.
    if matches!(state, AccountState::Candidate(_)) && remote.resume == Some(tx) {
        existing(remote)?;
        return Ok(());
    }
    match role(state, tx)? {
        Role::New => {
            let expected = match state {
                AccountState::CandidatePinned(p) => p.pin,
                AccountState::PendingImport(p) => p.pin,
                AccountState::Active(l) => l.pairs.origin.as_ref().ok_or_else(invalid)?.pin,
                _ => return Err(invalid()),
            };
            if existing(remote)? != expected {
                return Err(invalid());
            }
        }
        Role::Existing => {
            let AccountState::Active(l) = state else {
                return Err(invalid());
            };
            let t = l
                .pairs
                .existing
                .iter()
                .find(|t| t.envelope().is_ok_and(|e| e.tx == tx))
                .ok_or_else(invalid)?;
            let p = Payload::decode(&t.payload)?;
            if remote.role != Role::New
                || remote.installation != p.cert.installation_id
                || remote.transport != p.cert.transport_public_key
                || kind != Some(p.cert.kind)
            {
                return Err(invalid());
            }
        }
    }
    Ok(())
}
pub(crate) fn envelope_matches(
    state: &AccountState,
    pin: ExistingPin,
    e: Envelope,
) -> Result<(), CoreError> {
    let okay = match state {
        AccountState::Candidate(_) => true,
        AccountState::CandidatePinned(p) => p.pin == pin && p.envelope == e,
        AccountState::PendingImport(p) => {
            p.pin == pin
                && p.receipt.tx == e.tx
                && p.receipt.payload_hash == e.hash
                && p.payload.len() == e.total
        }
        AccountState::Active(l) => l
            .pairs
            .origin
            .as_ref()
            .is_some_and(|o| o.pin == pin && o.tx == e.tx && o.hash == e.hash),
    };
    if okay { Ok(()) } else { Err(invalid()) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{InstallationId, PeerId, Secret32, derive_device};
    #[test]
    fn changed_payload_candidate_existing_identity_and_tx_rejected() {
        let pin = ExistingPin {
            peer: PeerId([1; 32]),
            installation: InstallationId([2; 32]),
            transport: [3; 32],
        };
        let e = Envelope {
            tx: TxId([4; 16]),
            total: 100,
            hash: [5; 32],
        };
        let state =
            AccountState::CandidatePinned(Box::new(super::super::transaction::CandidatePinned {
                device: derive_device(Secret32::from([6; 32])).unwrap(),
                revision: 1,
                pin,
                envelope: e,
            }));
        assert!(envelope_matches(&state, pin, e).is_ok());
        for changed in [
            Envelope { hash: [9; 32], ..e },
            Envelope { total: 99, ..e },
            Envelope {
                tx: TxId([7; 16]),
                ..e
            },
        ] {
            assert!(envelope_matches(&state, pin, changed).is_err());
        }
        let remote = Preface {
            flow: crate::account::FlowId([8; 16]),
            role: Role::Existing,
            transport: pin.transport,
            installation: pin.installation,
            existing: Some(pin.peer),
            resume: Some(e.tx),
        };
        assert!(validate(&state, e.tx, &remote, None).is_ok());
        let mut other = remote;
        other.transport[0] ^= 1;
        assert!(validate(&state, e.tx, &other, None).is_err());
        other.transport = pin.transport;
        other.installation.0[0] ^= 1;
        assert!(validate(&state, e.tx, &other, None).is_err());
        other.installation = pin.installation;
        other.existing = Some(PeerId([9; 32]));
        assert!(validate(&state, e.tx, &other, None).is_err());
        assert!(role(&state, TxId([0; 16])).is_err());
    }
}
