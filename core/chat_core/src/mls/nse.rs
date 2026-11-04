//! Read-only MLS decrypt for the iOS notification service extension (design 10).
//!
//! The app writes a snapshot of one conversation's `mls_kv` rows with [`export_snapshot`]; the
//! extension decrypts one ciphertext against it with [`decrypt_readonly`]. OpenMLS writes while
//! it decrypts (it re-saves the message secrets, spike B) and those writes land in an
//! in-memory `KvStore` that is dropped on return. The extension has no database, no
//! transaction and no `flush`, so no state can be written back.

use openmls::prelude::MlsMessageIn;
use openmls::prelude::tls_codec::Deserialize as _;
use rusqlite::{Connection, OptionalExtension as _, params};

use super::MlsProvider;
use super::group::{self, CLASS_NO_GROUP, MAX_MLS_MESSAGE_BYTES, OpenFailure, Opened};
use super::kv::{KvStore, storage_error};
use super::payload::Content;
use crate::message::types::ConversationId;
use crate::{CoreError, ErrorCode};

#[cfg(any(test, feature = "nse-fixture"))]
pub mod fixture;
mod state;

/// Longest preview, in bytes.
const PREVIEW_BYTES: usize = 48;

/// What the notification may show of a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NsePreview {
    /// Bytes of the message text.
    pub plaintext_len: u32,
    /// The start of the text, at most 48 bytes, cut at a character boundary.
    pub preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NseError {
    /// The snapshot is malformed or a stored value is unusable.
    #[error("bad state: {0}")]
    BadState(&'static str),
    /// The snapshot holds no group with the snapshot's group id.
    #[error("group not in state")]
    GroupNotFound,
    /// The bytes are not an MLS private message.
    #[error("bad ciphertext")]
    BadCiphertext,
    /// OpenMLS or the group state refused it; the number is the kind 17 `DecryptFailed.class`
    /// (1 past epoch, 2 future epoch, 3 out of order, 5 anything else, including a group
    /// state that cannot be loaded).
    #[error("decrypt failed (class {0})")]
    Decrypt(u8),
    /// A Commit. It was not applied.
    #[error("not an application message")]
    NotApplication,
    /// Authenticated, but not a text message (a receipt, an unknown kind or an undecodable
    /// payload): there is nothing to show.
    #[error("nothing to show")]
    NoPreview,
}

/// The snapshot for [`decrypt_readonly`]: the group id and the `mls_kv` rows of the
/// conversation's scope, in the `CNS2` format. Reads only.
///
/// The global rows (the installation's MLS signature key pair, KeyPackage private keys, PSKs)
/// are left out: decrypting needs none of them, and the extension must not hold long-term
/// keys. What remains is still plaintext at rest: the group's epoch secrets, secret tree and
/// private tree keys let anyone who reads the file decrypt that conversation's traffic of the
/// retained epochs. M7 must store the file in the App Group container with an iOS data
/// protection class (not `NSFileProtectionNone`) and delete it when the conversation goes.
///
/// Call it when no write to the MLS state is in flight (inside the writer's turn or a read
/// transaction), so the group id and the rows belong to one state.
pub fn export_snapshot(c: &Connection, conversation: ConversationId) -> Result<Vec<u8>, CoreError> {
    let group_id: Vec<u8> = c
        .query_row(
            "SELECT group_id FROM mls_conversation WHERE conversation=?1",
            params![conversation.0.as_slice()],
            |r| r.get(0),
        )
        .optional()
        .map_err(storage_error)?
        .ok_or_else(|| CoreError::new(ErrorCode::NotAvailable, "mls group not found"))?;
    let rows = KvStore::scope_rows(c, conversation.0)?;
    state::encode(&group_id, &rows)
        .map_err(|_| CoreError::new(ErrorCode::StorageCorrupt, "mls_snapshot_invalid"))
}

/// Decrypts one `MlsMessageOut` TLS ciphertext against a [`export_snapshot`] snapshot and
/// returns what a notification may show. Nothing is written anywhere: the rows are copied into
/// an in-memory store that is dropped on return, so the same ciphertext opens again and the
/// snapshot (which is only read) stays valid for the next one.
pub fn decrypt_readonly(snapshot: &[u8], ciphertext: &[u8]) -> Result<NsePreview, NseError> {
    let snapshot = state::decode(snapshot).map_err(|e| NseError::BadState(e.0))?;
    let well_formed = ciphertext.len() <= MAX_MLS_MESSAGE_BYTES
        && MlsMessageIn::tls_deserialize_exact(ciphertext)
            .ok()
            .and_then(|message| message.try_into_protocol_message().ok())
            .is_some();
    if !well_formed {
        return Err(NseError::BadCiphertext);
    }
    let store = KvStore::from_rows(snapshot.rows.iter().copied())
        .map_err(|_| NseError::BadState("invalid stored value"))?;
    let provider = MlsProvider::new(store);
    match group::open(&provider, snapshot.group_id, ciphertext) {
        Ok(Opened::Application { message, .. }) => match message.content {
            Content::Text { body } => Ok(NsePreview {
                plaintext_len: u32::try_from(body.len()).unwrap_or(u32::MAX),
                preview: preview_of(&body),
            }),
            _ => Err(NseError::NoPreview),
        },
        Ok(Opened::Undecodable { .. }) => Err(NseError::NoPreview),
        Ok(Opened::Commit { .. }) => Err(NseError::NotApplication),
        Err(OpenFailure {
            class: CLASS_NO_GROUP,
            ..
        }) => Err(NseError::GroupNotFound),
        Err(OpenFailure { class, .. }) => Err(NseError::Decrypt(class)),
    }
}

fn preview_of(body: &str) -> String {
    let mut end = body.len().min(PREVIEW_BYTES);
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    body[..end].to_owned()
}

#[cfg(test)]
mod tests {
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use rusqlite::params;

    use super::fixture::{Built, CONVERSATION, build_parties, message};
    use super::*;
    use crate::mls::MlsProvider;
    use crate::mls::group::{
        self, CLASS_FUTURE_EPOCH, CLASS_NO_GROUP, CLASS_OTHER, CLASS_PAST_EPOCH, Opened,
    };
    use crate::mls::kv::KvStore;
    use crate::mls::payload::Content;

    fn built() -> Built {
        build_parties(3, 3, 0)
    }

    fn snapshot_of(built: &Built) -> Vec<u8> {
        export_snapshot(&built.receiver, CONVERSATION).unwrap()
    }

    /// Every `mls_kv` row and the group row, as the app database holds them.
    fn database_rows(built: &Built) -> Vec<Vec<u8>> {
        let c = &built.receiver;
        let mut rows = Vec::new();
        let mut statement = c
            .prepare("SELECT scope,key,value FROM mls_kv ORDER BY key")
            .unwrap();
        let mut query = statement.query([]).unwrap();
        while let Some(row) = query.next().unwrap() {
            for column in 0..3 {
                rows.push(row.get::<_, Vec<u8>>(column).unwrap());
            }
        }
        rows.push(
            c.query_row("SELECT group_id FROM mls_conversation", [], |r| r.get(0))
                .unwrap(),
        );
        rows
    }

    fn body(seq: usize) -> String {
        match message(seq).content {
            Content::Text { body } => body,
            _ => unreachable!(),
        }
    }

    /// The snapshot's rows with `f` applied, re-encoded.
    fn rewrite(
        snapshot: &[u8],
        f: impl FnOnce(&mut Vec<u8>, &mut Vec<(Vec<u8>, Vec<u8>)>),
    ) -> Vec<u8> {
        let decoded = state::decode(snapshot).unwrap();
        let mut group_id = decoded.group_id.to_vec();
        let mut rows: Vec<(Vec<u8>, Vec<u8>)> = decoded
            .rows
            .iter()
            .map(|(key, value)| (key.to_vec(), value.to_vec()))
            .collect();
        f(&mut group_id, &mut rows);
        state::encode(&group_id, &rows).unwrap()
    }

    #[test]
    fn readonly_decrypt_does_not_change_snapshot() {
        let built = built();
        let snapshot = snapshot_of(&built);
        let before = database_rows(&built);
        for _ in 0..2 {
            // The second call would fail if the first had advanced the ratchet in the snapshot.
            let preview = decrypt_readonly(&snapshot, &built.ciphertexts[1]).unwrap();
            assert_eq!(preview.plaintext_len as usize, body(1).len());
        }
        assert_eq!(
            snapshot,
            snapshot_of(&built),
            "exporting again gives the same bytes"
        );
        assert_eq!(
            database_rows(&built),
            before,
            "the app database was not touched"
        );
        // And every other message still opens from the same, unchanged snapshot.
        for (seq, ciphertext) in built.ciphertexts.iter().enumerate() {
            let preview = decrypt_readonly(&snapshot, ciphertext).unwrap();
            assert!(
                preview
                    .preview
                    .starts_with(&format!("M1b probe message {seq} "))
            );
        }
    }

    #[test]
    fn readonly_decrypt_matches_app_open() {
        let built = built();
        let snapshot = snapshot_of(&built);
        let preview = decrypt_readonly(&snapshot, &built.ciphertexts[2]).unwrap();

        let app = MlsProvider::new(KvStore::load(&built.receiver, CONVERSATION.0).unwrap());
        let Ok(Opened::Application { message, .. }) =
            group::open(&app, &built.group_id, &built.ciphertexts[2])
        else {
            panic!("the app opens it")
        };
        let Content::Text { body: app_body } = message.content else {
            panic!("text")
        };
        assert_eq!(preview.plaintext_len as usize, app_body.len());
        assert_eq!(preview.preview, app_body[..PREVIEW_BYTES]);
        assert_eq!(app_body, body(2));
    }

    /// A key of a global label (the installation signature key pair), as `flush` files it.
    fn global_key(tail: &[u8]) -> Vec<u8> {
        let label = b"SignatureKeyPair";
        let mut key = vec![label.len() as u8];
        key.extend_from_slice(label);
        key.extend_from_slice(tail);
        key
    }

    /// Adds rows the snapshot must not carry: another conversation's, and global ones.
    fn add_foreign_and_global_rows(built: &Built) -> (Vec<u8>, Vec<Vec<u8>>) {
        let c = &built.receiver;
        let foreign = b"other-conversation".to_vec();
        let globals = vec![
            global_key(b"installation"),
            b"unlabelled-global-row".to_vec(),
        ];
        let insert = |scope: [u8; 32], key: &[u8]| {
            c.execute(
                "INSERT INTO mls_kv(scope,key,value) VALUES(?1,?2,?3)",
                params![scope.as_slice(), key, [1u8, 2, 3].as_slice()],
            )
            .unwrap();
        };
        insert([9; 32], &foreign);
        for key in &globals {
            insert([0; 32], key);
        }
        (foreign, globals)
    }

    #[test]
    fn snapshot_contains_only_the_conversation_scope_rows() {
        let built = built();
        let (foreign, globals) = add_foreign_and_global_rows(&built);
        let c = &built.receiver;
        let snapshot = snapshot_of(&built);
        let decoded = state::decode(&snapshot).unwrap();
        assert_eq!(decoded.group_id, built.group_id.as_slice());

        let expected: Vec<Vec<u8>> = c
            .prepare("SELECT key FROM mls_kv WHERE scope=?1 ORDER BY key")
            .unwrap()
            .query_map(params![CONVERSATION.0.as_slice()], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let keys: Vec<Vec<u8>> = decoded.rows.iter().map(|(key, _)| key.to_vec()).collect();
        assert_eq!(keys, expected);
        assert!(keys.len() > 3, "the conversation's own rows are there");
        assert!(!keys.contains(&foreign));
        for key in &globals {
            assert!(!keys.contains(key), "no global row in the snapshot");
        }
        assert!(keys.iter().all(|key| !crate::mls::kv::is_global(key)));
        // The database really holds global rows, so the filter had something to drop.
        let global_rows: i64 = c
            .query_row(
                "SELECT count(*) FROM mls_kv WHERE scope=?1",
                params![[0u8; 32].as_slice()],
                |r| r.get(0),
            )
            .unwrap();
        assert!(global_rows >= 2);
    }

    #[test]
    fn a_global_label_stored_under_the_conversation_scope_is_still_left_out() {
        let built = built();
        let key = global_key(b"misfiled");
        built
            .receiver
            .execute(
                "INSERT INTO mls_kv(scope,key,value) VALUES(?1,?2,?3)",
                params![CONVERSATION.0.as_slice(), key, [1u8, 2].as_slice()],
            )
            .unwrap();
        let snapshot = snapshot_of(&built);
        let decoded = state::decode(&snapshot).unwrap();
        assert!(decoded.rows.iter().all(|(row, _)| *row != key.as_slice()));
    }

    #[test]
    fn snapshot_without_global_rows_still_decrypts() {
        let built = built();
        add_foreign_and_global_rows(&built);
        let snapshot = snapshot_of(&built);
        let decoded = state::decode(&snapshot).unwrap();
        assert!(
            decoded
                .rows
                .iter()
                .all(|(key, _)| !crate::mls::kv::is_global(key))
        );
        for (seq, ciphertext) in built.ciphertexts.iter().enumerate() {
            let preview = decrypt_readonly(&snapshot, ciphertext).unwrap();
            assert!(
                preview
                    .preview
                    .starts_with(&format!("M1b probe message {seq} "))
            );
        }
        // The app, which holds everything, reads the same text.
        let app = MlsProvider::new(KvStore::load(&built.receiver, CONVERSATION.0).unwrap());
        assert!(matches!(
            group::open(&app, &built.group_id, &built.ciphertexts[0]),
            Ok(Opened::Application { .. })
        ));
    }

    #[test]
    fn unknown_conversation_cannot_be_exported() {
        let built = built();
        assert!(export_snapshot(&built.receiver, ConversationId([7; 32])).is_err());
    }

    #[test]
    fn a_commit_is_not_an_application_message_and_is_not_applied() {
        let built = built();
        let snapshot = snapshot_of(&built);
        assert_eq!(
            decrypt_readonly(&snapshot, &built.commit),
            Err(NseError::NotApplication)
        );
        // Nothing was kept: the same snapshot still opens the old epoch's messages.
        assert!(decrypt_readonly(&snapshot, &built.ciphertexts[0]).is_ok());
    }

    #[test]
    fn ciphertexts_of_other_epochs_are_errors() {
        let built = built();
        let snapshot = snapshot_of(&built);
        assert_eq!(
            decrypt_readonly(&snapshot, &built.next),
            Err(NseError::Decrypt(CLASS_FUTURE_EPOCH))
        );
        assert_eq!(
            decrypt_readonly(&snapshot, &built.stale),
            Err(NseError::Decrypt(CLASS_PAST_EPOCH))
        );
    }

    #[test]
    fn bad_ciphertexts_are_errors() {
        let built = built();
        let snapshot = snapshot_of(&built);
        assert!(matches!(
            decrypt_readonly(&snapshot, &[]),
            Err(NseError::BadCiphertext)
        ));
        assert!(matches!(
            decrypt_readonly(&snapshot, &built.ciphertexts[0][..10]),
            Err(NseError::BadCiphertext)
        ));
        let too_big = vec![0u8; group::MAX_MLS_MESSAGE_BYTES + 1];
        assert!(matches!(
            decrypt_readonly(&snapshot, &too_big),
            Err(NseError::BadCiphertext)
        ));
        let mut tampered = built.ciphertexts[0].clone();
        *tampered.last_mut().unwrap() ^= 0xFF; // inside the AEAD tag
        assert_eq!(
            decrypt_readonly(&snapshot, &tampered),
            Err(NseError::Decrypt(CLASS_OTHER))
        );
    }

    #[test]
    fn a_snapshot_for_another_group_is_group_not_found() {
        let built = built();
        let other = rewrite(&snapshot_of(&built), |group_id, _| group_id.push(0));
        assert_eq!(
            decrypt_readonly(&other, &built.ciphertexts[0]),
            Err(NseError::GroupNotFound)
        );
        assert_eq!(CLASS_NO_GROUP, 4);
        let empty = state::encode(&built.group_id, &[]).unwrap();
        assert_eq!(
            decrypt_readonly(&empty, &built.ciphertexts[0]),
            Err(NseError::GroupNotFound)
        );
    }

    #[test]
    fn broken_state_values_are_errors() {
        let built = built();
        let snapshot = snapshot_of(&built);
        // A value that is not a stored value at all (wrong format byte).
        let wrong_format = rewrite(&snapshot, |_, rows| rows[0].1[0] = b'{');
        assert!(matches!(
            decrypt_readonly(&wrong_format, &built.ciphertexts[0]),
            Err(NseError::BadState(_))
        ));
        // Garbage after the format byte of every row: the group cannot load.
        let garbage = rewrite(&snapshot, |_, rows| {
            for (_, value) in rows.iter_mut() {
                value.truncate(1);
                value.extend_from_slice(&[0xc1, 0xc1, 0xc1]);
            }
        });
        assert!(decrypt_readonly(&garbage, &built.ciphertexts[0]).is_err());
    }

    #[test]
    fn huge_length_claims_are_bad_state() {
        let built = built();
        let snapshot = snapshot_of(&built);
        let ciphertext = &built.ciphertexts[0];
        let mut group_len = snapshot.clone();
        group_len[4..8].copy_from_slice(&u32::MAX.to_be_bytes());
        let mut count = state::encode(b"g", &[]).unwrap();
        let at = count.len() - 4;
        count[at..].copy_from_slice(&u32::MAX.to_be_bytes());
        let mut field = state::encode(b"g", &[(b"k".to_vec(), vec![1])]).unwrap();
        let at = field.len() - 6; // value_len of the one row
        field[at..at + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        for claim in [group_len, count, field] {
            assert!(matches!(
                decrypt_readonly(&claim, ciphertext),
                Err(NseError::BadState(_))
            ));
        }
    }

    /// Flipping any one byte of the snapshot is an error or a normal result, never a panic.
    #[test]
    fn flipping_any_snapshot_byte_never_panics() {
        let built = build_parties(2, 1, 0);
        let snapshot = snapshot_of(&built);
        let ciphertext = &built.ciphertexts[0];
        assert!(decrypt_readonly(&snapshot, ciphertext).is_ok());
        let mut errors = 0;
        for at in 0..snapshot.len() {
            let mut flipped = snapshot.clone();
            flipped[at] ^= 0xFF;
            match catch_unwind(AssertUnwindSafe(|| decrypt_readonly(&flipped, ciphertext))) {
                Ok(Ok(_)) => {}
                Ok(Err(_)) => errors += 1,
                Err(_) => panic!(
                    "decrypt_readonly panicked with byte {at} of {} flipped",
                    snapshot.len()
                ),
            }
        }
        assert!(errors > 0, "some flips must be caught");
    }

    /// The same for the ciphertext against an intact snapshot.
    #[test]
    fn flipping_any_ciphertext_byte_never_panics() {
        let built = build_parties(2, 1, 0);
        let snapshot = snapshot_of(&built);
        let ciphertext = &built.ciphertexts[0];
        for at in 0..ciphertext.len() {
            let mut flipped = ciphertext.clone();
            flipped[at] ^= 0xFF;
            if catch_unwind(AssertUnwindSafe(|| decrypt_readonly(&snapshot, &flipped))).is_err() {
                panic!(
                    "decrypt_readonly panicked with byte {at} of {} flipped",
                    ciphertext.len()
                );
            }
        }
    }

    #[test]
    fn preview_is_at_most_48_bytes_and_whole_characters() {
        assert_eq!(preview_of("hello"), "hello");
        let long = "x".repeat(100);
        assert_eq!(preview_of(&long).len(), 48);
        let korean = "가".repeat(20); // 3 bytes each
        assert_eq!(preview_of(&korean), "가".repeat(16));
        let mixed = format!("{}가", "x".repeat(47)); // the character straddles byte 48
        assert_eq!(preview_of(&mixed), "x".repeat(47));
    }
}
