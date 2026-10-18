//! Local approval state. A remote frame can only confirm code comparison.
use super::{
    PairPhase, PairView,
    channel::{self, Channel},
    wire::{Qr, Role},
};
use crate::{
    CoreError,
    account::{FlowId, clock::Clock, wire::invalid},
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;
pub(crate) enum Input {
    Incoming(iroh::endpoint::Connection),
    Frame {
        flow: FlowId,
        connection: usize,
        body: Result<Vec<u8>, CoreError>,
    },
}
pub(crate) struct Runtime {
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub stop: Option<super::PairTestStop>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub reached: Option<super::PairTestStop>,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub bytes: (usize, usize),
    pub send: mpsc::Sender<Input>,
    pub input: mpsc::Receiver<Input>,
    pub current: Option<Session>,
}
impl Default for Runtime {
    fn default() -> Self {
        let (send, input) = mpsc::channel(8);
        Self {
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            stop: None,
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            reached: None,
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            bytes: (0, 0),
            send,
            input,
            current: None,
        }
    }
}
/// B6 may construct this only after committing the exact disclosure transaction.
/// B5 has deliberately no production constructor.
#[allow(dead_code)]
pub(crate) struct DisclosurePermit {
    flow: FlowId,
    handshake_hash: [u8; 32],
    authority: Option<crate::account::publication::SignedRecord>,
}
pub(crate) struct Session {
    pub qr: Qr,
    pub role: Role,
    pub initiator: bool,
    pub channel: Option<Channel>,
    pub candidate: Option<[u8; 32]>,
    pub candidate_installation: Option<crate::account::InstallationId>,
    pub failures: u8,
    pub deadline: u64,
    pub local_confirmed: bool,
    pub remote_confirmed: bool,
    pub approved: bool,
    pub disclosure_possible: bool,
    pub transaction: Option<crate::account::TxId>,
    pub receiving: Option<super::transfer::Receiving>,
    pub phase: Option<PairPhase>,
    pub cancel: CancellationToken,
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub received: Zeroizing<Vec<u8>>,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}
impl Session {
    pub(crate) fn new(qr: Qr, role: Role, initiator: bool, clock: &dyn Clock) -> Self {
        let deadline = clock
            .monotonic_ms()
            .saturating_add(qr.expires.saturating_sub(clock.unix_seconds()).min(300) * 1000);
        Self {
            qr,
            role,
            initiator,
            channel: None,
            candidate: None,
            candidate_installation: None,
            failures: 0,
            deadline,
            local_confirmed: false,
            remote_confirmed: false,
            approved: false,
            disclosure_possible: false,
            transaction: None,
            receiving: None,
            phase: None,
            cancel: CancellationToken::new(),
            #[cfg(all(feature = "test-hooks", debug_assertions))]
            received: Zeroizing::new(vec![]),
        }
    }
    pub(crate) fn live(&self, clock: &dyn Clock) -> bool {
        self.failures < 3
            && clock.monotonic_ms() < self.deadline
            && clock.unix_seconds() < self.qr.expires
            && self
                .channel
                .as_ref()
                .is_none_or(|c| c.conn.close_reason().is_none())
    }
    pub(crate) fn bind(&mut self, remote: [u8; 32]) -> Result<(), CoreError> {
        if self.channel.is_some() || self.candidate.is_some_and(|p| p != remote) {
            return Err(invalid());
        }
        self.candidate = Some(remote);
        Ok(())
    }
    pub(crate) fn view(&self) -> PairView {
        PairView {
            flow: self.qr.flow,
            local_role: if self.role == Role::Existing { super::PairRole::Existing } else { super::PairRole::New },
            remote_installation: self.channel.as_ref().map(|c| c.remote.installation),
            remote_kind: self.channel.as_ref().and_then(|c| {
                if self.role == Role::Existing {
                    c.candidate_kind
                } else {
                    c.existing_record.as_ref().and_then(|record| record.record.devices.iter()
                        .find(|device| device.installation_id == c.remote.installation
                            && device.transport_public_key == c.remote.transport)
                        .map(|device| device.kind))
                }
            }),
            direction: self.qr.direction,
            phase: self.phase.unwrap_or(if self.approved {
                PairPhase::Approved
            } else if self.role == Role::Existing && self.local_confirmed && self.remote_confirmed {
                PairPhase::AwaitingApproval
            } else if self.channel.is_some() {
                PairPhase::Comparing
            } else {
                PairPhase::WaitingQr
            }),
            sas: self.channel.as_ref().map(|c| c.sas.clone()),
            expires_at: self.qr.expires,
            disclosure_possible: self.disclosure_possible,
            transaction: self.transaction.or(self.qr.resume),
        }
    }
    pub(crate) async fn confirm(&mut self) -> Result<(), CoreError> {
        if self.local_confirmed {
            return Ok(());
        }
        self.channel
            .as_mut()
            .ok_or_else(invalid)?
            .send(&[0x82, 1, 0])
            .await?;
        self.local_confirmed = true;
        Ok(())
    }
    pub(crate) fn approve(&mut self, clock: &dyn Clock) -> Result<(), CoreError> {
        if !self.live(clock)
            || self.role != Role::Existing
            || !self.local_confirmed
            || !self.remote_confirmed
            || self.channel.is_none()
        {
            return Err(invalid());
        }
        if !self.approved {
            self.deadline = self
                .deadline
                .min(clock.monotonic_ms().saturating_add(60_000));
            self.approved = true;
        }
        Ok(())
    }
    pub(crate) fn receive(
        &mut self,
        connection: usize,
        cipher: &[u8],
    ) -> Result<Option<Zeroizing<Vec<u8>>>, CoreError> {
        let channel = self.channel.as_mut().ok_or_else(invalid)?;
        if channel.conn.stable_id() != connection {
            return Err(invalid());
        }
        let plain = channel.receive(cipher)?;
        if plain.as_slice() == [0x82, 1, 0] {
            if self.remote_confirmed {
                return Err(invalid());
            }
            self.remote_confirmed = true;
            return Ok(None);
        }
        #[cfg(all(feature = "test-hooks", debug_assertions))]
        if self.role == Role::New
            && self.local_confirmed
            && self.remote_confirmed
            && plain.len() <= channel::MAX_PLAIN
            && plain.starts_with(b"B5 fixture:")
        {
            self.received
                .extend_from_slice(&plain[b"B5 fixture:".len()..]);
            if self.received.len() > channel::MAX_PLAIN {
                return Err(invalid());
            }
            return Ok(None);
        }
        if !self.local_confirmed || !self.remote_confirmed {
            return Err(invalid());
        }
        Ok(Some(plain))
    }
    pub(crate) fn committed_permit(
        &self,
        t: &super::transaction::PairTransaction,
        authority: &crate::account::publication::SignedRecord,
    ) -> Result<DisclosurePermit, CoreError> {
        let c = self.channel.as_ref().ok_or_else(invalid)?;
        let p = super::transfer::Payload::decode(&t.payload)?;
        if t.approved_flow != self.qr.flow
            || t.approved_hash != c.hash
            || p.cert.installation_id != c.remote.installation
            || p.cert.transport_public_key != c.remote.transport
            || Some(p.cert.kind) != c.candidate_kind
        {
            return Err(invalid());
        }
        Ok(DisclosurePermit {
            flow: self.qr.flow,
            handshake_hash: c.hash,
            authority: Some(authority.clone()),
        })
    }
    pub(crate) async fn send_receipt(
        &mut self,
        clock: &dyn Clock,
        r: &super::transfer::Receipt,
    ) -> Result<usize, CoreError> {
        if !self.live(clock)
            || self.role != Role::New
            || !self.local_confirmed
            || !self.remote_confirmed
            || r.flow != self.qr.flow
            || self.channel.as_ref().is_none_or(|c| c.hash != r.handshake)
        {
            return Err(invalid());
        }
        self.channel
            .as_mut()
            .ok_or_else(invalid)?
            .send(&r.bytes())
            .await
    }
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    pub(crate) fn fixture_permit(&self) -> Result<DisclosurePermit, CoreError> {
        Ok(DisclosurePermit {
            flow: self.qr.flow,
            handshake_hash: self.channel.as_ref().ok_or_else(invalid)?.hash,
            authority: None,
        })
    }
    /// B6 must supply a durable disclosure commit before calling this gate. The
    /// builder is invoked only AFTER all local/remote gates, so private data is not
    /// even borrowed by a sender while approval or the durable commit is missing.
    #[allow(dead_code)] // B6's durable commit will provide the production permit.
    pub(crate) async fn send_gated(
        &mut self,
        clock: &dyn Clock,
        permit: &DisclosurePermit,
        build: impl FnOnce() -> Zeroizing<Vec<u8>>,
    ) -> Result<usize, CoreError> {
        if !self.live(clock)
            || self.role != Role::Existing
            || !self.local_confirmed
            || !self.remote_confirmed
            || !self.approved
            || permit.flow != self.qr.flow
            || permit.authority.as_ref().is_some_and(|a| {
                !a.current(clock.unix_seconds()) || a.record.is_expired(clock.unix_seconds())
            })
        {
            return Err(invalid());
        }
        let channel = self.channel.as_mut().ok_or_else(invalid)?;
        if self.candidate != Some(channel.remote.transport)
            || self.candidate_installation != Some(channel.remote.installation)
            || permit.handshake_hash != channel.hash
        {
            return Err(invalid());
        }
        let plain = build();
        if plain.len() > channel::MAX_PLAIN {
            return Err(invalid());
        }
        self.disclosure_possible = true;
        channel.send(&plain).await
    }
}
