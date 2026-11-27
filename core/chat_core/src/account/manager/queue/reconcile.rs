//! Making this device's queues agree with what the verdict wants (spec 6.2, 6.3, 7):
//! * `plan` is pure: from what this device wants (`Want`), its conversations with a group and
//!   its `queue_own` rows, it lists the actions and when the next timer falls due. A
//!   conversation without an `Active` queue gets one; an unconfirmed one is created at the
//!   server again (its id is this device's, so a `CREATE` sent twice is one queue: a crash
//!   before the answer converges, Review Focus 2); one older than `ROTATE_MS` is replaced, the
//!   old one `Retiring`; with no queue wanted every `Active` one retires with a withdrawal; a
//!   `Retiring` one past its `delete_at` is deleted at the server, or with the mode "never"
//!   only dropped here.
//! * The actor carries a plan out: the local half (new rows with their keys written before any
//!   `CREATE`, retirements with their withdrawals sealed) is written one queue per transaction,
//!   and only rows that were written go to the server; the `CREATE` and `DELETE` batches (at
//!   most `MAX_ITEMS` items a request) run in a spawned task, nothing waits on them in a
//!   transaction or an actor turn, and their answers are stored the same way, one transaction
//!   confirming each created queue and sealing its `QueueRef`. The frames ride `push_delivery`
//!   (kind 2), sent by the push handles' delivery loop.
//! * It runs when what it reads changes: the queue address, the verdict, the mode, a device
//!   list, a conversation that got its group (created or joined); when a timer is due
//!   (rotation, deletion); hourly; and after a failed request, with the M4 back-off (1 s
//!   doubling to 5 min). Never two at once.
//! * A queue whose `QueueRef` cannot be sealed, an item the server refused (`limited`, `bad`),
//!   a request it refused (`QueueError::refused`), or answers that could not be stored wait for
//!   the hourly check: a lasting fault costs the server one `CREATE` item an hour for each
//!   queue (its limit is 500 an hour), not one each back-off step.
//! * No address, or the mode "never": no request to the queue server of any kind.

use super::*;

/// What this device wants of its queues (spec 5.3): one per conversation, none, or none and no
/// request to the queue server either (the mode "never").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Want {
    Queues,
    None,
    Never,
}

impl Want {
    fn of(mode: QueueMode, verdict: &Verdict) -> Self {
        match mode {
            QueueMode::Never => Self::Never,
            mode if verdict::wants_queue(mode, verdict) => Self::Queues,
            _ => Self::None,
        }
    }
}

/// One step that makes the queues what is wanted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum QueueAction {
    /// A new `Active` queue: its keys are written, then it is created at the server and its
    /// `QueueRef` sealed.
    Create { conversation: ConversationId },
    /// An `Active` queue the server may not have: created there (again), then announced.
    Announce { queue: QueueId },
    /// `old` goes `Retiring` without a withdrawal, and a new queue replaces it (spec 6.3).
    Rotate {
        conversation: ConversationId,
        old: QueueId,
    },
    /// `Active` goes `Retiring`, its withdrawal sealed.
    Retire { queue: QueueId },
    /// A `Retiring` queue past its `delete_at`: deleted at the server, then its row.
    Delete { queue: QueueId },
    /// The same, with no request: its row only (the mode "never").
    Forget { queue: QueueId },
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Plan {
    pub actions: Vec<QueueAction>,
    /// Wall time the next rotation or deletion falls due, once these actions are done.
    pub wake_at: Option<u64>,
}

/// The actions that make `own` (this device's queues) what `want` asks for at wall time `now`,
/// given the conversations with a group. The same input gives the same plan; done, it gives
/// none.
pub(super) fn plan(
    want: Want,
    conversations: &[ConversationId],
    own: &[OwnQueue],
    now: u64,
) -> Plan {
    let mut actions = Vec::new();
    let mut wake_at: Option<u64> = None;
    let mut wake = |at: u64| wake_at = Some(wake_at.map_or(at, |w| w.min(at)));
    for queue in own {
        if let QueueState::Retiring { delete_at_ms } = queue.state {
            if now < delete_at_ms {
                wake(delete_at_ms);
            } else if want == Want::Never {
                actions.push(QueueAction::Forget { queue: queue.id });
            } else {
                actions.push(QueueAction::Delete { queue: queue.id });
            }
        }
    }
    let active = |conversation: &ConversationId| {
        own.iter()
            .find(|q| q.state == QueueState::Active && q.conversation == *conversation)
    };
    if want == Want::Queues {
        for conversation in conversations {
            match active(conversation) {
                None => {
                    actions.push(QueueAction::Create {
                        conversation: *conversation,
                    });
                    wake(now.saturating_add(ROTATE_MS));
                }
                Some(queue) if !queue.confirmed => {
                    actions.push(QueueAction::Announce { queue: queue.id });
                }
                Some(queue) if now >= queue.created_at_ms.saturating_add(ROTATE_MS) => {
                    actions.push(QueueAction::Rotate {
                        conversation: *conversation,
                        old: queue.id,
                    });
                    wake(now.saturating_add(RETIRE_MS));
                    wake(now.saturating_add(ROTATE_MS));
                }
                Some(queue) => wake(queue.created_at_ms.saturating_add(ROTATE_MS)),
            }
        }
    }
    for queue in own {
        let kept = want == Want::Queues && conversations.contains(&queue.conversation);
        if queue.state == QueueState::Active && !kept {
            actions.push(QueueAction::Retire { queue: queue.id });
            wake(now.saturating_add(RETIRE_MS));
        }
    }
    Plan { actions, wake_at }
}

/// A `CREATE` of queue `id`, signed by its recipient key at `ts`.
fn create_item(id: &QueueId, recipient: &[u8; 32], sender: &[u8; 32], ts: u64) -> CreateItem {
    let sender_pub = public_key(sender);
    let signed = Signed::Create {
        sender_pub: &sender_pub,
    };
    CreateItem {
        queue_id: b64(id),
        recipient_pub: b64(&public_key(recipient)),
        sender_pub: b64(&sender_pub),
        ts,
        sig: b64(&sign(recipient, &signing_bytes(&signed, id, ts))),
    }
}

fn delete_item(id: &QueueId, recipient: &[u8; 32], ts: u64) -> DeleteItem {
    DeleteItem {
        queue_id: b64(id),
        ts,
        sig: b64(&sign(recipient, &signing_bytes(&Signed::Delete, id, ts))),
    }
}

/// A queue id and its two keys, from the OS.
fn new_queue(conversation: ConversationId) -> Result<NewQueue, CoreError> {
    let random = || {
        let mut bytes = Zeroizing::new([0u8; 32]);
        getrandom::fill(bytes.as_mut())
            .map_err(|_| CoreError::new(ErrorCode::NotAvailable, "OS randomness unavailable"))?;
        Ok::<_, CoreError>(bytes)
    };
    Ok(NewQueue {
        id: *random()?,
        conversation,
        recipient_secret: random()?,
        sender_secret: random()?,
    })
}

impl Manager {
    /// A run failed: the next one waits the M4 back-off.
    fn queue_failed(&mut self) {
        let now = self.schedule_now();
        self.messages.queue.check.fail(now);
    }

    /// The next run waits for the hourly check: a run is done, or what is left of it would
    /// only spend the server's `CREATE` limit sooner (see `QueueEvent::Answered.failed`).
    fn queue_check_later(&mut self) {
        let now = self.schedule_now();
        self.messages.queue.check.ok(now, CHECK_MS);
    }

    /// Each peer account's current devices, for the frames sealed for them.
    fn queue_devices(&self) -> Vec<(PeerId, Vec<InstallationId>)> {
        let Ok(local) = self.local() else {
            return Vec::new();
        };
        local
            .remote
            .iter()
            .map(|r| (r.peer, self.current_installations(r.peer)))
            .collect()
    }

    /// Plans for `verdict` and carries the plan out.
    pub(super) async fn queue_run(&mut self, verdict: Verdict, request: Request) {
        let Some(server) = self.messages.queue.server.clone() else {
            return;
        };
        let queue = &mut self.messages.queue;
        queue.due = false;
        queue.planned = Some(verdict);
        let view = match self.queue_db(QueueDbOp::Load, request).await {
            Ok(QueueDbReply::Loaded(view)) => view,
            _ => return self.queue_failed(),
        };
        let wall = self.services.clock.wall_ms();
        let want = Want::of(view.mode, &verdict);
        let Plan { actions, wake_at } = plan(want, &view.conversations, &view.own, wall);
        self.messages.queue.wake_at = wake_at;
        let mut retire = Vec::new();
        let mut new = Vec::new();
        let mut forget = Vec::new();
        let mut creates = Vec::new();
        // The new queues' `CREATE`s, sent once their rows are written.
        let mut fresh = Vec::new();
        let mut deletes = Vec::new();
        let row = |id: QueueId| view.own.iter().find(|q| q.id == id);
        for action in actions {
            let made = match action {
                QueueAction::Create { conversation } => Some(conversation),
                QueueAction::Rotate { conversation, old } => {
                    retire.push((old, false));
                    Some(conversation)
                }
                QueueAction::Announce { queue } => {
                    if let Some(q) = row(queue) {
                        let item = create_item(&q.id, &q.recipient_secret, &q.sender_secret, wall);
                        creates.push((q.id, item));
                    }
                    None
                }
                QueueAction::Retire { queue } => {
                    retire.push((queue, true));
                    None
                }
                QueueAction::Delete { queue } => {
                    if let Some(q) = row(queue) {
                        deletes.push((q.id, delete_item(&q.id, &q.recipient_secret, wall)));
                    }
                    None
                }
                QueueAction::Forget { queue } => {
                    forget.push(queue);
                    None
                }
            };
            if let Some(conversation) = made {
                let Ok(queue) = new_queue(conversation) else {
                    return self.queue_failed();
                };
                let item = create_item(
                    &queue.id,
                    &queue.recipient_secret,
                    &queue.sender_secret,
                    wall,
                );
                fresh.push((queue.id, item));
                new.push(queue);
            }
        }
        if !(retire.is_empty() && new.is_empty() && forget.is_empty()) {
            let op = QueueDbOp::Apply {
                retire,
                new,
                forget,
                devices: self.queue_devices(),
                wall,
                now: self.schedule_now(),
            };
            match self.queue_db(op, request).await {
                Ok(QueueDbReply::Applied { withdrawn, written }) => {
                    if withdrawn > 0 {
                        self.push_deliver_soon();
                    }
                    // No request goes for a row that is not there (its keys are written first).
                    creates.extend(fresh.into_iter().filter(|(id, _)| written.contains(id)));
                }
                _ => return self.queue_failed(),
            }
        }
        if creates.is_empty() && deletes.is_empty() {
            return self.queue_check_later();
        }
        if self.queue_quiet() {
            // The server refused a request: the rows wait for the hourly check.
            let now = self.schedule_now();
            let queue = &mut self.messages.queue;
            queue.check.ok(now, queue.quiet_until.saturating_sub(now));
            return;
        }
        self.messages.queue.running = true;
        let mailbox = self.auth.send.clone();
        let ticket = request.ticket;
        let (creates, deletes) = (batches(creates), batches(deletes));
        tokio::spawn(async move {
            let client = server.client.clone();
            let (created, mut failure) = run_batches(creates, |items| client.create(items)).await;
            let deleted = if failure.is_some() {
                Vec::new()
            } else {
                let (deleted, delete_failure) =
                    run_batches(deletes, |items| client.delete(items)).await;
                failure = delete_failure;
                deleted
            };
            let refused = failure.is_some_and(|f| f.refused());
            let mut event = (Vec::new(), Vec::new(), Vec::new());
            // Some item answered `limited` or `bad` (see `QueueEvent::Answered.failed`).
            let mut rejected = false;
            for (id, result) in created {
                match result {
                    ItemResult::Ok => event.0.push(id),
                    ItemResult::Exists => event.1.push(id),
                    _ => rejected = true,
                }
            }
            for (id, result) in deleted {
                match result {
                    ItemResult::Ok | ItemResult::Gone => event.2.push(id),
                    _ => rejected = true,
                }
            }
            let failed = failure.is_some() && !rejected && !refused;
            let (created, exists, deleted) = event;
            let _ = mailbox
                .send(Input::Queue(QueueEvent::Answered {
                    ticket,
                    url: server.url,
                    created,
                    exists,
                    deleted,
                    failed,
                    refused,
                }))
                .await;
        });
    }

    /// A reconcile run's `CREATE`s and `DELETE`s came back.
    pub(super) async fn queue_answered(
        &mut self,
        ticket: u64,
        url: Arc<str>,
        created: Vec<QueueId>,
        refused: Vec<QueueId>,
        deleted: Vec<QueueId>,
        failed: bool,
    ) {
        self.messages.queue.running = false;
        let Some(request) = self.ready().ok().filter(|r| r.ticket == ticket) else {
            // The account was locked meanwhile: the rows say what is left.
            self.messages.queue.due = true;
            return;
        };
        if !(created.is_empty() && refused.is_empty() && deleted.is_empty()) {
            // A queue some other keys hold its id: a new one is made for its conversation.
            let again = !refused.is_empty();
            let op = QueueDbOp::Settle {
                created,
                refused,
                deleted,
                server_url: url.to_string(),
                devices: self.queue_devices(),
                wall: self.services.clock.wall_ms(),
                now: self.schedule_now(),
            };
            match self.queue_db(op, request).await {
                Ok(QueueDbReply::Settled { announced }) => {
                    if announced > 0 {
                        self.push_deliver_soon();
                    }
                }
                // The server holds the created queues already: they are asked for again at
                // the hourly check, not at each back-off step.
                _ => return self.queue_check_later(),
            }
            self.messages.queue.due |= again;
        }
        if failed {
            self.queue_failed();
        } else {
            self.queue_check_later();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: u64 = 1_800_000_000_000;

    fn conv(n: u8) -> ConversationId {
        ConversationId([n; 32])
    }

    fn row(
        n: u8,
        conversation: u8,
        created_at_ms: u64,
        state: QueueState,
        confirmed: bool,
    ) -> OwnQueue {
        OwnQueue {
            id: [n; 32],
            conversation: conv(conversation),
            recipient_secret: Zeroizing::new([n; 32]),
            sender_secret: Zeroizing::new([n; 32]),
            created_at_ms,
            state,
            confirmed,
        }
    }

    #[test]
    fn plan_creates_one_queue_per_conversation_without_one() {
        let own = [row(1, 1, T, QueueState::Active, true)];
        let plan = plan(Want::Queues, &[conv(1), conv(2), conv(3)], &own, T + 5);
        assert_eq!(
            plan.actions,
            [
                QueueAction::Create {
                    conversation: conv(2)
                },
                QueueAction::Create {
                    conversation: conv(3)
                },
            ]
        );
        // The kept queue rotates before the new ones would.
        assert_eq!(plan.wake_at, Some(T + ROTATE_MS));
    }

    #[test]
    fn plan_announces_an_unconfirmed_queue_again_and_makes_no_second_one() {
        let own = [row(1, 1, T, QueueState::Active, false)];
        let plan = plan(Want::Queues, &[conv(1)], &own, T + ROTATE_MS + 1);
        assert_eq!(plan.actions, [QueueAction::Announce { queue: [1; 32] }]);
    }

    #[test]
    fn plan_rotates_after_7_days_and_deletes_after_48_hours() {
        let own = [
            row(1, 1, T, QueueState::Active, true),
            row(
                2,
                1,
                T - 10,
                QueueState::Retiring {
                    delete_at_ms: T + RETIRE_MS,
                },
                true,
            ),
        ];
        let early = plan(Want::Queues, &[conv(1)], &own, T + RETIRE_MS - 1);
        assert!(early.actions.is_empty());
        assert_eq!(early.wake_at, Some(T + RETIRE_MS));
        let now = T + ROTATE_MS;
        let late = plan(Want::Queues, &[conv(1)], &own, now);
        assert_eq!(
            late.actions,
            [
                QueueAction::Delete { queue: [2; 32] },
                QueueAction::Rotate {
                    conversation: conv(1),
                    old: [1; 32]
                },
            ]
        );
        assert_eq!(late.wake_at, Some(now + RETIRE_MS));
    }

    #[test]
    fn plan_retires_every_active_queue_when_none_is_wanted() {
        let own = [
            row(1, 1, T, QueueState::Active, true),
            row(2, 2, T, QueueState::Active, false),
            row(
                3,
                3,
                T,
                QueueState::Retiring {
                    delete_at_ms: T + 9,
                },
                true,
            ),
        ];
        for want in [Want::None, Want::Never] {
            let plan = plan(want, &[conv(1), conv(2), conv(3)], &own, T);
            assert_eq!(
                plan.actions,
                [
                    QueueAction::Retire { queue: [1; 32] },
                    QueueAction::Retire { queue: [2; 32] },
                ]
            );
            assert_eq!(plan.wake_at, Some(T + 9));
        }
        // A queue whose conversation has no group retires too.
        let plan = plan(Want::Queues, &[conv(2)], &own, T);
        assert_eq!(
            plan.actions,
            [
                QueueAction::Announce { queue: [2; 32] },
                QueueAction::Retire { queue: [1; 32] }
            ]
        );
    }

    #[test]
    fn plan_never_drops_retired_queues_without_a_request() {
        let own = [row(3, 3, T, QueueState::Retiring { delete_at_ms: T }, true)];
        assert_eq!(
            plan(Want::Never, &[], &own, T).actions,
            [QueueAction::Forget { queue: [3; 32] }]
        );
        assert_eq!(
            plan(Want::None, &[], &own, T).actions,
            [QueueAction::Delete { queue: [3; 32] }]
        );
    }

    #[test]
    fn plan_done_is_empty() {
        let own = [
            row(1, 1, T, QueueState::Active, true),
            row(2, 2, T, QueueState::Active, true),
        ];
        let plan = plan(Want::Queues, &[conv(1), conv(2)], &own, T + 1);
        assert!(plan.actions.is_empty());
        assert!(
            super::plan(Want::None, &[conv(1)], &[], T)
                .actions
                .is_empty()
        );
    }

    /// Plan "성능": 100 conversations turning on make 100 `Create` actions, and their items go
    /// in one request; one more goes in a second.
    #[tokio::test]
    async fn create_batches_100_conversations_in_one_request() {
        struct Counting(std::sync::Mutex<Vec<usize>>);
        impl QueueServer for Counting {
            fn create(&self, items: Vec<CreateItem>) -> crate::queue::QueueFuture {
                self.0.lock().unwrap().push(items.len());
                let n = items.len();
                Box::pin(async move { Ok(vec![ItemResult::Ok; n]) })
            }
            fn send(&self, _: Vec<crate::queue::SendItem>) -> crate::queue::QueueFuture {
                unreachable!()
            }
            fn recv(&self, _: Vec<crate::queue::RecvItem>) -> crate::queue::QueueFuture {
                unreachable!()
            }
            fn ack(&self, _: Vec<crate::queue::AckItem>) -> crate::queue::QueueFuture {
                unreachable!()
            }
            fn delete(&self, _: Vec<DeleteItem>) -> crate::queue::QueueFuture {
                unreachable!()
            }
        }
        for (conversations, requests) in [(100u8, vec![100]), (101, vec![100, 1])] {
            let all: Vec<ConversationId> = (0..conversations).map(conv).collect();
            let actions = plan(Want::Queues, &all, &[], T).actions;
            assert_eq!(actions.len(), usize::from(conversations));
            let items = actions
                .into_iter()
                .map(|action| {
                    let QueueAction::Create { conversation } = action else {
                        panic!("{action:?}");
                    };
                    let queue = new_queue(conversation).unwrap();
                    let item =
                        create_item(&queue.id, &queue.recipient_secret, &queue.sender_secret, T);
                    (queue.id, item)
                })
                .collect();
            let server = Counting(std::sync::Mutex::new(Vec::new()));
            let (answers, failure) = run_batches(batches(items), |items| server.create(items)).await;
            assert_eq!(failure, None);
            assert_eq!(answers.len(), usize::from(conversations));
            assert_eq!(*server.0.lock().unwrap(), requests);
        }
    }

    #[test]
    fn create_and_delete_items_verify_under_the_recipient_key() {
        let (id, recipient, sender) = ([4; 32], [5; 32], [6; 32]);
        let item = create_item(&id, &recipient, &sender, T);
        let sender_pub = public_key(&sender);
        let bytes = signing_bytes(
            &Signed::Create {
                sender_pub: &sender_pub,
            },
            &id,
            T,
        );
        let sig: [u8; 64] = BASE64URL_NOPAD
            .decode(item.sig.as_bytes())
            .unwrap()
            .try_into()
            .unwrap();
        assert!(queue_wire::verify(&public_key(&recipient), &bytes, &sig));
        assert_eq!(item.sender_pub, b64(&sender_pub));
        let item = delete_item(&id, &recipient, T);
        let sig: [u8; 64] = BASE64URL_NOPAD
            .decode(item.sig.as_bytes())
            .unwrap()
            .try_into()
            .unwrap();
        assert!(queue_wire::verify(
            &public_key(&recipient),
            &signing_bytes(&Signed::Delete, &id, T),
            &sig
        ));
    }
}
