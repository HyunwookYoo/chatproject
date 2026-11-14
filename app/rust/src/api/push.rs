//! Push notifications as the Flutter app drives them (M7 spec 5, 8): configuration at every
//! start, the OS token (iOS), the on/off setting, status, the iOS notification extension's
//! inbox, and the conversation a tapped notification names.
use std::future::Future;
use std::io::Read;
use std::path::{Path, PathBuf};

use chat_core::push::{PushConfig, Tag};
use push_wire::{Platform, BASE64URL_NOPAD, PUSH_CIPHER_MAX};

use super::chat::{CoreError, ErrorCode};

/// The service this device receives pushes through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushPlatform {
    Fcm,
    Apns,
}

/// This device's push setting and handles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushStatusView {
    pub enabled: bool,
    pub platform: Option<PushPlatform>,
    /// Conversations with a current handle of this device.
    pub handles: u32,
    pub oldest_issued_at_ms: Option<u64>,
}

impl From<PushPlatform> for Platform {
    fn from(platform: PushPlatform) -> Self {
        match platform {
            PushPlatform::Fcm => Platform::Fcm,
            PushPlatform::Apns => Platform::Apns,
        }
    }
}

impl From<Platform> for PushPlatform {
    fn from(platform: Platform) -> Self {
        match platform {
            Platform::Fcm => PushPlatform::Fcm,
            Platform::Apns => PushPlatform::Apns,
        }
    }
}

fn handle() -> Result<chat_core::account::AccountHandle, CoreError> {
    crate::global::CORE.account().map_err(Into::into)
}

/// At every start: the proxy address (`None`: this build's, from `CHATPROJECT_PUSH_URL`; no
/// address turns push off), the platform this device receives on (`None` on Windows) and the
/// iOS App Group folder for the notification extension. Release builds refuse `http://`
/// (`InvalidConfig`).
pub fn push_configure(
    proxy_url: Option<String>,
    platform: Option<PushPlatform>,
    snapshot_dir: Option<String>,
) -> Result<(), CoreError> {
    handle()?
        .push_configure(PushConfig {
            proxy_url: crate::native::push_url(proxy_url),
            platform: platform.map(Platform::from),
            snapshot_dir: snapshot_dir.map(PathBuf::from),
        })
        .map_err(Into::into)
}

/// A new OS push token (iOS; Kotlin passes Android's): every handle is issued again under it.
pub async fn push_set_token(platform: PushPlatform, token: String) -> Result<(), CoreError> {
    handle()?
        .push_set_token(platform.into(), token)
        .await
        .map_err(Into::into)
}

/// Push on, or off: off withdraws every handle and sends the proxy nothing until it is on.
pub async fn push_set_enabled(enabled: bool) -> Result<(), CoreError> {
    handle()?.push_set_enabled(enabled).await.map_err(Into::into)
}

pub async fn push_status() -> Result<PushStatusView, CoreError> {
    let status = handle()?.push_status().await.map_err(CoreError::from)?;
    Ok(PushStatusView {
        enabled: status.enabled,
        platform: status.platform.map(PushPlatform::from),
        handles: status.handles,
        oldest_issued_at_ms: status.oldest_issued_at_ms,
    })
}

/// The conversation a push tag (16 bytes) names, if it is one of this device's current
/// handles. Other lengths are `InvalidConfig`.
pub async fn push_conversation_of(tag: Vec<u8>) -> Result<Option<Vec<u8>>, CoreError> {
    let tag = Tag::try_from(tag.as_slice()).map_err(|_| invalid("Invalid push tag"))?;
    let conversation = handle()?
        .push_conversation_of(tag)
        .await
        .map_err(CoreError::from)?;
    Ok(conversation.map(|id| id.0.to_vec()))
}

/// The notification extension's inbox in `dir` (iOS): one JSON object per line, `{"t": tag,
/// "e": ciphertext}`, both base64url without padding.
const INBOX: &str = "inbox.jsonl";
/// The inbox while it is ingested. Taken by renaming, so the extension's next line starts a
/// new inbox; one left by an ingest that did not end is ingested first.
const TAKEN: &str = "inbox.taken.jsonl";
/// Read of one inbox file at most; a larger file is cut there (its rest is dropped: those
/// texts still arrive from the sender or my mailbox).
const INBOX_MAX: u64 = 8 << 20;
/// Entries handed to the core per call, so one call never holds the account actor long.
const INGEST_BATCH: usize = 64;

fn invalid(detail: &str) -> CoreError {
    CoreError { code: ErrorCode::InvalidConfig, detail: detail.into() }
}

fn unreadable() -> CoreError {
    CoreError { code: ErrorCode::StorageCorrupt, detail: "Push inbox unreadable".into() }
}

/// The entries of an inbox file; lines that are not an entry are skipped.
fn parse_inbox(bytes: &[u8]) -> Vec<(Tag, Vec<u8>)> {
    let decode = |value: &serde_json::Value, key: &str| {
        BASE64URL_NOPAD.decode(value.get(key)?.as_str()?.as_bytes()).ok()
    };
    bytes
        .split(|byte| *byte == b'\n')
        .filter_map(|line| {
            let value: serde_json::Value = serde_json::from_slice(line).ok()?;
            let tag = Tag::try_from(decode(&value, "t")?.as_slice()).ok()?;
            let ciphertext = decode(&value, "e")?;
            (ciphertext.len() <= PUSH_CIPHER_MAX).then_some((tag, ciphertext))
        })
        .collect()
}

fn read_taken(path: &Path) -> Result<Vec<(Tag, Vec<u8>)>, CoreError> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(INBOX_MAX).read_to_end(&mut bytes))
        .map_err(|_| unreadable())?;
    Ok(parse_inbox(&bytes))
}

/// Takes `dir`'s inbox and hands it to `ingest` in batches, in order; the taken file is removed
/// once all of it was ingested. A file left taken by an earlier run comes first. Returns the
/// texts stored.
async fn ingest_dir<F, Fut>(dir: &Path, mut ingest: F) -> Result<u32, CoreError>
where
    F: FnMut(Vec<(Tag, Vec<u8>)>) -> Fut,
    Fut: Future<Output = Result<u32, CoreError>>,
{
    let (inbox, taken) = (dir.join(INBOX), dir.join(TAKEN));
    let mut stored = 0u32;
    // At most the earlier run's file and the current one.
    for _ in 0..2 {
        if !taken.exists() {
            match std::fs::rename(&inbox, &taken) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(_) => return Err(unreadable()),
            }
        }
        let entries = read_taken(&taken)?;
        for batch in entries.chunks(INGEST_BATCH) {
            stored = stored.saturating_add(ingest(batch.to_vec()).await?);
        }
        std::fs::remove_file(&taken).map_err(|_| unreadable())?;
    }
    Ok(stored)
}

/// Ingests and empties the notification extension's inbox in `dir` (iOS, at launch and on
/// every return to the foreground). Returns how many texts were stored; ones stored already
/// and unknown tags are skipped. On `Err` what was not ingested stays for the next call.
pub async fn push_ingest_inbox(dir: String) -> Result<u32, CoreError> {
    let account = handle()?;
    ingest_dir(Path::new(&dir), |entries| {
        let account = account.clone();
        async move { account.push_ingest_inbox(entries).await.map_err(CoreError::from) }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(tag: &Tag, ciphertext: &[u8]) -> String {
        format!(
            "{{\"t\":\"{}\",\"e\":\"{}\"}}\n",
            BASE64URL_NOPAD.encode(tag),
            BASE64URL_NOPAD.encode(ciphertext)
        )
    }

    #[test]
    fn inbox_lines_parse_and_bad_ones_are_skipped() {
        let tag: Tag = [4; 16];
        let mut file = line(&tag, b"one");
        file.push_str("not json\n");
        file.push_str(&line(&[5; 16], b"two"));
        file.push_str("{\"t\":\"AAAA\",\"e\":\"AAAA\"}\n"); // a 3-byte tag
        file.push_str("{\"t\":\"BAQEBAQEBAQEBAQEBAQEBA\"}\n"); // no `e`
        file.push_str(&line(&tag, &vec![1; PUSH_CIPHER_MAX + 1]));
        file.push_str(&line(&tag, b"three")[..20]); // a line the extension did not finish
        assert_eq!(
            parse_inbox(file.as_bytes()),
            vec![(tag, b"one".to_vec()), ([5; 16], b"two".to_vec())]
        );
    }

    fn block<T>(future: impl Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(future)
    }

    #[test]
    fn inbox_is_taken_in_batches_and_removed() {
        let dir = tempfile::tempdir().unwrap();
        let none = block(ingest_dir(dir.path(), |_| async { Ok(1) })).unwrap();
        assert_eq!(none, 0, "no inbox, nothing ingested");

        // A taken file of an earlier run that did not end, then the current inbox.
        std::fs::write(dir.path().join(TAKEN), line(&[1; 16], b"old")).unwrap();
        let current: String = (0..INGEST_BATCH + 1).map(|i| line(&[2; 16], &[i as u8])).collect();
        std::fs::write(dir.path().join(INBOX), current).unwrap();
        let mut batches = Vec::new();
        let stored = block(ingest_dir(dir.path(), |entries| {
            batches.push(entries.clone());
            async move { Ok(entries.len() as u32) }
        }))
        .unwrap();
        assert_eq!(stored, INGEST_BATCH as u32 + 2);
        let sizes: Vec<usize> = batches.iter().map(Vec::len).collect();
        assert_eq!(sizes, vec![1, INGEST_BATCH, 1]);
        assert_eq!(batches[0][0], ([1; 16], b"old".to_vec()));
        assert_eq!(batches[2][0], ([2; 16], vec![INGEST_BATCH as u8]));
        assert!(!dir.path().join(INBOX).exists() && !dir.path().join(TAKEN).exists());

        // A failed ingest keeps the file for the next call.
        std::fs::write(dir.path().join(INBOX), line(&[3; 16], b"kept")).unwrap();
        let failed = block(ingest_dir(dir.path(), |_| async {
            Err(CoreError { code: ErrorCode::StorageLocked, detail: String::new() })
        }));
        assert_eq!(failed.unwrap_err().code, ErrorCode::StorageLocked);
        let mut again = Vec::new();
        block(ingest_dir(dir.path(), |entries| {
            again.extend(entries);
            async { Ok(1) }
        }))
        .unwrap();
        assert_eq!(again, vec![([3; 16], b"kept".to_vec())]);
    }
}
