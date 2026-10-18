//! Identity-only invite codec. Parsing never performs network or storage IO.
use super::{
    AccountKeys, EndpointHint, InstallationCert, InvitePreview, PeerId, verify_cert, wire,
};
use crate::CoreError;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use minicbor::{Decoder, Encoder, data::Type};
const SCHEME: &str = "chatproject:identity-invite/1/";
const LIMIT: usize = 4096;
const DOMAIN: &[u8] = b"ChatProject/identity-invite/v1";
struct Invite {
    preview: InvitePreview,
    nonce: [u8; 16],
    sig: [u8; 64],
}
pub(crate) fn validate_name(name: &str) -> Result<(), CoreError> {
    if name.len() > 64
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
    {
        return Err(wire::invalid());
    }
    Ok(())
}
fn validate_hint(hint: &EndpointHint) -> Result<(), CoreError> {
    if let Some(relay) = &hint.relay_url {
        if relay.len() > 256 {
            return Err(wire::invalid());
        }
        let url: iroh::RelayUrl = relay.parse().map_err(|_| wire::invalid())?;
        if url.scheme() != "https"
            || url.host_str().is_none()
            || url.host_str().is_some_and(|h| {
                h.trim_matches(['[', ']'])
                    .parse::<std::net::IpAddr>()
                    .is_ok()
            })
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(wire::invalid());
        }
    }
    Ok(())
}
impl Invite {
    fn encode(&self, signed: bool) -> Vec<u8> {
        let p = &self.preview;
        let mut e = Encoder::new(Vec::new());
        e.array(if signed { 9 } else { 8 })
            .unwrap()
            .u8(1)
            .unwrap()
            .bytes(&p.peer_id.0)
            .unwrap();
        let mut body = e.into_writer();
        body.extend(p.issuing_cert.to_bytes());
        let mut e = Encoder::new(body);
        e.array(2)
            .unwrap()
            .bytes(&p.endpoint_hint.transport_public_key)
            .unwrap();
        if let Some(relay) = &p.endpoint_hint.relay_url {
            e.str(relay).unwrap();
        } else {
            e.null().unwrap();
        }
        e.bytes(&self.nonce).unwrap();
        if let Some(name) = &p.display_name {
            e.str(name).unwrap();
        } else {
            e.null().unwrap();
        }
        e.u64(p.issued_at).unwrap().u64(p.expires_at).unwrap();
        if signed {
            e.bytes(&self.sig).unwrap();
        }
        e.into_writer()
    }
    fn message(&self) -> Vec<u8> {
        let mut m = DOMAIN.to_vec();
        m.push(1);
        m.extend(self.encode(false));
        m
    }
}
fn optional_text(d: &mut Decoder<'_>) -> Result<Option<String>, CoreError> {
    if d.datatype().map_err(|_| wire::invalid())? == Type::Null {
        d.null().map_err(|_| wire::invalid())?;
        Ok(None)
    } else {
        Ok(Some(d.str().map_err(|_| wire::invalid())?.into()))
    }
}
pub fn parse_invite(input: &str, now: u64) -> Result<InvitePreview, CoreError> {
    let payload = input.strip_prefix(SCHEME).ok_or_else(wire::invalid)?;
    if payload.len() > (LIMIT * 4).div_ceil(3) {
        return Err(wire::invalid());
    }
    let bytes = data_encoding::BASE64URL_NOPAD
        .decode(payload.as_bytes())
        .map_err(|_| wire::invalid())?;
    if bytes.len() > LIMIT {
        return Err(wire::invalid());
    }
    let mut d = Decoder::new(&bytes);
    wire::array(&mut d, 9)?;
    if wire::uint(&mut d)? != 1 {
        return Err(wire::invalid());
    }
    let peer_id = PeerId(wire::bytes(&mut d)?);
    let start = d.position();
    d.skip().map_err(|_| wire::invalid())?;
    let cert = InstallationCert::from_bytes(&bytes[start..d.position()])?;
    wire::array(&mut d, 2)?;
    let hint = EndpointHint {
        transport_public_key: wire::bytes(&mut d)?,
        relay_url: optional_text(&mut d)?,
    };
    let nonce = wire::bytes(&mut d)?;
    let name = optional_text(&mut d)?;
    let issued_at = wire::uint(&mut d)?;
    let expires_at = wire::uint(&mut d)?;
    let sig = wire::bytes(&mut d)?;
    let invite = Invite {
        preview: InvitePreview {
            peer_id,
            issuing_cert: cert,
            endpoint_hint: hint,
            display_name: name,
            issued_at,
            expires_at,
        },
        nonce,
        sig,
    };
    wire::canonical(&bytes, &invite.encode(true))?;
    let p = &invite.preview;
    if let Some(name) = &p.display_name {
        validate_name(name)?;
    }
    validate_hint(&p.endpoint_hint)?;
    verify_cert(&p.issuing_cert, peer_id)?;
    if p.endpoint_hint.transport_public_key != p.issuing_cert.transport_public_key {
        return Err(wire::invalid());
    }
    VerifyingKey::from_bytes(&peer_id.0)
        .map_err(|_| wire::invalid())?
        .verify_strict(&invite.message(), &Signature::from_bytes(&sig))
        .map_err(|_| wire::invalid())?;
    if expires_at <= issued_at
        || expires_at - issued_at > 604800
        || issued_at > now.saturating_add(300)
        || expires_at <= now
    {
        return Err(wire::invalid());
    }
    Ok(invite.preview)
}
pub(crate) fn create(
    keys: &AccountKeys,
    cert: &InstallationCert,
    name: Option<String>,
    now: u64,
) -> Result<String, CoreError> {
    if let Some(name) = &name {
        validate_name(name)?;
    }
    let mut nonce = [0; 16];
    getrandom::fill(&mut nonce).map_err(|_| wire::invalid())?;
    let mut invite = Invite {
        preview: InvitePreview {
            peer_id: keys.peer_id(),
            issuing_cert: cert.clone(),
            endpoint_hint: EndpointHint {
                transport_public_key: cert.transport_public_key,
                relay_url: None,
            },
            display_name: name,
            issued_at: now,
            expires_at: now.checked_add(86400).ok_or_else(wire::invalid)?,
        },
        nonce,
        sig: [0; 64],
    };
    invite.sig = SigningKey::from_bytes(keys.identity().as_bytes())
        .sign(&invite.message())
        .to_bytes();
    Ok(format!(
        "{SCHEME}{}",
        data_encoding::BASE64URL_NOPAD.encode(&invite.encode(true))
    ))
}
