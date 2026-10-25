//! Private Release tests: signed packets and temporary encrypted storage, no endpoint.
use super::*;

fn retained_fixture() -> LocalAccount {
    let mut local = local_fixture(1);
    local.records.allocated_seq = ACCEPTED_SEQ;
    let accepted = local.records.accepted.as_ref().unwrap();
    local.records.conflict = Some(signed(&local.keys, &accepted.record, OLD_SEQ));
    local.records.state = PublicationState::Conflict;
    local
}

fn accepted_hash(h: &Harness) -> [u8; 32] {
    h.manager
        .local()
        .unwrap()
        .records
        .accepted
        .as_ref()
        .unwrap()
        .hash
}

fn no_authority(h: &Harness) {
    assert!(!h.manager.local_authorized());
    assert!(h.manager.auth.registry.0.lock().unwrap().is_empty());
    assert!(h.manager.auth.registry.1.lock().unwrap().is_empty());
    assert!(h.manager.auth.cached_lookups.is_empty());
    assert_eq!(h.backend.notified.load(Ordering::SeqCst), 0);
    assert!(h.events.0.lock().unwrap().is_empty());
    let local = h.manager.local().unwrap();
    if let Some(accepted) = &local.records.accepted {
        let grant = crate::account::auth::AuthGrant {
            peer: local.keys.peer_id(),
            installation: local.device.installation_id(),
            transport: local.device.transport_public_key(),
            record_hash: accepted.hash,
            generation: accepted.record.generation,
            expires_at: accepted.record.expires_at,
        };
        assert!(!h.manager.grant_authorized(&grant));
    }
}

async fn unchanged(h: &mut Harness, before: &[u8]) {
    assert!(h.bytes() == before);
    assert_eq!(h.manager.revision, 1);
    no_authority(h);
    h.reload().await;
    assert!(h.bytes() == before);
    assert!(h.manager.local().unwrap().records.conflict.is_some());
    no_authority(h);
}

#[tokio::test]
async fn retained_republish_exact_bytes_once_preserves_durable_conflict_and_unrelated_tombstone() {
    let local = retained_fixture();
    let packet = local
        .records
        .accepted
        .as_ref()
        .unwrap()
        .packet
        .as_bytes()
        .to_vec();
    let mut h = Harness::new(local, None).await;
    let before = h.bytes();
    let hash = accepted_hash(&h);
    assert!(h.manager.view().publication.can_republish_accepted);
    h.manager.republish_accepted_record(hash).await.unwrap();
    assert_eq!(h.backend.published.load(Ordering::SeqCst), 1);
    assert!(h.backend.published_bytes.lock().unwrap().as_slice() == [packet]);
    unchanged(&mut h, &before).await;
    assert!(h.manager.view().publication.can_republish_accepted);
    assert_eq!(h.manager.local().unwrap().records.tombstones.len(), 1);
    assert_eq!(
        h.manager.local().unwrap().records.state,
        PublicationState::Conflict
    );
}

#[tokio::test]
async fn retained_republish_guard_matrix_has_zero_publish_and_no_state_changes() {
    for case in [
        "wrong_hash",
        "accepted_missing",
        "accepted_expired",
        "accepted_future",
        "sequence_future",
        "observed_floor",
        "allocated_floor",
        "conflict_missing",
        "not_conflict",
        "conflict_equal",
        "conflict_higher",
        "conflict_generation",
        "conflict_membership",
        "local_generation",
        "installation_missing",
        "transport_mismatch",
        "current_installation_tombstone",
        "current_transport_tombstone",
        "listed_installation_tombstone",
        "listed_transport_tombstone",
        "pending_packet",
        "pending_held",
        "pending_notify",
    ] {
        let mut local = retained_fixture();
        let mut record = local.records.accepted.as_ref().unwrap().record.clone();
        let mut accepted_seq = ACCEPTED_SEQ;
        let mut conflict = local.records.conflict.as_ref().unwrap().record.clone();
        let mut conflict_seq = OLD_SEQ;
        let mut hash = local.records.accepted.as_ref().unwrap().hash;
        match case {
            "wrong_hash" => hash[0] ^= 1,
            "accepted_expired" => record.expires_at = NOW,
            "accepted_future" => record.issued_at = 1400,
            "sequence_future" => {
                accepted_seq = 1_400_000_000;
                local.records.observed_seq = accepted_seq;
            }
            "observed_floor" => local.records.observed_seq += 1,
            "allocated_floor" => local.records.allocated_seq += 1,
            "conflict_equal" => {
                conflict_seq = ACCEPTED_SEQ;
                conflict.issued_at += 1;
            }
            "conflict_higher" => {
                conflict_seq = ACCEPTED_SEQ + 1;
                local.records.observed_seq = conflict_seq;
            }
            "conflict_generation" => conflict.generation = Generation([8; 16]),
            "conflict_membership" => conflict.devices[0].kind = DeviceKind::Mobile,
            "local_generation" => local.records.generation = Generation([8; 16]),
            "installation_missing" => {
                record.devices[0].installation_id = local_fixture(2).device.installation_id();
                conflict = record.clone();
            }
            "transport_mismatch" => {
                record.devices[0].transport_public_key =
                    local_fixture(2).device.transport_public_key();
                conflict = record.clone();
            }
            "current_installation_tombstone"
            | "current_transport_tombstone"
            | "listed_installation_tombstone"
            | "listed_transport_tombstone" => {
                let extra = local_fixture(2);
                let extra_device =
                    extra.records.accepted.as_ref().unwrap().record.devices[0].clone();
                let target = if case.starts_with("listed") {
                    record.devices.push(extra_device.clone());
                    record.devices.sort_by_key(|d| d.installation_id);
                    conflict = record.clone();
                    extra_device
                } else {
                    record.devices[0].clone()
                };
                // Match only the named tombstone target, with the other target absent.
                let unrelated = local_fixture(3);
                let mut target = target;
                if case.contains("installation_tombstone") {
                    target.transport_public_key = unrelated.device.transport_public_key();
                } else {
                    target.installation_id = unrelated.device.installation_id();
                }
                local.records.tombstones.push(
                    RevokeIntent::sign(&local.keys, record.generation, hash, &target, NOW).unwrap(),
                );
            }
            _ => {}
        }
        let accepted = signed(&local.keys, &record, accepted_seq);
        if case != "wrong_hash" {
            hash = accepted.hash;
        }
        local.records.accepted = Some(accepted);
        local.records.conflict = Some(signed(&local.keys, &conflict, conflict_seq));
        match case {
            "accepted_missing" => local.records.accepted = None,
            "conflict_missing" => local.records.conflict = None,
            "not_conflict" => local.records.state = PublicationState::Observed,
            "pending_packet" | "pending_held" | "pending_notify" => {
                local.records.pending.push(PacketIntent {
                    signed: local.records.accepted.as_ref().unwrap().clone(),
                    base_hash: Some(hash),
                    operation: PacketOperation::Renew,
                    status: if case == "pending_notify" {
                        PacketStatus::AwaitingNotification
                    } else if case == "pending_held" {
                        PacketStatus::Held
                    } else {
                        PacketStatus::Ready
                    },
                    revoke_nonce: if case == "pending_notify" {
                        Some(local.records.tombstones[0].nonce)
                    } else {
                        None
                    },
                    recipients: None,
                })
            }
            _ => {}
        }
        let mut h = Harness::new(local, None).await;
        let before = h.bytes();
        if case != "wrong_hash" {
            assert!(
                !h.manager.view().publication.can_republish_accepted,
                "{case}"
            );
        }
        assert_eq!(
            h.manager
                .republish_accepted_record(hash)
                .await
                .unwrap_err()
                .code,
            if case == "wrong_hash" {
                ErrorCode::RecordConflict
            } else {
                ErrorCode::AccountRestricted
            },
            "{case}"
        );
        h.no_effects();
        assert!(h.bytes() == before, "{case}");
        assert_eq!(h.manager.revision, 1);
        no_authority(&h);
        h.reload().await;
        assert!(h.bytes() == before, "{case}");
        h.no_effects();
    }
}

#[tokio::test]
async fn retained_republish_backend_errors_preserve_state_and_do_not_retry() {
    for code in [ErrorCode::RecordConflict, ErrorCode::RecordUnavailable] {
        let mut h = Harness::new(retained_fixture(), None).await;
        let before = h.bytes();
        let hash = accepted_hash(&h);
        *h.backend.publish_error.lock().unwrap() = Some(code);
        assert_eq!(
            h.manager
                .republish_accepted_record(hash)
                .await
                .unwrap_err()
                .code,
            code
        );
        assert_eq!(h.backend.published.load(Ordering::SeqCst), 1);
        unchanged(&mut h, &before).await;
    }
}

#[tokio::test(start_paused = true)]
async fn retained_republish_timeout_preserves_state_without_retry() {
    let mut h = Harness::new(retained_fixture(), None).await;
    let before = h.bytes();
    let hash = accepted_hash(&h);
    h.backend.publish_hang.store(true, Ordering::SeqCst);
    assert_eq!(
        h.manager
            .republish_accepted_record(hash)
            .await
            .unwrap_err()
            .code,
        ErrorCode::RecordUnavailable
    );
    assert_eq!(h.backend.published.load(Ordering::SeqCst), 1);
    unchanged(&mut h, &before).await;
}

#[tokio::test]
async fn retained_republish_clock_changes_after_send_leave_no_authority() {
    for now in [2000, 600] {
        let mut h = Harness::new(retained_fixture(), None).await;
        let before = h.bytes();
        let hash = accepted_hash(&h);
        *h.backend.clock_after_publish.lock().unwrap() = Some((h.clock.clone(), now));
        assert_eq!(
            h.manager
                .republish_accepted_record(hash)
                .await
                .unwrap_err()
                .code,
            ErrorCode::AccountRestricted
        );
        assert_eq!(h.backend.published.load(Ordering::SeqCst), 1);
        assert!(!h.manager.view().publication.can_republish_accepted);
        unchanged(&mut h, &before).await;
    }
}

#[tokio::test]
async fn retained_republish_lifecycle_before_and_during_publish_remains_an_error() {
    for mode in ["before", "cancel", "ack"] {
        let mut h = Harness::new(retained_fixture(), None).await;
        let before = h.bytes();
        let hash = accepted_hash(&h);
        if mode != "before" {
            *h.backend.lifecycle_after_publish.lock().unwrap() = Some(h.manager.shared.clone());
            h.backend
                .publish_hang
                .store(mode == "cancel", Ordering::SeqCst);
        } else {
            h.manager.shared.requests.send_modify(|r| r.suspend = true);
        }
        assert_eq!(
            h.manager
                .republish_accepted_record(hash)
                .await
                .unwrap_err()
                .code,
            ErrorCode::StorageLocked
        );
        assert_eq!(
            h.backend.published.load(Ordering::SeqCst),
            usize::from(mode != "before")
        );
        assert!(!h.manager.view().publication.can_republish_accepted);
        unchanged(&mut h, &before).await;
    }
}

#[tokio::test]
async fn retained_republish_ack_stays_conflicted_until_lookup_and_explicit_confirm() {
    let local = retained_fixture();
    let old = local.records.conflict.as_ref().unwrap().clone();
    let accepted = local.records.accepted.as_ref().unwrap().clone();
    let mut h = Harness::new(local, Some(old.packet.clone())).await;
    h.manager
        .republish_accepted_record(accepted.hash)
        .await
        .unwrap();
    assert_eq!(
        h.manager.view().publication.state,
        PublicationState::Conflict
    );
    no_authority(&h);
    assert_eq!(
        h.manager
            .publication(Action::Confirm(old.hash))
            .await
            .unwrap_err()
            .code,
        ErrorCode::AccountRestricted
    );
    assert_eq!(h.backend.published.load(Ordering::SeqCst), 1);
    no_authority(&h);
    // A signed backend fixture supplies the subsequent lookup; this is not a physical network trial.
    *h.backend.lookup.lock().unwrap() = Some(accepted.packet);
    assert_eq!(
        h.manager
            .publication(Action::Refresh)
            .await
            .unwrap_err()
            .code,
        ErrorCode::RecordConflict
    );
    assert_eq!(h.backend.published.load(Ordering::SeqCst), 1);
    assert!(
        h.manager
            .local()
            .unwrap()
            .records
            .conflict
            .as_ref()
            .is_some_and(|c| c.hash == accepted.hash)
    );
    no_authority(&h);
    h.manager
        .publication(Action::Confirm(accepted.hash))
        .await
        .unwrap();
    assert_eq!(h.backend.published.load(Ordering::SeqCst), 2);
    assert!(h.manager.local().unwrap().records.conflict.is_none());
    assert!(!h.manager.view().publication.can_republish_accepted);
    let records = &h.manager.local().unwrap().records;
    assert_eq!(records.state, PublicationState::Pending);
    assert_eq!(records.pending.len(), 1);
    assert!(records.pending[0].operation == PacketOperation::Resolve);
    assert!(h.manager.auth.registry.0.lock().unwrap().is_empty());
    // Existing explicit Confirm permits ordinary authority from the accepted base;
    // its Resolve publish ACK still does not mark that new packet Observed.
    assert!(h.manager.local_authorized());
}

#[tokio::test]
async fn retained_republish_handle_carries_exact_hash_and_rejection() {
    let h = Harness::new(retained_fixture(), None).await;
    let hash = accepted_hash(&h);
    let (commands, mut input) = mpsc::channel(1);
    let handle = AccountHandle {
        commands,
        shared: h.manager.shared.clone(),
    };
    let pending = tokio::spawn(async move { handle.republish_accepted_record(hash).await });
    let queued = input.recv().await.unwrap();
    assert_eq!(queued.ticket, h.manager.ready().unwrap().ticket);
    assert!(matches!(&queued.command, Command::RepublishAccepted(value, _) if *value == hash));
    queued
        .command
        .reject(unavailable(ErrorCode::RecordConflict));
    assert_eq!(
        pending.await.unwrap().unwrap_err().code,
        ErrorCode::RecordConflict
    );
    h.no_effects();
}

#[tokio::test]
async fn retained_republish_new_lookup_evidence_disables_resend() {
    for case in ["higher", "membership"] {
        let local = retained_fixture();
        let accepted = local.records.accepted.as_ref().unwrap();
        let hash = accepted.hash;
        let mut record = accepted.record.clone();
        if case == "membership" {
            record.devices[0].kind = DeviceKind::Mobile;
        }
        let evidence = signed(&local.keys, &record, ACCEPTED_SEQ + 1);
        let mut h = Harness::new(local, Some(evidence.packet)).await;
        assert_eq!(
            h.manager
                .publication(Action::Refresh)
                .await
                .unwrap_err()
                .code,
            ErrorCode::RecordConflict
        );
        let before = h.bytes();
        assert!(!h.manager.view().publication.can_republish_accepted);
        assert_eq!(
            h.manager
                .republish_accepted_record(hash)
                .await
                .unwrap_err()
                .code,
            ErrorCode::AccountRestricted
        );
        h.no_effects();
        assert!(h.bytes() == before);
        no_authority(&h);
        h.reload().await;
        assert!(h.bytes() == before);
    }
}

#[tokio::test]
async fn retained_republish_requires_active_account_and_defaults_false() {
    assert!(!PublicationView::default().can_republish_accepted);
    for candidate in [false, true] {
        let mut h = Harness::new(retained_fixture(), None).await;
        let hash = accepted_hash(&h);
        h.manager.state = if candidate {
            Some(AccountState::Candidate(local_fixture(2).device))
        } else {
            None
        };
        assert!(!h.manager.view().publication.can_republish_accepted);
        assert_eq!(
            h.manager
                .republish_accepted_record(hash)
                .await
                .unwrap_err()
                .code,
            ErrorCode::AccountRestricted
        );
        h.no_effects();
    }
}

#[tokio::test]
async fn retained_republish_stale_displayed_accepted_hash_cannot_publish_replacement() {
    let mut h = Harness::new(retained_fixture(), None).await;
    let displayed = h.manager.view().publication;
    assert!(displayed.can_republish_accepted);
    let local = h.manager.local().unwrap();
    let mut records = local.records.clone();
    records.accepted = Some(signed(
        &local.keys,
        &records.accepted.as_ref().unwrap().record,
        ACCEPTED_SEQ + 1,
    ));
    records.observed_seq = ACCEPTED_SEQ + 1;
    records.allocated_seq = ACCEPTED_SEQ + 1;
    h.manager
        .commit_records(records, h.manager.ready().unwrap())
        .await
        .unwrap();
    let before = h.bytes();
    let revision = h.manager.revision;
    assert!(h.manager.view().publication.can_republish_accepted);
    assert_eq!(
        h.manager
            .republish_accepted_record(displayed.accepted_hash.unwrap())
            .await
            .unwrap_err()
            .code,
        ErrorCode::RecordConflict
    );
    h.no_effects();
    assert!(h.bytes() == before);
    assert_eq!(h.manager.revision, revision);
    no_authority(&h);
    h.reload().await;
    assert!(h.bytes() == before);
}
