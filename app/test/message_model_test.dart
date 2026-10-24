import 'dart:async';
import 'dart:isolate';

import 'package:chat_app/account/account_model.dart';
import 'package:chat_app/core_client.dart';
import 'package:chat_app/message/message_bridge.dart';
import 'package:chat_app/message/message_model.dart';
import 'package:chat_app/network/network_model.dart';
import 'package:chat_app/src/rust/api/account.dart' as account;
import 'package:chat_app/src/rust/api/chat.dart' as rust;
import 'package:chat_app/src/rust/frb_generated.dart';
import 'package:flutter_test/flutter_test.dart';

import 'account_model_test.dart' as fixture;
import 'support/fake_message_client.dart';

Future<void> settle() async {
  for (var i = 0; i < 20; i++) {
    await Future<void>.delayed(Duration.zero);
  }
}

void main() {
  late fixture.FakeAccountClient accountClient;
  late AccountModel accounts;
  late FakeMessageClient client;
  late MessageModel model;

  setUp(() async {
    accountClient = fixture.FakeAccountClient();
    accounts = AccountModel(accountClient);
    await accounts.open();
    accounts.applySnapshot(
      fixture.view(BigInt.two, phase: account.AccountPhase.localReady),
    );
    client = FakeMessageClient();
    model = MessageModel(client, account: accounts);
  });
  tearDown(() async {
    model.dispose();
    accounts.dispose();
    await client.controller.close();
    await accountClient.controller.close();
  });

  Future<DurableMessageClient> durableLostReply() async {
    model.dispose();
    await client.controller.close();
    final durable = DurableMessageClient();
    client = durable;
    accounts.applySnapshot(
      account.AccountView(
        revision: BigInt.from(3),
        phase: account.AccountPhase.localReady,
        publicPeerId: bytes(2),
        installation: bytes(3),
        devices: [],
        contacts: [],
        remoteRecords: [],
        publishIntents: [],
        publication: accounts.snapshot!.publication,
        disclosurePossible: false,
        pendingPairTransactions: [],
        pairResumes: [],
        pendingImportRequiresRemoteRevoke: false,
      ),
    );
    model = MessageModel(client, account: accounts);
    await model.openConversation(bytes(4));
    expect(await model.send('fixture'), isFalse);
    await model.reload();
    expect(model.messages.single.state, DeliveryState.queued);
    expect(model.messages.single.key.id, durable.tokens.first);
    return durable;
  }

  test('cancelled_lost_reply_action_resend_creates_new_durable_row', () async {
    final durable = await durableLostReply();
    final observed = model.messages.single.key;
    final decoded = MessageKey(
      conversation: bytes(observed.conversation.first),
      sender: bytes(2),
      installation: bytes(3),
      id: observed.id.sublist(0),
    );
    expect(await model.cancel(decoded), isTrue);
    await settle();
    expect(model.messages.single.cancelRequested, isTrue);
    expect(durable.queued, isEmpty);
    expect(await model.send('fixture'), isTrue);
    await model.reload();
    expect(durable.tokens.last, hasLength(16));
    expect(durable.tokens.last, isNot(durable.tokens.first));
    expect(durable.rows, hasLength(2));
    expect(
      durable.rows.where((row) => row.state == DeliveryState.queued),
      hasLength(1),
    );
    expect(durable.queued, hasLength(1));
  });

  test(
    'successful_cancel_retires_action_before_held_page_and_owns_requested_key',
    () async {
      final durable = await durableLostReply();
      final observed = model.messages.single.key;
      final requested = MessageKey(
        conversation: observed.conversation.sublist(0),
        sender: observed.sender.sublist(0),
        installation: observed.installation.sublist(0),
        id: observed.id.sublist(0),
      );
      final held = Completer<MessagePage>();
      final releaseCancel = Completer<void>();
      durable.query = (_) => held.future;
      durable.beforeCancel = releaseCancel.future;
      final cancelling = model.cancel(requested);
      requested.id[0] ^= 1;
      releaseCancel.complete();
      expect(await cancelling, isTrue);
      await settle();
      expect(model.messages.single.cancelRequested, isFalse); // page still held
      expect(durable.rows.single.cancelRequested, isTrue);
      expect(await model.send('fixture'), isTrue);
      durable.query = null;
      held.complete(
        MessagePage(revision: durable.revision, items: List.of(durable.rows)),
      );
      await model.reload();
      expect(durable.tokens.last, isNot(durable.tokens.first));
      expect(durable.rows, hasLength(2));
      expect(durable.queued, hasLength(1));
    },
  );

  test(
    'lost_reply_without_cancellation_retries_one_durable_obligation',
    () async {
      final durable = await durableLostReply();
      expect(await model.send('fixture'), isTrue);
      expect(durable.tokens.last, durable.tokens.first);
      expect(durable.rows, hasLength(1));
      expect(durable.queued, hasLength(1));
    },
  );

  test('failed_cancel_does_not_retire_lost_reply_action', () async {
    final durable = await durableLostReply();
    durable.cancelError = const rust.CoreError(
      code: rust.ErrorCode.storageFull,
      detail: 'fixture',
    );
    expect(await model.cancel(model.messages.single.key), isFalse);
    expect(await model.send('fixture'), isTrue);
    expect(durable.tokens.last, durable.tokens.first);
    expect(durable.rows, hasLength(1));
    expect(durable.queued, hasLength(1));
  });

  test(
    'unrelated_cancel_matches_every_identifier_before_retiring_action',
    () async {
      for (var field = 0; field < 4; field++) {
        final durable = await durableLostReply();
        final row = model.messages.single;
        final fields = [
          row.key.conversation.sublist(0),
          row.key.sender.sublist(0),
          row.key.installation.sublist(0),
          row.key.id.sublist(0),
        ];
        fields[field][0] ^= 1;
        final unrelated = MessageKey(
          conversation: fields[0],
          sender: fields[1],
          installation: fields[2],
          id: fields[3],
        );
        durable.rows.add(
          MessageView(
            key: unrelated,
            lamport: BigInt.two,
            sentAtMs: row.sentAtMs,
            body: row.body,
            outgoing: true,
            state: DeliveryState.queued,
            cancelRequested: false,
          ),
        );
        expect(await model.cancel(unrelated), isTrue);
        await settle();
        expect(await model.send('fixture'), isTrue);
        expect(
          durable.tokens.last,
          durable.tokens.first,
          reason: 'identifier field $field',
        );
        expect(durable.queued, hasLength(1));
      }
    },
  );

  test(
    'late_cancel_does_not_retire_newer_or_replacement_route_action',
    () async {
      for (final replaceRoute in [false, true]) {
        final durable = await durableLostReply();
        final oldKey = model.messages.single.key;
        final releaseCancel = Completer<void>();
        durable.beforeCancel = releaseCancel.future;
        final cancelling = model.cancel(oldKey);
        if (replaceRoute) await model.openConversation(bytes(4));
        durable.loseSendReply = true;
        expect(await model.send('new fixture'), isFalse);
        final currentToken = List.of(durable.tokens.last);
        releaseCancel.complete();
        expect(await cancelling, !replaceRoute);
        await model.reload();
        expect(await model.send('new fixture'), isTrue);
        expect(durable.tokens.last, currentToken);
        expect(durable.rows, hasLength(2));
        expect(durable.queued, hasLength(1));
      }
    },
  );

  test(
    'accepted_cancel_fact_retires_action_after_lost_cancel_reply_even_with_read',
    () async {
      final durable = await durableLostReply();
      durable.loseCancelReply = true;
      expect(await model.cancel(model.messages.single.key), isFalse);
      final cancelled = durable.rows.single;
      durable.rows[0] = MessageView(
        key: cancelled.key,
        lamport: cancelled.lamport,
        sentAtMs: cancelled.sentAtMs,
        body: cancelled.body,
        outgoing: true,
        state: DeliveryState.read,
        cancelRequested: true,
      );
      await model.reload();
      expect(model.messages.single.state, DeliveryState.read);
      expect(await model.send('fixture'), isTrue);
      expect(durable.tokens.last, isNot(durable.tokens.first));
      expect(durable.rows, hasLength(2));
      expect(durable.queued, hasLength(1));
    },
  );

  test(
    'late_model_uses_current_offline_established_account_snapshot',
    () async {
      await model.openConversation(bytes(2));
      expect(model.messages, hasLength(1));
      expect(await model.send('fixture'), isTrue);
      expect(client.tokens, hasLength(1));
      expect(accounts.snapshot!.phase, account.AccountPhase.localReady);
      for (final phase in [
        account.AccountPhase.publishPending,
        account.AccountPhase.conflict,
        account.AccountPhase.active,
      ]) {
        accounts.applySnapshot(
          fixture.view(accounts.snapshot!.revision + BigInt.one, phase: phase),
        );
        expect(await model.send('fixture'), isTrue);
      }
    },
  );

  test('page_event_race_converges_without_duplicates', () async {
    final old = Completer<MessagePage>();
    client.query = (_) => old.future;
    final opening = model.openConversation(bytes(2));
    await settle();
    expect(client.controller.hasListener, isTrue);
    client.rows = [message(2), message(1), message(1)];
    client.changed(9);
    client.query = null;
    old.complete(MessagePage(revision: BigInt.one, items: [message(1)]));
    await opening;
    await settle();
    expect(model.revision, BigInt.from(9));
    expect(model.messages.map((row) => row.key.id.first), [2, 1]);
    expect(model.conversations, hasLength(1));
    expect(client.maxQueries, 1);
    final count = client.messageQueries;
    client.changed(9);
    client.changed(8);
    await settle();
    expect(client.messageQueries, count);
  });

  test('double_tap_and_lost_reply_reuse_request_id', () async {
    await model.openConversation(bytes(2));
    final response = Completer<MessageView>();
    client.sending = (_, _) => response.future;
    final first = model.send('fixture');
    final second = model.send('fixture');
    expect(client.tokens, hasLength(1));
    expect(model.saving, isTrue);
    expect(model.messages, hasLength(1)); // no optimistic durable row
    response.completeError(StateError('untrusted native detail'));
    expect(await first, isFalse);
    expect(await second, isFalse);
    expect(model.error, isNot(contains('untrusted')));
    client.sending = null;
    expect(await model.send('fixture'), isTrue);
    expect(client.tokens, hasLength(2));
    expect(client.tokens[0], client.tokens[1]);
    expect(client.tokens.first, hasLength(16));
    expect(await model.send('fixture'), isTrue);
    expect(client.tokens[2], isNot(client.tokens[0]));
  });

  test('failed_save_keeps_composer', () async {
    await model.openConversation(bytes(2));
    client.sending = (_, _) async => throw const rust.CoreError(
      code: rust.ErrorCode.storageFull,
      detail: 'private native detail',
    );
    var composer = 'fixture';
    if (await model.send(composer)) composer = '';
    expect(composer, 'fixture');
    expect(model.messages, hasLength(1));
    expect(model.error, contains('입력'));
    expect(model.saving, isFalse);
  });

  test('old_query_after_lock_does_not_restore_text', () async {
    await model.openConversation(bytes(2));
    expect(model.messages, hasLength(1));
    final old = Completer<MessagePage>();
    client.query = (_) => old.future;
    final refreshing = model.reload();
    await settle();
    expect(client.activeQueries, 1);
    accounts.applySnapshot(
      fixture.view(BigInt.from(3), phase: account.AccountPhase.locked),
    );
    expect(model.messages, isEmpty);
    expect(model.conversations, isEmpty);
    await model.markVisibleRead([message(1, outgoing: false).key]);
    expect(client.reads, isEmpty);
    old.complete(MessagePage(revision: BigInt.from(20), items: [message(1)]));
    await refreshing;
    expect(model.messages, isEmpty);
    expect(await model.send('fixture'), isFalse);
  });

  test('burst_events_coalesce_one_refresh', () async {
    await model.openConversation(bytes(2));
    final held = Completer<MessagePage>();
    client.query = (_) => held.future;
    final refreshing = model.reload();
    await settle();
    final count = client.messageQueries;
    for (var i = 2; i <= 1000; i++) {
      client.changed(i);
    }
    expect(client.messageQueries, count);
    client.query = null;
    client.rows = [message(2, state: DeliveryState.read)];
    held.complete(MessagePage(revision: BigInt.one, items: [message(1)]));
    await refreshing;
    await settle();
    expect(client.messageQueries, count + 1);
    expect(client.maxQueries, 1);
    expect(model.messages.single.state, DeliveryState.read);
    expect(model.revision, BigInt.from(1000));
  });

  test('page50_three_page_window_refreshes_and_preserves_anchor', () async {
    client.rows = List.generate(200, (i) => message(i + 1));
    await model.openConversation(bytes(2));
    expect(model.messages, hasLength(50));
    model.scrollAnchor = model.messages.last.key;
    await model.loadOlder();
    await model.loadOlder();
    await model.loadOlder();
    expect(model.messages, hasLength(150));
    expect(client.limits.every((limit) => limit == 50), isTrue);
    client.rows.add(message(201));
    client.changed(2);
    await settle();
    expect(model.messages, hasLength(150));
    expect(model.messages.first.key.id.first, 201);
    expect(model.scrollAnchor!.id.first, 151);
    expect(client.maxQueries, 1);
  });

  test(
    'all_message_identifier_bytes_define_identity_bigints_stay_exact',
    () async {
      final large = BigInt.parse('9007199254740993');
      client.revision = large;
      client.rows = [
        message(1, lamport: large),
        message(1, lamport: large),
        message(1, sender: 9, lamport: large),
        message(1, installation: 9, lamport: large),
        message(2, lamport: large),
      ];
      await model.openConversation(bytes(2));
      expect(model.messages, hasLength(4));
      expect(model.revision, large);
      expect(model.messages.first.lamport, large);
      expect(model.messages.first.sentAtMs, large);
      client.revision = large + BigInt.one;
      client.controller.add(
        rust.ChatEvent.messageStateChanged(
          revision: large + BigInt.one,
          key: message(1).key,
          state: DeliveryState.delivered,
          cancelRequested: false,
        ),
      );
      await settle();
      // Events trigger an authoritative snapshot, never fabricate receipt states.
      expect(
        model.messages.every((row) => row.state == DeliveryState.queued),
        isTrue,
      );
    },
  );

  test('read_only_visible_incoming_keys_deduplicated_and_batched50', () async {
    client.rows = List.generate(120, (i) => message(i + 1, outgoing: false));
    await model.openConversation(bytes(2));
    await model.loadOlder();
    await model.loadOlder();
    await model.markVisibleRead([message(1, conversation: 9).key]);
    expect(client.reads, isEmpty);
    await model.markVisibleRead([
      ...model.messages.map((row) => row.key),
      message(120).key,
      message(1, conversation: 9).key,
    ]);
    expect(client.reads.map((batch) => batch.length), [50, 50, 20]);
    expect(client.reads.expand((batch) => batch).length, 120);
    await model.cancel(message(1).key);
    await model.retry(message(1).key);
    expect(client.cancellations, hasLength(1));
    expect(client.retries, hasLength(1));
  });

  test(
    'conversation_recency_change_resets_cursor_and_converges_bounded_list',
    () async {
      client.conversations = List.generate(
        200,
        (i) => conversation(i + 1, revision: i + 1),
      );
      await model.reload();
      await model.loadOlder();
      await model.loadOlder();
      await model.loadOlder();
      expect(model.conversations, hasLength(150));
      expect(model.conversations.first.id.first, 200);
      final held = Completer<ConversationPage>();
      client.listQuery = (before) async {
        if (before == null) {
          return ConversationPage(
            revision: BigInt.one,
            items: [conversation(200)],
            next: ConversationCursor(
              changeRevision: BigInt.from(200),
              id: bytes(200),
            ),
          );
        }
        return held.future;
      };
      final refreshing = model.reload();
      await settle();
      client.conversations.removeWhere((row) => row.id.first == 1);
      client.conversations.insert(0, conversation(1, revision: 201));
      client.changed(2);
      client.listQuery = null;
      held.complete(
        ConversationPage(
          revision: BigInt.two,
          items: [conversation(1, revision: 201)],
        ),
      );
      await refreshing;
      expect(model.conversations, hasLength(150));
      expect(model.conversations.first.id.first, 1);
      expect(
        model.conversations.map((row) => row.id.first).toSet(),
        hasLength(150),
      );
      expect(model.revision, BigInt.two);
      expect(client.maxQueries, 1);
    },
  );

  test(
    'moved_conversation_cursor_error_resnapshots_instead_of_stalling',
    () async {
      client.conversations = List.generate(
        60,
        (i) => conversation(i + 1, revision: i + 1),
      );
      await model.reload();
      var moved = false;
      client.listQuery = (before) async {
        if (before == null) {
          return ConversationPage(
            revision: moved ? BigInt.two : BigInt.one,
            items: [conversation(moved ? 1 : 60)],
            next: moved
                ? null
                : ConversationCursor(
                    changeRevision: BigInt.from(60),
                    id: bytes(60),
                  ),
          );
        }
        moved = true;
        throw const rust.CoreError(
          code: rust.ErrorCode.invalidMessage,
          detail: 'private cursor detail',
        );
      };
      await model.loadOlder();
      expect(model.revision, BigInt.two);
      expect(model.conversations.single.id.first, 1);
      expect(model.error, isNull);
    },
  );

  test(
    'late_failing_query_cannot_publish_error_into_replacement_account',
    () async {
      await model.openConversation(bytes(2));
      final held = Completer<MessagePage>();
      client.query = (_) => held.future;
      final refreshing = model.reload();
      await settle();
      accounts.applySnapshot(
        fixture.view(BigInt.from(3), phase: account.AccountPhase.locked),
      );
      accounts.applySnapshot(
        fixture.view(BigInt.from(4), phase: account.AccountPhase.localReady),
      );
      held.completeError(
        const rust.CoreError(
          code: rust.ErrorCode.storageFull,
          detail: 'old account detail',
        ),
      );
      await refreshing;
      expect(model.error, isNull);
      expect(model.messages, isEmpty);
    },
  );

  test(
    'failed_old_route_query_does_not_strand_new_conversation_refresh',
    () async {
      await model.openConversation(bytes(2));
      final held = Completer<MessagePage>();
      client.query = (_) => held.future;
      final refreshing = model.reload();
      await settle();
      client.rows = [message(7, conversation: 2)];
      client.conversations.add(conversation(2));
      client.query = null;
      final opening = model.openConversation(bytes(3));
      await settle();
      held.completeError(StateError('stale route failure'));
      await Future.wait([refreshing, opening]);
      expect(model.currentConversation!.id.first, 2);
      expect(model.messages.single.key.conversation.first, 2);
      expect(model.messages.single.key.id.first, 7);
      expect(model.error, isNull);
    },
  );

  test(
    'reload_notifies_busy_before_snapshot_and_clears_it_after_settle',
    () async {
      await model.openConversation(bytes(2));
      final held = Completer<MessagePage>();
      client.query = (_) => held.future;
      final observed = <bool>[];
      model.addListener(() => observed.add(model.busy));
      final refreshing = model.reload();
      expect(observed, contains(true));
      held.complete(MessagePage(revision: BigInt.one, items: [message(1)]));
      await refreshing;
      expect(observed.last, isFalse);
    },
  );

  test('late_old_command_does_not_strand_busy_on_new_conversation', () async {
    await model.openConversation(bytes(2));
    final held = Completer<MessageView>();
    client.cancelling = (_) => held.future;
    final cancelling = model.cancel(message(1).key);
    client.conversations.add(conversation(2));
    client.rows = [message(2, conversation: 2)];
    await model.openConversation(bytes(3));
    expect(await model.retry(message(2, conversation: 2).key), isTrue);
    held.complete(message(1, state: DeliveryState.cancelled));
    expect(await cancelling, isFalse);
    await settle();
    expect(model.busy, isFalse);
  });

  test('synchronous_query_event_cannot_start_a_second_refresh', () async {
    var emitted = false;
    client.listQuery = (_) async {
      if (!emitted) {
        emitted = true;
        client.changed(2);
      }
      return ConversationPage(revision: BigInt.two, items: [conversation(1)]);
    };
    await model.reload();
    expect(client.maxQueries, 1);
    expect(model.revision, BigInt.two);
  });

  test('synchronous_send_event_and_reentrant_tap_share_one_save', () async {
    await model.openConversation(bytes(2));
    final response = Completer<MessageView>();
    var reentered = false;
    Future<bool>? duplicate;
    model.addListener(() {
      if (model.saving && !reentered) {
        reentered = true;
        duplicate = model.send('fixture');
      }
    });
    client.sending = (_, _) {
      client.changed(2);
      return response.future;
    };
    final sending = model.send('fixture');
    expect(client.tokens, hasLength(1));
    response.complete(message(1));
    expect(await sending, isTrue);
    expect(await duplicate, isTrue);
  });

  test(
    'unestablished_accounts_and_auth_diagnostics_never_grant_readiness',
    () async {
      for (final phase in [
        account.AccountPhase.candidate,
        account.AccountPhase.pendingImport,
        account.AccountPhase.unconfigured,
        account.AccountPhase.locked,
      ]) {
        accounts.applySnapshot(
          fixture.view(accounts.snapshot!.revision + BigInt.one, phase: phase),
        );
        client.controller.add(
          const rust.ChatEvent.accountAuthChanged(
            peerId: 'public-peer',
            installationId: 'public-installation',
            ready: true,
          ),
        );
        await model.openConversation(bytes(2));
        await model.reload();
        expect(await model.send('fixture'), isFalse);
        expect(model.ready, isFalse);
        expect(model.messages, isEmpty);
      }
      expect(client.messageQueries, 0);
      expect(client.listQueries, 0);
    },
  );

  test('delayed_command_reply_cannot_regress_fresher_read_snapshot', () async {
    await model.openConversation(bytes(2));
    final held = Completer<MessageView>();
    client.sending = (_, _) => held.future;
    final sending = model.send('fixture');
    client.rows = [message(1, state: DeliveryState.read)];
    client.changed(2);
    await settle();
    expect(model.messages.single.state, DeliveryState.read);
    held.complete(message(1, state: DeliveryState.queued));
    expect(await sending, isTrue);
    expect(model.messages.single.state, DeliveryState.read);
    await settle();
    expect(model.messages.single.state, DeliveryState.read);
  });

  test('checked_Frb_adapter_and_models_share_one_Rust_subscription', () async {
    final result = await Isolate.run(_checkedAdapterAndSharedBus);
    expect(result['events'], 1);
    expect(result['listens'], 1);
    expect(result['operations'], 7);
    expect(result['rejected'], 7);
    expect(result['messageCount'], 1);
    expect(result['saved'], isTrue);
    expect(result['accountLockedAfterMessageDispose'], isTrue);
    expect(result['networkStillStopped'], isTrue);
  });

  test(
    'old_open_and_query_after_identity_replacement_or_dispose_are_ignored',
    () async {
      await model.openConversation(bytes(2));
      expect(model.messages, hasLength(1));
      final delayed = Completer<ConversationView>();
      client.opening = (_) => delayed.future;
      final opening = model.openConversation(bytes(2));
      accounts.applySnapshot(
        account.AccountView(
          revision: BigInt.from(3),
          phase: account.AccountPhase.active,
          publicPeerId: bytes(99),
          installation: bytes(98),
          devices: [],
          contacts: [],
          remoteRecords: [],
          publishIntents: [],
          publication: accounts.snapshot!.publication,
          disclosurePossible: false,
          pendingPairTransactions: [],
          pairResumes: [],
          pendingImportRequiresRemoteRevoke: false,
        ),
      );
      delayed.complete(conversation(1));
      await opening;
      expect(model.messages, isEmpty);
      expect(model.currentConversation, isNull);
      model.dispose();
      model = MessageModel(client, account: accounts);
      final globalListener = client.events().listen((_) {});
      model.dispose();
      client.changed(8);
      expect(client.controller.hasListener, isTrue);
      await globalListener.cancel();
      // Replace the already disposed fixture before tearDown.
      model = MessageModel(client, account: accounts);
    },
  );
}

class _SharedApi implements RustLibApi {
  _SharedApi() {
    bus = StreamController<rust.ChatEvent>(onListen: () => listens++);
  }
  late final StreamController<rust.ChatEvent> bus;
  int events = 0;
  int listens = 0;
  final operations = <Symbol>{};
  @override
  dynamic noSuchMethod(Invocation invocation) {
    final method = invocation.memberName;
    if (method.toString().contains('crateApiMessage')) operations.add(method);
    return switch (method) {
      #crateApiChatEvents => () {
        events++;
        return bus.stream;
      }(),
      #crateApiAccountOpenAccount => Future<void>.value(),
      #crateApiAccountSnapshot => Future.value(
        fixture.view(BigInt.one, phase: account.AccountPhase.localReady),
      ),
      #crateApiChatNetworkInfo => Future.value(
        rust.NetworkInfo(
          revision: BigInt.zero,
          state: rust.NetworkState.stopped,
          peers: [],
        ),
      ),
      #crateApiMessageOpenDm => Future.value(conversation(1)),
      #crateApiMessageListConversations => Future.value(
        ConversationPage(revision: BigInt.one, items: [conversation(1)]),
      ),
      #crateApiMessageLoadMessages => Future.value(
        MessagePage(revision: BigInt.one, items: [message(1, outgoing: false)]),
      ),
      #crateApiMessageSendText ||
      #crateApiMessageCancelSend ||
      #crateApiMessageRetrySend => Future.value(message(1)),
      #crateApiMessageMarkRead => Future<void>.value(),
      _ => super.noSuchMethod(invocation),
    };
  }
}

// FRB singleton mock initialization is isolated from other test/native sessions.
Future<Map<String, Object>> _checkedAdapterAndSharedBus() async {
  final api = _SharedApi();
  RustLib.initMock(api: api);
  final core = FrbCoreClient();
  final accounts = AccountModel(core.account);
  await accounts.open();
  final network = NetworkModel(core);
  await network.open();
  final model = MessageModel(core.messages, account: accounts);
  await model.openConversation(bytes(2));
  final messageCount = model.messages.length;
  final saved = await model.send('fixture');
  await model.cancel(message(1).key);
  await model.retry(message(1).key);
  await model.markVisibleRead([message(1).key]);
  final bad = rust.MessageKey(
    conversation: bytes(1, 31),
    sender: bytes(2),
    installation: bytes(3),
    id: bytes(1, 16),
  );
  var rejected = 0;
  for (final operation in <Future<Object?> Function()>[
    () => core.messages.openDm(peer: List.filled(32, 256)),
    () => core.messages.listConversations(limit: 4294967297),
    () => core.messages.loadMessages(conversation: bytes(1), limit: -1),
    () => core.messages.sendText(
      peer: bytes(2),
      id: List.filled(16, 256),
      body: 'fixture',
    ),
    () => core.messages.cancelSend(key: bad),
    () => core.messages.retrySend(key: bad),
    () => core.messages.markRead(conversation: bytes(1), keys: [bad]),
  ]) {
    try {
      await operation();
    } on rust.CoreError catch (failure) {
      if (failure.code == rust.ErrorCode.invalidMessage) rejected++;
    }
  }
  model.dispose();
  api.bus.add(
    rust.ChatEvent.accountChanged(
      revision: BigInt.two,
      view: fixture.view(BigInt.two, phase: account.AccountPhase.locked),
    ),
  );
  await settle();
  final result = <String, Object>{
    'events': api.events,
    'listens': api.listens,
    'operations': api.operations.length,
    'rejected': rejected,
    'messageCount': messageCount,
    'saved': saved,
    'accountLockedAfterMessageDispose':
        accounts.snapshot!.phase == account.AccountPhase.locked,
    'networkStillStopped': network.snapshot!.state == rust.NetworkState.stopped,
  };
  accounts.dispose();
  network.dispose();
  await api.bus.close();
  RustLib.dispose();
  return result;
}
