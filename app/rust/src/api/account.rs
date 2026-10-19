//! Typed account boundary. Public snapshots are separate from one-shot secrets.
use super::chat::CoreError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountPhase {
    Unconfigured,
    Candidate,
    PendingImport,
    LocalReady,
    PublishPending,
    Active,
    Conflict,
    Locked,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishIntent {
    LocalCreate,
    Replacement,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationState {
    LocalBlocked,
    Pending,
    Observed,
    Conflict,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceKind {
    Desktop,
    Mobile,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairDirection {
    NewDisplays,
    ExistingDisplays,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairPhase {
    WaitingQr,
    Comparing,
    Approved,
    AwaitingApproval,
    DisclosurePossible,
    ReceiptReady,
    ReceiptVerified,
    PublishPending,
    Active,
    Observed,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceView {
    pub installation_id: Vec<u8>,
    pub transport_public_key: Vec<u8>,
    pub kind: DeviceKind,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordView {
    pub generation: Vec<u8>,
    pub issued_at: u64,
    pub expires_at: u64,
    pub devices: Vec<DeviceView>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevocationView {
    pub peer_id: Vec<u8>,
    pub generation: Vec<u8>,
    pub base_hash: Vec<u8>,
    pub target_installation: Vec<u8>,
    pub target_transport: Vec<u8>,
    pub nonce: Vec<u8>,
    pub issued_at: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationView {
    pub state: PublicationState,
    pub generation: Option<Vec<u8>>,
    pub observed_seq: u64,
    pub allocated_seq: u64,
    pub accepted_hash: Option<Vec<u8>>,
    pub accepted: Option<RecordView>,
    pub conflict_hash: Option<Vec<u8>>,
    pub conflict: Option<RecordView>,
    pub tombstones: Vec<RevocationView>,
    pub pending_count: u32,
    pub pending_notifications: Vec<RevocationView>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRecordView {
    pub peer: Vec<u8>,
    pub accepted_generation: Vec<u8>,
    pub conflict_hash: Option<Vec<u8>>,
    pub conflict: Option<RecordView>,
    pub cached: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContactView {
    pub peer_id: Vec<u8>,
    pub alias: String,
    pub verified: bool,
    pub new_device: bool,
    pub device_unverified: bool,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairRole { Existing, New }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairResumeView { pub transaction: Vec<u8>, pub role: PairRole }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairView {
    pub flow: Vec<u8>,
    pub direction: PairDirection,
    pub local_role: PairRole,
    pub remote_installation: Option<Vec<u8>>,
    pub remote_kind: Option<DeviceKind>,
    pub phase: PairPhase,
    pub sas: Option<String>,
    pub expires_at: u64,
    pub disclosure_possible: bool,
    pub transaction: Option<Vec<u8>>,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountView {
    pub revision: u64,
    pub phase: AccountPhase,
    pub lock_error: Option<super::chat::ErrorCode>,
    pub public_peer_id: Option<Vec<u8>>,
    pub installation: Option<Vec<u8>>,
    pub devices: Vec<DeviceView>,
    pub contacts: Vec<ContactView>,
    pub remote_records: Vec<RemoteRecordView>,
    pub pair: Option<PairView>,
    pub publish_intents: Vec<PublishIntent>,
    pub publication: PublicationView,
    pub disclosure_possible: bool,
    pub pending_pair_transactions: Vec<Vec<u8>>,
    pub pair_resumes: Vec<PairResumeView>,
    pub pending_import_requires_remote_revoke: bool,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CandidateInfo {
    pub installation_id: Vec<u8>,
    pub endpoint_id: String,
}
/// Ephemeral response; no Debug, Clone, serde, or event representation.
pub struct RecoveryDisplay {
    pub flow: Vec<u8>,
    pub words: Vec<String>,
    pub positions: Vec<u8>,
}
/// Ephemeral response; no Debug, Clone, serde, or event representation.
pub struct PairDisplay {
    pub flow: Vec<u8>,
    pub qr: String,
    pub expires_at: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvitePreview {
    pub peer_id: Vec<u8>,
    pub installation: DeviceView,
    pub display_name: Option<String>,
    pub issued_at: u64,
    pub expires_at: u64,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyDisplay {
    pub digest: Vec<u8>,
    pub groups: String,
    pub qr: String,
}

fn handle() -> Result<chat_core::account::AccountHandle, CoreError> {
    crate::global::CORE.account().map_err(Into::into)
}
fn id<const N: usize>(bytes: Vec<u8>) -> Result<[u8; N], CoreError> {
    bytes.try_into().map_err(|_| CoreError {
        code: super::chat::ErrorCode::InvalidConfig,
        detail: "Invalid public identifier".into(),
    })
}
pub async fn open_account() -> Result<(), CoreError> {
    if crate::global::CORE.account().is_ok() {
        return Ok(());
    }
    let info = crate::global::CORE.info().ok_or_else(|| CoreError {
        code: super::chat::ErrorCode::CoreNotStarted,
        detail: "Core not started".into(),
    })?;
    let protector = crate::platform_keys::provider(&info.data_dir).map_err(CoreError::from)?;
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    if storage_fixture::enabled() {
        return storage_fixture::open(protector);
    }
    crate::global::CORE
        .open_account(protector)
        .map(|_| ())
        .map_err(Into::into)
}
pub async fn retry_open() -> Result<AccountView, CoreError> {
    open_account().await?;
    let account = handle()?;
    account.retry_open().await.map_err(CoreError::from)?;
    account.snapshot().await.map(Into::into).map_err(Into::into)
}
pub async fn snapshot() -> Result<AccountView, CoreError> {
    handle()?
        .snapshot()
        .await
        .map(Into::into)
        .map_err(Into::into)
}
pub async fn prepare_candidate() -> Result<CandidateInfo, CoreError> {
    handle()?
        .prepare_candidate()
        .await
        .map(|v| CandidateInfo {
            installation_id: v.installation_id.0.to_vec(),
            endpoint_id: v.endpoint_id,
        })
        .map_err(Into::into)
}
pub async fn discard_pending_import() -> Result<(), CoreError> {
    handle()?.discard_pending_import().await.map_err(Into::into)
}
pub async fn begin_create() -> Result<RecoveryDisplay, CoreError> {
    let mut display = handle()?.begin_create().await.map_err(CoreError::from)?;
    Ok(RecoveryDisplay {
        flow: display.flow.0.to_vec(),
        words: std::mem::take(&mut display.words),
        positions: display.positions.to_vec(),
    })
}
pub async fn confirm_create(flow: Vec<u8>, answers: Vec<String>) -> Result<(), CoreError> {
    let mut answers = zeroize::Zeroizing::new(answers);
    let account = handle()?;
    let flow = chat_core::account::FlowId(id(flow)?);
    if answers.len() != 3 || answers.iter().any(|a| a.len() > 64) {
        return Err(CoreError {
            code: super::chat::ErrorCode::InvalidRecovery,
            detail: "Invalid recovery answers".into(),
        });
    }
    account
        .confirm_create(
            flow,
            std::array::from_fn(|i| std::mem::take(&mut answers[i])),
        )
        .await
        .map_err(Into::into)
}
pub async fn cancel_create(flow: Vec<u8>) -> Result<(), CoreError> {
    handle()?
        .cancel_create(chat_core::account::FlowId(id(flow)?))
        .await
        .map_err(Into::into)
}
pub async fn recover(words: String, confirm_replacement: bool) -> Result<(), CoreError> {
    let mut words = zeroize::Zeroizing::new(words);
    let account = handle()?;
    account
        .recover(std::mem::take(&mut *words), confirm_replacement)
        .await
        .map_err(Into::into)
}
pub async fn create_invite(name: Option<String>) -> Result<String, CoreError> {
    handle()?.create_invite(name).await.map_err(Into::into)
}
pub async fn accept_invite(input: String, alias: String) -> Result<ContactView, CoreError> {
    let mut input = zeroize::Zeroizing::new(input);
    let account = handle()?;
    account
        .accept_invite(std::mem::take(&mut *input), alias)
        .await
        .map(Into::into)
        .map_err(Into::into)
}
pub async fn contacts() -> Result<Vec<ContactView>, CoreError> {
    handle()?
        .contacts()
        .await
        .map(|v| v.into_iter().map(Into::into).collect())
        .map_err(Into::into)
}
pub async fn confirm_safety(peer: Vec<u8>, digest: Vec<u8>) -> Result<(), CoreError> {
    handle()?
        .confirm_safety(chat_core::account::PeerId(id(peer)?), id(digest)?)
        .await
        .map_err(Into::into)
}
pub async fn connect_account(peer: Vec<u8>) -> Result<(), CoreError> {
    handle()?
        .connect_account(chat_core::account::PeerId(id(peer)?))
        .await
        .map_err(Into::into)
}
pub async fn refresh_record(peer: Vec<u8>) -> Result<(), CoreError> {
    handle()?
        .refresh_record(chat_core::account::PeerId(id(peer)?))
        .await
        .map_err(Into::into)
}
pub async fn revoke_device(target: Vec<u8>) -> Result<(), CoreError> {
    handle()?
        .revoke_device(chat_core::account::InstallationId(id(target)?))
        .await
        .map_err(Into::into)
}
pub async fn confirm_remote_record(peer: Vec<u8>, hash: Vec<u8>) -> Result<(), CoreError> {
    handle()?.confirm_remote_record(chat_core::account::PeerId(id(peer)?), id(hash)?).await.map_err(Into::into)
}
pub async fn confirm_record_conflict(base_hash: Vec<u8>) -> Result<(), CoreError> {
    handle()?
        .confirm_record_conflict(id(base_hash)?)
        .await
        .map_err(Into::into)
}
pub async fn pair_begin(
    direction: PairDirection,
    resume: Option<Vec<u8>>,
) -> Result<PairDisplay, CoreError> {
    let mut display = handle()?
        .pair_begin(
            direction.into(),
            resume.map(id).transpose()?.map(chat_core::account::TxId),
        )
        .await
        .map_err(CoreError::from)?;
    Ok(PairDisplay {
        flow: display.flow.0.to_vec(),
        qr: std::mem::take(&mut display.qr),
        expires_at: display.expires_at,
    })
}
pub async fn pair_scan(qr: String) -> Result<Vec<u8>, CoreError> {
    let mut qr = zeroize::Zeroizing::new(qr);
    let account = handle()?;
    account
        .pair_scan(std::mem::take(&mut *qr))
        .await
        .map(|flow| flow.0.to_vec())
        .map_err(Into::into)
}
pub async fn pair_confirm_code(flow: Vec<u8>, matches: bool) -> Result<(), CoreError> {
    handle()?
        .pair_confirm_code(chat_core::account::FlowId(id(flow)?), matches)
        .await
        .map_err(Into::into)
}
pub async fn pair_approve(flow: Vec<u8>) -> Result<(), CoreError> {
    handle()?
        .pair_approve(chat_core::account::FlowId(id(flow)?))
        .await
        .map_err(Into::into)
}
pub async fn pair_cancel(flow: Vec<u8>) -> Result<(), CoreError> {
    handle()?
        .pair_cancel(chat_core::account::FlowId(id(flow)?))
        .await
        .map_err(Into::into)
}
pub async fn pair_view() -> Result<Option<PairView>, CoreError> {
    handle()?
        .pair_view()
        .await
        .map(|v| v.map(Into::into))
        .map_err(Into::into)
}
pub fn parse_invite(input: String, now: u64) -> Result<InvitePreview, CoreError> {
    let input = zeroize::Zeroizing::new(input);
    let v = chat_core::account::parse_invite(&input, now).map_err(CoreError::from)?;
    Ok(InvitePreview {
        peer_id: v.peer_id.0.to_vec(),
        installation: DeviceView {
            installation_id: v.issuing_cert.installation_id.0.to_vec(),
            transport_public_key: v.issuing_cert.transport_public_key.to_vec(),
            kind: v.issuing_cert.kind.into(),
        },
        display_name: v.display_name,
        issued_at: v.issued_at,
        expires_at: v.expires_at,
    })
}
pub fn safety_display(local: Vec<u8>, peer: Vec<u8>) -> Result<SafetyDisplay, CoreError> {
    let v = chat_core::account::safety_display(
        chat_core::account::PeerId(id(local)?),
        chat_core::account::PeerId(id(peer)?),
    );
    Ok(SafetyDisplay {
        digest: v.digest.to_vec(),
        groups: v.groups,
        qr: v.qr,
    })
}

/// Stable bridge stub; configuration exists only with both debug assertions and test-hooks.
/// Must run before core/account startup. There is no singleton reset or production fallback.
pub async fn test_storage_start(
    data_dir: String,
    failure: Option<super::chat::ErrorCode>,
) -> Result<super::chat::CoreInfo, CoreError> {
    #[cfg(all(feature = "test-hooks", debug_assertions))]
    {
        storage_fixture::start(data_dir, failure).await
    }
    #[cfg(not(all(feature = "test-hooks", debug_assertions)))]
    {
        let _ = (data_dir, failure);
        Err(CoreError {
            code: super::chat::ErrorCode::NotAvailable,
            detail: "Test configuration unavailable".into(),
        })
    }
}
#[cfg(all(feature = "test-hooks", debug_assertions))]
mod storage_fixture {
    use super::*;
    use chat_core::account::{storage::KeyProtector, AccountServices, Secret32, StoreId};
    use std::sync::{Arc, OnceLock};
    static MODE: OnceLock<Option<chat_core::ErrorCode>> = OnceLock::new();
    pub(super) fn enabled() -> bool {
        MODE.get().is_some()
    }
    pub(super) async fn start(
        data_dir: String,
        failure: Option<super::super::chat::ErrorCode>,
    ) -> Result<super::super::chat::CoreInfo, CoreError> {
        use super::super::chat::ErrorCode as E;
        let failure = match failure {
            None => None,
            Some(E::StorageLocked) => Some(chat_core::ErrorCode::StorageLocked),
            Some(E::StorageMissing) => Some(chat_core::ErrorCode::StorageMissing),
            _ => {
                return Err(CoreError {
                    code: E::InvalidConfig,
                    detail: "Invalid fixture mode".into(),
                })
            }
        };
        if crate::global::CORE.info().is_some() || MODE.get().is_some() {
            return Err(CoreError {
                code: E::Busy,
                detail: "Fixture must precede core startup".into(),
            });
        }
        let info = crate::global::CORE
            .start(
                chat_core::CoreConfig {
                    data_dir: data_dir.into(),
                },
                &crate::global::BUS,
            )
            .map_err(CoreError::from)?;
        crate::global::CORE
            .network()
            .map_err(CoreError::from)?
            .configure_local(None)
            .await
            .map_err(CoreError::from)?;
        MODE.set(failure).map_err(|_| CoreError {
            code: E::Busy,
            detail: "Fixture already configured".into(),
        })?;
        Ok(info.into())
    }
    struct Injected {
        real: Arc<dyn KeyProtector>,
        code: chat_core::ErrorCode,
    }
    impl KeyProtector for Injected {
        fn protect(&self, _: StoreId, _: &Secret32) -> Result<Vec<u8>, chat_core::CoreError> {
            Err(chat_core::CoreError::new(
                self.code,
                "Injected protected storage status",
            ))
        }
        fn unprotect(&self, _: StoreId, _: &[u8]) -> Result<Secret32, chat_core::CoreError> {
            Err(chat_core::CoreError::new(
                self.code,
                "Injected protected storage status",
            ))
        }
        fn delete(&self, id: StoreId) -> Result<(), chat_core::CoreError> {
            self.real.delete(id)
        }
    }
    pub(super) fn open(real: Arc<dyn KeyProtector>) -> Result<(), CoreError> {
        let provider: Arc<dyn KeyProtector> = match MODE.get().expect("configured fixture") {
            Some(code) => Arc::new(Injected { real, code: *code }),
            None => real,
        };
        crate::global::CORE
            .open_account_with_services(provider, AccountServices::offline_for_test())
            .map(|_| ())
            .map_err(Into::into)
    }
}

#[cfg(all(test, not(debug_assertions)))]
mod release_tests {
    #[tokio::test]
    async fn release_storage_fixture_is_unavailable() {
        let result = super::test_storage_start("unused".into(), None).await;
        assert!(matches!(
            result,
            Err(super::CoreError {
                code: super::super::chat::ErrorCode::NotAvailable,
                ..
            })
        ));
        assert!(crate::global::CORE.info().is_none());
    }
}
