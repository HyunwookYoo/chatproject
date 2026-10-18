//! Canonical durable local contacts. Invite hints do not authorize devices.
use super::{ContactView, InstallationId, PeerId, invite::validate_name, safety_digest, wire};
use crate::CoreError;
use minicbor::{Decoder, Encoder, data::Type};
use zeroize::{Zeroize, Zeroizing};
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct Contact {
    pub peer: PeerId,
    pub alias: String,
    pub verified_digest: Option<[u8; 32]>,
    pub known_installation: Option<InstallationId>,
    pub new_device: bool,
}
impl Zeroize for Contact {
    fn zeroize(&mut self) {
        self.peer.0.zeroize();
        self.alias.zeroize();
        self.verified_digest.zeroize();
        if let Some(id) = &mut self.known_installation {
            id.0.zeroize();
        }
        self.new_device.zeroize();
    }
}
impl Drop for Contact {
    fn drop(&mut self) {
        self.zeroize();
    }
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
fn encoded_capacity(contacts: &[Contact]) -> usize {
    // Nine bytes covers any definite array/text length prefix. All other
    // fields have fixed maximum widths; reserve before copying private data.
    9 + contacts
        .iter()
        .map(|c| 1 + 34 + 9 + c.alias.len() + 1 + 34 + 34 + 1)
        .sum::<usize>()
}
pub(crate) fn encode(contacts: &[Contact]) -> Zeroizing<Vec<u8>> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(encoded_capacity(contacts)));
    let mut e = Encoder::new(&mut *bytes);
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
    bytes
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
        let mut contact = Contact {
            peer: PeerId(wire::bytes(&mut d)?),
            alias: String::new(),
            verified_digest: None,
            known_installation: None,
            new_device: false,
        };
        let alias = d.str().map_err(|_| wire::invalid())?;
        validate_name(alias)?;
        contact.alias = alias.into();
        let verified = wire::uint(&mut d)?;
        contact.verified_digest = if d.datatype().map_err(|_| wire::invalid())? == Type::Null {
            d.null().map_err(|_| wire::invalid())?;
            None
        } else {
            Some(wire::bytes(&mut d)?)
        };
        contact.known_installation = if d.datatype().map_err(|_| wire::invalid())? == Type::Null {
            d.null().map_err(|_| wire::invalid())?;
            None
        } else {
            Some(InstallationId(wire::bytes(&mut d)?))
        };
        contact.new_device = match wire::uint(&mut d)? {
            0 => false,
            1 => true,
            _ => return Err(wire::invalid()),
        };
        if contact.peer == own
            || contacts
                .last()
                .is_some_and(|c: &Contact| c.peer >= contact.peer)
            || verified != u64::from(contact.verified_digest.is_some())
            || contact
                .verified_digest
                .is_some_and(|v| v != safety_digest(own, contact.peer))
            || (contact.new_device && contact.known_installation.is_none())
        {
            return Err(wire::invalid());
        }
        contacts.push(contact);
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
    fn private_contact_owner_and_complete_encoding_reserve() {
        assert!(std::mem::needs_drop::<Contact>());
        let mut contacts: Vec<_> = (0..1000).map(contact).collect();
        for c in &mut contacts {
            c.alias = "x".repeat(64);
            c.verified_digest = Some([7; 32]);
            c.known_installation = Some(InstallationId([8; 32]));
        }
        let encoded: Zeroizing<Vec<u8>> = encode(&contacts);
        assert_eq!(encoded.capacity(), encoded_capacity(&contacts));
        assert!(encoded.len() <= encoded_capacity(&contacts));
        contacts[0].zeroize();
        assert!(contacts[0].alias.is_empty());
        assert_eq!(contacts[0].peer.0, [0; 32]);
        assert!(contacts[0].verified_digest.is_none_or(|d| d == [0; 32]));
        assert!(
            contacts[0]
                .known_installation
                .is_none_or(|d| d.0 == [0; 32])
        );
        // A valid first entry followed by a malformed entry exercises partial
        // decode: every constructed Contact owns its own zeroizing Drop.
        let mut partial = encode(&[contact(1), contact(2)]);
        partial.pop();
        assert!(decode(&partial, PeerId([255; 32])).is_err());
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
