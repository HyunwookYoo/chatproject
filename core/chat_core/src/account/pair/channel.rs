//! Noise over one ordered bidirectional iroh stream. No private account data is borrowed here.
use super::wire::{self, Preface, Qr, Role};
use crate::CoreError;
use crate::account::{
    DeviceKeys, InstallationCert, Lookup, RecordBackend, clock::Clock, parse_record, verify_cert,
    wire::invalid,
};
use hkdf::Hkdf;
use iroh::endpoint::{Connection, RecvStream, SendStream};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;
pub(crate) const MAX_FRAME: usize = 65535;
pub(crate) const MAX_PLAIN: usize = 16 * 1024;
pub(crate) struct Channel {
    pub conn: Connection,
    send: SendStream,
    noise: snow::TransportState,
    pub remote: Preface,
    pub hash: [u8; 32],
    pub sas: String,
}
impl Drop for Channel {
    fn drop(&mut self) {
        self.noise.rekey_manually(Some(&[0; 32]), Some(&[0; 32]));
        self.conn.close(0u32.into(), b"pair closed");
    }
}
pub(crate) async fn read_frame(recv: &mut RecvStream, limit: usize) -> Result<Vec<u8>, CoreError> {
    let mut len = [0; 4];
    recv.read_exact(&mut len).await.map_err(|_| invalid())?;
    let len = u32::from_be_bytes(len) as usize;
    if len == 0 || len > limit {
        return Err(invalid());
    }
    let mut body = vec![0; len];
    recv.read_exact(&mut body).await.map_err(|_| invalid())?;
    Ok(body)
}
pub(crate) async fn write_frame(send: &mut SendStream, body: &[u8]) -> Result<(), CoreError> {
    if body.is_empty() || body.len() > MAX_FRAME {
        return Err(invalid());
    }
    send.write_all(&(body.len() as u32).to_be_bytes())
        .await
        .map_err(|_| invalid())?;
    send.write_all(body).await.map_err(|_| invalid())
}
impl Channel {
    pub(super) async fn send(&mut self, plain: &[u8]) -> Result<usize, CoreError> {
        if plain.len() > MAX_PLAIN {
            return Err(invalid());
        }
        let mut out = vec![0; plain.len() + 16];
        let n = self
            .noise
            .write_message(plain, &mut out)
            .map_err(|_| invalid())?;
        write_frame(&mut self.send, &out[..n]).await?;
        Ok(n + 4)
    }
    pub(super) fn receive(&mut self, cipher: &[u8]) -> Result<Zeroizing<Vec<u8>>, CoreError> {
        if cipher.len() > MAX_PLAIN + 16 {
            return Err(invalid());
        }
        let mut out = Zeroizing::new(vec![0; cipher.len()]);
        let n = self
            .noise
            .read_message(cipher, &mut out)
            .map_err(|_| invalid())?;
        out.truncate(n);
        Ok(out)
    }
}
// snow 0.10 has no Zeroize/Drop guarantee. Its safe setters overwrite the
// retained keys best-effort; upstream ephemeral/chaining copies remain a known
// residual. App-owned PSK/plaintext buffers are separately zeroized.
struct Handshake(Option<snow::HandshakeState>);
impl std::ops::Deref for Handshake {
    type Target = snow::HandshakeState;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().unwrap()
    }
}
impl std::ops::DerefMut for Handshake {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.0.as_mut().unwrap()
    }
}
impl Drop for Handshake {
    fn drop(&mut self) {
        if let Some(state) = &mut self.0 {
            let _ = state.set_psk(0, &[0; 32]);
        }
    }
}
impl Handshake {
    fn into_transport_mode(mut self) -> Result<snow::TransportState, snow::Error> {
        let mut state = self.0.take().unwrap();
        state.set_psk(0, &[0; 32])?;
        state.into_transport_mode()
    }
}
pub(crate) struct Prepared {
    send: SendStream,
    recv: RecvStream,
    local: Preface,
    pub remote: Preface,
    prologue: Vec<u8>,
    initiator: bool,
}
pub(crate) async fn prepare(
    conn: &Connection,
    qr: &Qr,
    local: Preface,
    initiator: bool,
) -> Result<Prepared, CoreError> {
    let (mut send, mut recv) = if initiator {
        conn.open_bi().await
    } else {
        conn.accept_bi().await
    }
    .map_err(|_| invalid())?;
    write_frame(&mut send, &local.encode()).await?;
    let remote = Preface::parse(&read_frame(&mut recv, 16384).await?)?;
    let prologue = wire::prologue(qr, &local, &remote, *conn.remote_id().as_bytes(), initiator)?;
    Ok(Prepared {
        send,
        recv,
        local,
        remote,
        prologue,
        initiator,
    })
}
pub(crate) struct Establish<'a> {
    pub conn: Connection,
    pub qr: &'a Qr,
    pub prepared: Prepared,
    pub device: &'a DeviceKeys,
    pub cert: Option<&'a InstallationCert>,
    pub backend: Arc<dyn RecordBackend>,
    pub clock: &'a dyn Clock,
}
pub(crate) async fn establish(args: Establish<'_>) -> Result<(Channel, RecvStream), CoreError> {
    let Establish {
        conn,
        qr,
        prepared,
        device,
        cert,
        backend,
        clock,
    } = args;
    let Prepared {
        mut send,
        mut recv,
        local,
        remote,
        prologue,
        initiator,
    } = prepared;
    let prologue_hash: [u8; 32] = Sha256::digest(&prologue).into();
    let params = "Noise_NNpsk0_25519_ChaChaPoly_SHA256"
        .parse()
        .map_err(|_| invalid())?;
    let builder = snow::Builder::new(params)
        .psk(0, qr.psk.as_bytes())
        .map_err(|_| invalid())?
        .prologue(&prologue)
        .map_err(|_| invalid())?;
    let mut noise = Handshake(Some(
        if initiator {
            builder.build_initiator()
        } else {
            builder.build_responder()
        }
        .map_err(|_| invalid())?,
    ));
    let mut out = Zeroizing::new(vec![0; MAX_FRAME]);
    if initiator {
        let n = noise.write_message(&[], &mut out).map_err(|_| invalid())?;
        write_frame(&mut send, &out[..n]).await?;
        let incoming = read_frame(&mut recv, MAX_FRAME).await?;
        if noise
            .read_message(&incoming, &mut out)
            .map_err(|_| invalid())?
            != 0
        {
            return Err(invalid());
        }
    } else {
        let incoming = read_frame(&mut recv, MAX_FRAME).await?;
        if noise
            .read_message(&incoming, &mut out)
            .map_err(|_| invalid())?
            != 0
        {
            return Err(invalid());
        }
        let n = noise.write_message(&[], &mut out).map_err(|_| invalid())?;
        write_frame(&mut send, &out[..n]).await?;
    }
    let hash: [u8; 32] = noise
        .get_handshake_hash()
        .try_into()
        .map_err(|_| invalid())?;
    let sas = sas(&hash, qr.psk.as_bytes())?;
    let mut channel = Channel {
        conn,
        send,
        noise: noise.into_transport_mode().map_err(|_| invalid())?,
        remote,
        hash,
        sas,
    };
    channel
        .send(&wire::proof(device, &local, &hash, &prologue_hash))
        .await?;
    let incoming = read_frame(&mut recv, MAX_FRAME).await?;
    wire::verify_proof(
        &channel.receive(&incoming)?,
        &channel.remote,
        &hash,
        &prologue_hash,
    )?;
    channel
        .send(
            &cert
                .map(InstallationCert::to_bytes)
                .unwrap_or_else(|| vec![0xf6]),
        )
        .await?;
    let incoming = read_frame(&mut recv, MAX_FRAME).await?;
    let certificate = channel.receive(&incoming)?;
    if channel.remote.role == Role::Existing {
        let peer = channel.remote.existing.ok_or_else(invalid)?;
        let cert = InstallationCert::from_bytes(&certificate)?;
        verify_cert(&cert, peer)?;
        if cert.installation_id != channel.remote.installation
            || cert.transport_public_key != channel.remote.transport
        {
            return Err(invalid());
        }
        let Lookup::Found(packet) = backend.lookup(peer).await? else {
            return Err(invalid());
        };
        // Authority is evaluated at acceptance, after the final network await.
        // The clock may have advanced or rolled back during lookup/handshake.
        let now = clock.unix_seconds();
        if cert.created_at > now.saturating_add(300) {
            return Err(invalid());
        }
        let record = parse_record(&packet, peer, now)?;
        if !crate::account::publication::SignedRecord::new(packet, peer)?.current(now) {
            return Err(invalid());
        }
        if record.is_expired(now)
            || !record.devices.iter().any(|d| {
                d.installation_id == cert.installation_id
                    && d.transport_public_key == cert.transport_public_key
                    && d.kind == cert.kind
            })
        {
            return Err(invalid());
        }
    } else if certificate.as_slice() != [0xf6] {
        return Err(invalid());
    }
    Ok((channel, recv))
}
fn sas(hash: &[u8; 32], psk: &[u8; 32]) -> Result<String, CoreError> {
    let mut bytes = Zeroizing::new([0; 5]);
    Hkdf::<Sha256>::new(Some(hash), psk)
        .expand(b"ChatProject/pair-sas/v1", &mut *bytes)
        .map_err(|_| invalid())?;
    let code = data_encoding::BASE32_NOPAD.encode(&*bytes);
    Ok(format!("{}-{}", &code[..4], &code[4..]))
}
pub(crate) fn reader(
    mut recv: RecvStream,
    conn: Connection,
    flow: crate::account::FlowId,
    input: mpsc::Sender<super::session::Input>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        loop {
            let result = tokio::select! {biased;_=cancel.cancelled()=>break,result=read_frame(&mut recv,MAX_FRAME)=>result};
            let failed = result.is_err();
            let message = super::session::Input::Frame {
                flow,
                connection: conn.stable_id(),
                body: result,
            };
            tokio::select! {biased;_=cancel.cancelled()=>break,result=input.send(message)=>if result.is_err(){break}}
            if failed {
                break;
            }
        }
    });
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sas_fixed_hkdf_sha256_vector() {
        // Independently calculated with Python hmac/hashlib: extract(hash,PSK),
        // expand ASCII ChatProject/pair-sas/v1 || 0x01, first five bytes.
        assert_eq!(
            sas(
                &std::array::from_fn(|i| i as u8),
                &std::array::from_fn(|i| i as u8 + 32)
            )
            .unwrap(),
            "6RHQ-2TYQ"
        );
    }
}
