use super::{AccountKeys, Generation, InstallationId, PeerId, RecordDevice, RecordFuture, wire};
use crate::CoreError;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use minicbor::{Decoder, Encoder};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RevokeIntent {
    pub peer_id: PeerId,
    pub generation: Generation,
    pub base_hash: [u8; 32],
    pub target_installation: InstallationId,
    pub target_transport: [u8; 32],
    pub nonce: [u8; 16],
    pub issued_at: u64,
    #[serde(with = "signature_serde")]
    pub sig: [u8; 64],
}

impl RevokeIntent {
    fn encode(&self, signed: bool) -> Vec<u8> {
        let mut e = Encoder::new(Vec::new());
        e.array(if signed { 9 } else { 8 })
            .unwrap()
            .u8(1)
            .unwrap()
            .bytes(&self.peer_id.0)
            .unwrap()
            .bytes(&self.generation.0)
            .unwrap()
            .bytes(&self.base_hash)
            .unwrap()
            .bytes(&self.target_installation.0)
            .unwrap()
            .bytes(&self.target_transport)
            .unwrap()
            .bytes(&self.nonce)
            .unwrap()
            .u64(self.issued_at)
            .unwrap();
        if signed {
            e.bytes(&self.sig).unwrap();
        }
        e.into_writer()
    }
    fn message(&self) -> Vec<u8> {
        let mut message = b"ChatProject/revoke/v1".to_vec();
        message.push(1);
        message.extend(self.encode(false));
        message
    }
    pub(crate) fn sign(
        keys: &AccountKeys,
        generation: Generation,
        base_hash: [u8; 32],
        target: &RecordDevice,
        issued_at: u64,
    ) -> Result<Self, CoreError> {
        let mut nonce = [0; 16];
        getrandom::fill(&mut nonce).map_err(|_| wire::invalid())?;
        let mut intent = Self {
            peer_id: keys.peer_id(),
            generation,
            base_hash,
            target_installation: target.installation_id,
            target_transport: target.transport_public_key,
            nonce,
            issued_at,
            sig: [0; 64],
        };
        intent.sig = SigningKey::from_bytes(keys.identity().as_bytes())
            .sign(&intent.message())
            .to_bytes();
        Ok(intent)
    }
    pub fn to_bytes(&self) -> Vec<u8> {
        self.encode(true)
    }
    /// Structural/canonical decode only. Verify the expected PeerId before use.
    pub fn from_bytes(input: &[u8]) -> Result<Self, CoreError> {
        if input.len() > 256 {
            return Err(wire::invalid());
        }
        let mut d = Decoder::new(input);
        wire::array(&mut d, 9)?;
        if wire::uint(&mut d)? != 1 {
            return Err(wire::invalid());
        }
        let intent = Self {
            peer_id: PeerId(wire::bytes(&mut d)?),
            generation: Generation(wire::bytes(&mut d)?),
            base_hash: wire::bytes(&mut d)?,
            target_installation: InstallationId(wire::bytes(&mut d)?),
            target_transport: wire::bytes(&mut d)?,
            nonce: wire::bytes(&mut d)?,
            issued_at: wire::uint(&mut d)?,
            sig: wire::bytes(&mut d)?,
        };
        wire::canonical(input, &intent.to_bytes())?;
        Ok(intent)
    }
    pub fn verify(&self, expected: PeerId) -> Result<(), CoreError> {
        if self.peer_id != expected {
            return Err(wire::invalid());
        }
        VerifyingKey::from_bytes(&expected.0)
            .map_err(|_| wire::invalid())?
            .verify_strict(&self.message(), &Signature::from_bytes(&self.sig))
            .map_err(|_| wire::invalid())
    }
    /// Account control envelope [1, 3, payload bytes], with a u32 BE frame length.
    pub fn to_frame(&self) -> Vec<u8> {
        let mut e = Encoder::new(Vec::new());
        e.array(3)
            .unwrap()
            .u8(1)
            .unwrap()
            .u8(3)
            .unwrap()
            .bytes(&self.to_bytes())
            .unwrap();
        let payload = e.into_writer();
        let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
        frame.extend(payload);
        frame
    }
    pub fn from_frame(frame: &[u8]) -> Result<Self, CoreError> {
        if frame.len() < 4 || frame.len() > 65540 {
            return Err(wire::invalid());
        }
        let length = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
        if length > 65536 || length != frame.len() - 4 {
            return Err(wire::invalid());
        }
        let mut d = Decoder::new(&frame[4..]);
        wire::array(&mut d, 3)?;
        if wire::uint(&mut d)? != 1 || wire::uint(&mut d)? != 3 {
            return Err(wire::invalid());
        }
        let intent = Self::from_bytes(d.bytes().map_err(|_| wire::invalid())?)?;
        wire::canonical(frame, &intent.to_frame())?;
        Ok(intent)
    }
}

/// B3 must deliver only over authenticated, same-account connections and return
/// success after its durable receipt policy. No implementation is installed yet.
pub trait RevocationNotifier: Send + Sync {
    fn notify<'a>(&'a self, intent: &'a RevokeIntent) -> RecordFuture<'a, ()>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{DeviceKind, Secret32, derive_account, derive_device};

    #[test]
    fn revoke_signature_canonical_and_bound_to_account() {
        let keys = derive_account(&Secret32::from([1; 32])).unwrap();
        let device = derive_device(Secret32::from([2; 32])).unwrap();
        let target = RecordDevice {
            installation_id: device.installation_id(),
            transport_public_key: device.transport_public_key(),
            kind: DeviceKind::Desktop,
        };
        let intent = RevokeIntent::sign(&keys, Generation([3; 16]), [4; 32], &target, 100).unwrap();
        intent.verify(keys.peer_id()).unwrap();
        assert_eq!(
            RevokeIntent::from_bytes(&intent.to_bytes()).unwrap(),
            intent
        );
        let mut changed = intent.clone();
        changed.base_hash[0] ^= 1;
        assert!(changed.verify(keys.peer_id()).is_err());
        assert!(intent.verify(PeerId([0; 32])).is_err());
        let mut bytes = intent.to_bytes();
        bytes.push(0);
        assert!(RevokeIntent::from_bytes(&bytes).is_err());
        let mut bytes = intent.to_bytes();
        bytes.splice(1..2, [0x18, 1]);
        assert!(RevokeIntent::from_bytes(&bytes).is_err());
        assert_eq!(
            RevokeIntent::from_frame(&intent.to_frame()).unwrap(),
            intent
        );
        let mut frame = intent.to_frame();
        frame[3] ^= 1;
        assert!(RevokeIntent::from_frame(&frame).is_err());
        for payload in [
            {
                let mut e = Encoder::new(Vec::new());
                e.array(3).unwrap().u8(1).unwrap().u8(3).unwrap();
                let mut p = e.into_writer();
                p.extend(intent.to_bytes());
                p
            },
            {
                let mut e = Encoder::new(Vec::new());
                e.array(3)
                    .unwrap()
                    .u8(1)
                    .unwrap()
                    .u8(2)
                    .unwrap()
                    .bytes(&intent.to_bytes())
                    .unwrap();
                e.into_writer()
            },
            {
                let mut p = intent.to_frame()[4..].to_vec();
                p.splice(1..2, [0x18, 1]);
                p
            },
        ] {
            let mut frame = (payload.len() as u32).to_be_bytes().to_vec();
            frame.extend(payload);
            assert!(RevokeIntent::from_frame(&frame).is_err());
        }
        assert!(RevokeIntent::from_frame(&vec![0; 65541]).is_err());
    }
}

// Serde supports arrays through 32 bytes; this public signature stays exactly 64.
mod signature_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(value: &[u8; 64], serializer: S) -> Result<S::Ok, S::Error> {
        value.as_slice().serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<[u8; 64], D::Error> {
        Vec::<u8>::deserialize(deserializer)?.try_into().map_err(|_| serde::de::Error::custom("invalid public signature length"))
    }
}
