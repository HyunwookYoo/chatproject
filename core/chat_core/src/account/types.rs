use std::fmt;

use zeroize::Zeroizing;

macro_rules! public_id {
    ($name:ident, $size:expr) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub [u8; $size]);

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(
                    &data_encoding::BASE32_NOPAD
                        .encode(&self.0)
                        .to_ascii_lowercase(),
                )
            }
        }
    };
}

public_id!(PeerId, 32);
public_id!(InstallationId, 32);
public_id!(Generation, 16);
public_id!(FlowId, 16);
public_id!(TxId, 16);
public_id!(StoreId, 16);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Desktop,
    Mobile,
}

/// A secret owned by trusted Rust code, erased on drop. It cannot be logged,
/// cloned implicitly, or serialized.
///
/// ```compile_fail
/// use chat_core::account::Secret32;
/// let secret = Secret32::from([0; 32]);
/// println!("{secret:?}");
/// ```
/// ```compile_fail
/// use chat_core::account::Secret32;
/// let secret = Secret32::from([0; 32]);
/// let duplicate = secret.clone();
/// ```
/// ```compile_fail
/// use chat_core::account::Secret32;
/// let secret = Secret32::from([0; 32]);
/// serde_json::to_string(&secret).unwrap();
/// ```
pub struct Secret32(Zeroizing<[u8; 32]>);

impl From<[u8; 32]> for Secret32 {
    fn from(bytes: [u8; 32]) -> Self {
        Self(Zeroizing::new(bytes))
    }
}

impl Secret32 {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub(crate) fn as_mut_bytes(&mut self) -> &mut [u8; 32] {
        &mut self.0
    }
}

/// A public endpoint suggestion, never device authorization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointHint {
    pub transport_public_key: [u8; 32],
    pub relay_url: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvitePreview {
    pub peer_id: PeerId,
    pub issuing_cert: super::InstallationCert,
    pub endpoint_hint: EndpointHint,
    /// Signed display suggestion, never a verified real-world name.
    pub display_name: Option<String>,
    pub issued_at: u64,
    pub expires_at: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactView {
    pub peer_id: PeerId,
    pub alias: String,
    /// Local independent-channel confirmation of the full account safety digest.
    pub verified: bool,
    /// Sticky notice after a fresh proof uses a different installation.
    pub new_device: bool,
    /// True unless an authorized live B3 grant currently proves this peer.
    pub device_unverified: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyDisplay {
    pub digest: [u8; 32],
    pub groups: String,
    pub qr: String,
}
