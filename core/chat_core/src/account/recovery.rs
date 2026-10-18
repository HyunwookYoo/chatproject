use super::storage::schema::LocalAccount;
use super::{
    DeviceKind, PublishIntent,
    clock::{Clock, SystemClock},
    decode_recovery, derive_account, derive_device, random_secret, sign_cert,
};
use crate::CoreError;

pub(super) fn recover(words: &str, revision: u64) -> Result<LocalAccount, CoreError> {
    let entropy = decode_recovery(words)?;
    local_account(
        derive_account(&entropy)?,
        PublishIntent::Replacement,
        revision,
    )
}

pub(super) fn local_account(
    keys: super::AccountKeys,
    intent: PublishIntent,
    revision: u64,
) -> Result<LocalAccount, CoreError> {
    let device = derive_device(random_secret()?)?;
    let kind = if cfg!(any(target_os = "ios", target_os = "android")) {
        DeviceKind::Mobile
    } else {
        DeviceKind::Desktop
    };
    let cert = sign_cert(&keys, &device, kind, SystemClock::default().unix_seconds())?;
    Ok(LocalAccount {
        keys,
        device,
        cert,
        revision,
        intent,
        records: super::publication::RecordState::new()?,
        remote: vec![],
        contacts: vec![],
        pairs: Default::default(),
    })
}
