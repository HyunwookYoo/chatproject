use zeroize::{Zeroize, Zeroizing};

use super::{AccountKeys, FlowId, derive_account, encode_recovery, random_secret};
use crate::{CoreError, ErrorCode};

/// One-shot secret response. Deliberately has no Debug, Clone or serialization.
///
/// ```compile_fail
/// fn log(display: chat_core::account::RecoveryDisplay) { println!("{display:?}"); }
/// ```
/// ```compile_fail
/// fn serialize(display: chat_core::account::RecoveryDisplay) { serde_json::to_string(&display).unwrap(); }
/// ```
pub struct RecoveryDisplay {
    pub flow: FlowId,
    pub words: Vec<String>,
    /// Three distinct one-based word positions.
    pub positions: [u8; 3],
}
impl Drop for RecoveryDisplay {
    fn drop(&mut self) {
        self.words.zeroize();
    }
}

pub(super) struct Creation {
    pub flow: FlowId,
    pub keys: AccountKeys,
    expected: Zeroizing<[String; 3]>,
}
impl Creation {
    pub fn begin() -> Result<(Self, RecoveryDisplay), CoreError> {
        let entropy = random_secret()?;
        let keys = derive_account(&entropy)?;
        let phrase = Zeroizing::new(encode_recovery(&entropy)?);
        let mut words = Zeroizing::new(
            phrase
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<String>>(),
        );
        let mut flow = FlowId([0; 16]);
        getrandom::fill(&mut flow.0).map_err(|_| {
            CoreError::new(ErrorCode::NotAvailable, "Secure randomness unavailable")
        })?;
        let mut positions = [0; 3];
        for i in 0..3 {
            loop {
                let mut byte = [0];
                getrandom::fill(&mut byte).map_err(|_| {
                    CoreError::new(ErrorCode::NotAvailable, "Secure randomness unavailable")
                })?;
                // Rejection sampling gives every word position equal probability.
                if byte[0] < 240 {
                    let position = byte[0] % 24 + 1;
                    if !positions[..i].contains(&position) {
                        positions[i] = position;
                        break;
                    }
                }
            }
        }
        positions.sort();
        let expected = Zeroizing::new(positions.map(|p| words[usize::from(p - 1)].clone()));
        Ok((
            Self {
                flow,
                keys,
                expected,
            },
            RecoveryDisplay {
                flow,
                words: std::mem::take(&mut *words),
                positions,
            },
        ))
    }
    pub fn matches(&self, flow: FlowId, answers: &[String; 3]) -> bool {
        if self.flow != flow {
            return false;
        }
        answers
            .iter()
            .zip(self.expected.iter())
            .all(|(answer, expected)| {
                if answer.len() > 64 {
                    return false;
                }
                let mut normalized = std::borrow::Cow::Borrowed(answer.as_str());
                bip39::Mnemonic::normalize_utf8_cow(&mut normalized);
                let mut normalized = Zeroizing::new(normalized.into_owned());
                normalized.make_ascii_lowercase();
                normalized.trim() == expected && normalized.split_whitespace().count() == 1
            })
    }
}
