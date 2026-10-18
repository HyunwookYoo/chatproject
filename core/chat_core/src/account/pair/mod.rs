//! Ephemeral pairing. Durable identity import/disclosure belongs to B6.
pub(crate) mod channel;
pub(crate) mod session;
pub(crate) mod wire;
use super::FlowId;
use zeroize::Zeroize;
pub(crate) const ALPN: &[u8] = b"chatproject/pair/1";
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairDirection {
    NewDisplays,
    ExistingDisplays,
}
/// One-shot QR response, deliberately neither Debug nor serializable.
///
/// ```compile_fail
/// fn log(display: chat_core::account::PairDisplay) { println!("{display:?}"); }
/// ```
/// ```compile_fail
/// fn serialize(display: chat_core::account::PairDisplay) { serde_json::to_string(&display).unwrap(); }
/// ```
pub struct PairDisplay {
    pub flow: FlowId,
    pub qr: String,
    pub expires_at: u64,
}
impl Drop for PairDisplay {
    fn drop(&mut self) {
        self.qr.zeroize();
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairPhase {
    WaitingQr,
    Comparing,
    Approved,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairView {
    pub flow: FlowId,
    pub direction: PairDirection,
    pub phase: PairPhase,
    pub sas: Option<String>,
    pub expires_at: u64,
    pub disclosure_possible: bool,
}
