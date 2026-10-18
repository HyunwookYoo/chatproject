//! Canonical durable local contacts. Invite hints do not authorize devices.
use super::{ContactView, InstallationId, PeerId, invite::validate_name, safety_digest, wire};
use crate::CoreError;
use minicbor::{Decoder, Encoder, data::Type};
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Contact {
    pub peer: PeerId,
    pub alias: String,
    pub verified_digest: Option<[u8; 32]>,
    pub known_installation: Option<InstallationId>,
    pub new_device: bool,
}
impl Contact {
    pub(crate) fn view(&self, device_verified: bool) -> ContactView {
        ContactView {
            peer_id: self.peer,
            alias: self.alias.clone(),
            verified: self.verified_digest.is_some(),
            new_device: self.new_device,
            device_unverified: !device_verified,
        }
    }
}
pub(crate) fn encode(contacts: &[Contact]) -> Vec<u8> {
    let mut e = Encoder::new(Vec::new());
    e.array(contacts.len() as u64).unwrap();
    for c in contacts {
        e.array(6)
            .unwrap()
            .bytes(&c.peer.0)
            .unwrap()
            .str(&c.alias)
            .unwrap()
            .u8(u8::from(c.verified_digest.is_some()))
            .unwrap();
        if let Some(d) = c.verified_digest {
            e.bytes(&d).unwrap();
        } else {
            e.null().unwrap();
        }
        if let Some(i) = c.known_installation {
            e.bytes(&i.0).unwrap();
        } else {
            e.null().unwrap();
        }
        e.u8(u8::from(c.new_device)).unwrap();
    }
    e.into_writer()
}
pub(crate) fn decode(input: &[u8], own: PeerId) -> Result<Vec<Contact>, CoreError> {
    let mut d = Decoder::new(input);
    let n = d
        .array()
        .map_err(|_| wire::invalid())?
        .ok_or_else(wire::invalid)?;
    if n > 1000 {
        return Err(wire::invalid());
    }
    let mut contacts = Vec::with_capacity(n as usize);
    for _ in 0..n {
        wire::array(&mut d, 6)?;
        let peer = PeerId(wire::bytes(&mut d)?);
        let alias = d.str().map_err(|_| wire::invalid())?.to_string();
        validate_name(&alias)?;
        let verified = wire::uint(&mut d)?;
        let digest = if d.datatype().map_err(|_| wire::invalid())? == Type::Null {
            d.null().map_err(|_| wire::invalid())?;
            None
        } else {
            Some(wire::bytes(&mut d)?)
        };
        let known_installation = if d.datatype().map_err(|_| wire::invalid())? == Type::Null {
            d.null().map_err(|_| wire::invalid())?;
            None
        } else {
            Some(InstallationId(wire::bytes(&mut d)?))
        };
        let new_device = match wire::uint(&mut d)? {
            0 => false,
            1 => true,
            _ => return Err(wire::invalid()),
        };
        if peer == own
            || contacts.last().is_some_and(|c: &Contact| c.peer >= peer)
            || verified != u64::from(digest.is_some())
            || digest.is_some_and(|v| v != safety_digest(own, peer))
            || (new_device && known_installation.is_none())
        {
            return Err(wire::invalid());
        }
        contacts.push(Contact {
            peer,
            alias,
            verified_digest: digest,
            known_installation,
            new_device,
        });
    }
    wire::canonical(input, &encode(&contacts))?;
    Ok(contacts)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn contact(n: u16) -> Contact {
        let mut peer = [0; 32];
        peer[..2].copy_from_slice(&n.to_be_bytes());
        Contact {
            peer: PeerId(peer),
            alias: "local".into(),
            verified_digest: None,
            known_installation: None,
            new_device: false,
        }
    }
    #[test]
    fn canonical_contact_bounds_order_and_verification() {
        let own = PeerId([255; 32]);
        let contacts: Vec<_> = (0..1000).map(contact).collect();
        assert!(decode(&encode(&contacts), own).is_ok());
        let too_many: Vec<_> = (0..1001).map(contact).collect();
        assert!(decode(&encode(&too_many), own).is_err());
        let c = contact(1);
        assert!(decode(&encode(&[c.clone(), c.clone()]), own).is_err());
        assert!(decode(&encode(&[contact(2), c.clone()]), own).is_err());
        assert!(decode(&encode(std::slice::from_ref(&c)), c.peer).is_err());
        let mut bad = c.clone();
        bad.alias = "a\nb".into();
        assert!(decode(&encode(&[bad]), own).is_err());
        let mut bad = c.clone();
        bad.verified_digest = Some([0; 32]);
        assert!(decode(&encode(&[bad]), own).is_err());
        let mut bad = c.clone();
        bad.new_device = true;
        assert!(decode(&encode(&[bad]), own).is_err());
        let mut valid = c;
        valid.verified_digest = Some(safety_digest(own, valid.peer));
        valid.known_installation = Some(InstallationId([1; 32]));
        valid.new_device = true;
        let bytes = encode(&[valid.clone()]);
        let decoded = decode(&bytes, own).unwrap();
        assert_eq!(decoded[0].verified_digest, valid.verified_digest);
        assert!(decoded[0].new_device);
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(decode(&trailing, own).is_err());
        let mut nonminimal = bytes;
        nonminimal.splice(0..1, [0x98, 1]);
        assert!(decode(&nonminimal, own).is_err());
    }
}
