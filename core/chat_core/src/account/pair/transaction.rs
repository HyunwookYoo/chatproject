//! Durable transaction records; no QR secret or reusable approval is stored.
use super::transfer::{self, Envelope, Payload, Receipt};
use crate::{
    CoreError,
    account::{
        DeviceKeys, FlowId, Generation, InstallationCert, InstallationId, PeerId, TxId,
        publication::RecordState,
        wire::{self, invalid},
    },
};
use minicbor::{Decoder, Encoder, data::Type};
use zeroize::Zeroizing;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExistingPin {
    pub peer: PeerId,
    pub installation: InstallationId,
    pub transport: [u8; 32],
}
impl ExistingPin {
    pub(crate) fn encode(&self, e: &mut Encoder<Vec<u8>>) {
        e.array(3)
            .unwrap()
            .bytes(&self.peer.0)
            .unwrap()
            .bytes(&self.installation.0)
            .unwrap()
            .bytes(&self.transport)
            .unwrap();
    }
    pub(crate) fn decode(d: &mut Decoder<'_>) -> Result<Self, CoreError> {
        wire::array(d, 3)?;
        Ok(Self {
            peer: PeerId(wire::bytes(d)?),
            installation: InstallationId(wire::bytes(d)?),
            transport: wire::bytes(d)?,
        })
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    DisclosurePossible,
    ReceiptVerified,
    PublishPending,
    Observed,
}
pub(crate) struct PairTransaction {
    pub tx: TxId,
    pub pin: ExistingPin,
    pub payload: Zeroizing<Vec<u8>>,
    pub approved_flow: FlowId,
    pub approved_hash: [u8; 32],
    pub stage: Stage,
    pub receipt: Option<Receipt>,
    pub publication: Option<[u8; 32]>,
}
impl PairTransaction {
    pub(crate) fn envelope(&self) -> Result<Envelope, CoreError> {
        Ok(Envelope {
            tx: self.tx,
            total: self.payload.len(),
            hash: transfer::hash(&self.payload),
        })
    }
    fn bytes(&self) -> Zeroizing<Vec<u8>> {
        let mut e = Encoder::new(Vec::with_capacity(self.payload.len() + 512));
        e.array(11)
            .unwrap()
            .u8(1)
            .unwrap()
            .bytes(&self.tx.0)
            .unwrap()
            .bytes(&self.pin.peer.0)
            .unwrap()
            .bytes(&self.pin.installation.0)
            .unwrap()
            .bytes(&self.pin.transport)
            .unwrap()
            .bytes(&self.payload)
            .unwrap()
            .bytes(&self.approved_flow.0)
            .unwrap()
            .bytes(&self.approved_hash)
            .unwrap()
            .u8(self.stage as u8)
            .unwrap();
        if let Some(r) = &self.receipt {
            e.bytes(&r.bytes()).unwrap();
        } else {
            e.null().unwrap();
        }
        if let Some(h) = self.publication {
            e.bytes(&h).unwrap();
        } else {
            e.null().unwrap();
        }
        Zeroizing::new(e.into_writer())
    }
    fn decode(input: &[u8]) -> Result<Self, CoreError> {
        let mut d = Decoder::new(input);
        wire::array(&mut d, 11)?;
        if wire::uint(&mut d)? != 1 {
            return Err(invalid());
        }
        let tx = TxId(wire::bytes(&mut d)?);
        let pin = ExistingPin {
            peer: PeerId(wire::bytes(&mut d)?),
            installation: InstallationId(wire::bytes(&mut d)?),
            transport: wire::bytes(&mut d)?,
        };
        let payload = Zeroizing::new(d.bytes().map_err(|_| invalid())?.to_vec());
        let p = Payload::decode(&payload)?;
        let approved_flow = FlowId(wire::bytes(&mut d)?);
        let approved_hash = wire::bytes(&mut d)?;
        let stage = match wire::uint(&mut d)? {
            0 => Stage::DisclosurePossible,
            1 => Stage::ReceiptVerified,
            2 => Stage::PublishPending,
            3 => Stage::Observed,
            _ => return Err(invalid()),
        };
        let receipt = if d.datatype().map_err(|_| invalid())? == Type::Null {
            d.null().map_err(|_| invalid())?;
            None
        } else {
            Some(Receipt::decode(d.bytes().map_err(|_| invalid())?)?)
        };
        let publication = transfer::optional(&mut d)?;
        if p.tx != tx
            || p.keys.peer_id() != pin.peer
            || receipt.as_ref().is_some_and(|r| {
                r.tx != tx
                    || r.payload_hash != transfer::hash(&payload)
                    || r.installation != p.cert.installation_id
            })
            || (stage != Stage::DisclosurePossible && receipt.is_none())
            || matches!(stage, Stage::PublishPending | Stage::Observed) != publication.is_some()
        {
            return Err(invalid());
        }
        let s = Self {
            tx,
            pin,
            payload,
            approved_flow,
            approved_hash,
            stage,
            receipt,
            publication,
        };
        wire::canonical(input, &s.bytes())?;
        Ok(s)
    }
}
#[derive(Clone)]
pub(crate) struct Origin {
    pub tx: TxId,
    pub pin: ExistingPin,
    pub cert: InstallationCert,
    pub generation: Generation,
    pub base: [u8; 32],
    pub hash: [u8; 32],
    pub receipt: Receipt,
}
impl Origin {
    pub(crate) fn bytes(&self) -> Vec<u8> {
        let mut e = Encoder::new(Vec::new());
        e.array(8)
            .unwrap()
            .u8(1)
            .unwrap()
            .bytes(&self.tx.0)
            .unwrap();
        self.pin.encode(&mut e);
        e.bytes(&self.cert.to_bytes())
            .unwrap()
            .bytes(&self.generation.0)
            .unwrap()
            .bytes(&self.base)
            .unwrap()
            .bytes(&self.hash)
            .unwrap()
            .bytes(&self.receipt.bytes())
            .unwrap();
        e.into_writer()
    }
    pub(crate) fn decode(input: &[u8]) -> Result<Self, CoreError> {
        let mut d = Decoder::new(input);
        wire::array(&mut d, 8)?;
        if wire::uint(&mut d)? != 1 {
            return Err(invalid());
        }
        let s = Self {
            tx: TxId(wire::bytes(&mut d)?),
            pin: ExistingPin::decode(&mut d)?,
            cert: InstallationCert::from_bytes(d.bytes().map_err(|_| invalid())?)?,
            generation: Generation(wire::bytes(&mut d)?),
            base: wire::bytes(&mut d)?,
            hash: wire::bytes(&mut d)?,
            receipt: Receipt::decode(d.bytes().map_err(|_| invalid())?)?,
        };
        crate::account::verify_cert(&s.cert, s.pin.peer)?;
        if s.receipt.tx != s.tx
            || s.receipt.payload_hash != s.hash
            || s.receipt.installation != s.cert.installation_id
        {
            return Err(invalid());
        }
        wire::canonical(input, &s.bytes())?;
        Ok(s)
    }
}
#[derive(Default)]
pub(crate) struct PairState {
    pub existing: Vec<PairTransaction>,
    pub origin: Option<Origin>,
}
impl PairState {
    pub(crate) fn unfinished(&self) -> usize {
        self.existing.iter().filter(|t| t.stage != Stage::Observed).count()
    }
    pub(crate) fn admit_new(&self, pending: usize) -> Result<(), CoreError> {
        if self.existing.len() >= 256 {
            return Err(CoreError::new(crate::ErrorCode::PairHistoryFull, "Retained pairing history capacity reached"));
        }
        // Reserve one additional intent for publishing the newly received device.
        if pending != 0 || self.unfinished() + pending >= 31 {
            return Err(CoreError::new(crate::ErrorCode::StorageFull, "Pending account operation capacity reached"));
        }
        Ok(())
    }
    pub(crate) fn encode(&self) -> Zeroizing<Vec<u8>> {
        let capacity = 64
            + self
                .existing
                .iter()
                .map(|t| t.payload.len() + 512)
                .sum::<usize>()
            + 1024;
        let mut e = Encoder::new(Vec::with_capacity(capacity));
        e.array(3)
            .unwrap()
            .u8(1)
            .unwrap()
            .array(self.existing.len() as u64)
            .unwrap();
        for t in &self.existing {
            e.bytes(&t.bytes()).unwrap();
        }
        if let Some(o) = &self.origin {
            e.bytes(&o.bytes()).unwrap();
        } else {
            e.null().unwrap();
        }
        Zeroizing::new(e.into_writer())
    }
    pub(crate) fn decode(
        input: &[u8],
        peer: PeerId,
        device: &DeviceKeys,
        pending: usize,
    ) -> Result<Self, CoreError> {
        let mut d = Decoder::new(input);
        wire::array(&mut d, 3)?;
        if wire::uint(&mut d)? != 1 {
            return Err(invalid());
        }
        let n = d.array().map_err(|_| invalid())?.ok_or_else(invalid)? as usize;
        if n > 256 {
            return Err(invalid());
        }
        let mut s = Self::default();
        for _ in 0..n {
            let t = PairTransaction::decode(d.bytes().map_err(|_| invalid())?)?;
            if t.pin
                != (ExistingPin {
                    peer,
                    installation: device.installation_id(),
                    transport: device.transport_public_key(),
                })
                || s.existing
                    .iter()
                    .any(|old| old.tx == t.tx)
            {
                return Err(invalid());
            }
            s.existing.push(t);
        }
        if s.unfinished() + pending > 32 { return Err(invalid()); }
        s.origin = if d.datatype().map_err(|_| invalid())? == Type::Null {
            d.null().map_err(|_| invalid())?;
            None
        } else {
            Some(Origin::decode(d.bytes().map_err(|_| invalid())?)?)
        };
        if s.origin.as_ref().is_some_and(|o| {
            o.pin.peer != peer
                || o.cert.installation_id != device.installation_id()
                || o.cert.transport_public_key != device.transport_public_key()
        }) {
            return Err(invalid());
        }
        wire::canonical(input, &s.encode())?;
        Ok(s)
    }
}

pub(crate) struct CandidatePinned {
    pub device: DeviceKeys,
    pub revision: u64,
    pub pin: ExistingPin,
    pub envelope: Envelope,
}
pub(crate) struct PendingImport {
    pub device: DeviceKeys,
    pub revision: u64,
    pub pin: ExistingPin,
    pub payload: Zeroizing<Vec<u8>>,
    pub receipt: Receipt,
    pub records: RecordState,
}
impl PendingImport {
    pub(crate) fn origin(&self) -> Result<Origin, CoreError> {
        let p = Payload::decode(&self.payload)?;
        Ok(Origin {
            tx: p.tx,
            pin: self.pin,
            cert: p.cert.clone(),
            generation: p.generation,
            base: p.base,
            hash: transfer::hash(&self.payload),
            receipt: self.receipt.clone(),
        })
    }
}

#[cfg(test)]
mod capacity_tests {
    use super::*;
    use crate::account::{Secret32, derive_account, derive_device, sign_cert, DeviceKind};

    #[test]
    fn retained_history_and_unfinished_intents_have_separate_bounded_budgets() {
        let keys = derive_account(&Secret32::from([7; 32])).unwrap();
        let device = derive_device(Secret32::from([8; 32])).unwrap();
        let cert = sign_cert(&keys, &device, DeviceKind::Desktop, 1000).unwrap();
        let mut state = PairState::default();
        for i in 0..256u16 {
            let mut tx = TxId([0; 16]);
            tx.0[..2].copy_from_slice(&i.to_be_bytes());
            let payload = Payload::encode_parts(tx, &keys, &cert, Generation([3; 16]), [4; 32], &[]);
            let receipt = Receipt::sign(tx, transfer::hash(&payload), &device, FlowId([5; 16]), [6; 32]);
            state.existing.push(PairTransaction {
                tx, pin: ExistingPin { peer: keys.peer_id(), installation: device.installation_id(), transport: device.transport_public_key() },
                payload, approved_flow: FlowId([5; 16]), approved_hash: [6; 32], stage: Stage::Observed,
                receipt: Some(receipt), publication: Some([9; 32]),
            });
            if i == 31 {
                assert!(state.admit_new(0).is_ok());
                assert!(PairState::decode(&state.encode(), keys.peer_id(), &device, 32).is_ok());
            }
        }
        assert_eq!(state.admit_new(0).unwrap_err().code, crate::ErrorCode::PairHistoryFull);
        let decoded = PairState::decode(&state.encode(), keys.peer_id(), &device, 32).unwrap();
        assert_eq!(decoded.existing.len(), 256);
        assert_eq!(decoded.existing[0].payload, state.existing[0].payload);
        assert_eq!(decoded.existing[0].envelope().unwrap().tx, state.existing[0].tx);
        let original_tx = state.existing[0].tx;
        state.existing[0].tx = TxId([99; 16]);
        assert!(PairState::decode(&state.encode(), keys.peer_id(), &device, 0).is_err());
        state.existing[0].tx = original_tx;
        let mut duplicate = Encoder::new(Vec::new());
        duplicate.array(3).unwrap().u8(1).unwrap().array(2).unwrap()
            .bytes(&state.existing[0].bytes()).unwrap()
            .bytes(&state.existing[0].bytes()).unwrap().null().unwrap();
        assert!(PairState::decode(&duplicate.into_writer(), keys.peer_id(), &device, 0).is_err());
        let mut oversized = Encoder::new(Vec::new());
        oversized.array(3).unwrap().u8(1).unwrap().array(257).unwrap();
        assert!(PairState::decode(&oversized.into_writer(), keys.peer_id(), &device, 0).is_err());
        state.existing.truncate(32);
        for entry in &mut state.existing {
            entry.stage = Stage::DisclosurePossible;
            entry.receipt = None;
            entry.publication = None;
        }
        assert_eq!(state.admit_new(0).unwrap_err().code, crate::ErrorCode::StorageFull);
        assert!(PairState::decode(&state.encode(), keys.peer_id(), &device, 0).is_ok());
        assert!(PairState::decode(&state.encode(), keys.peer_id(), &device, 1).is_err());
    }
}
