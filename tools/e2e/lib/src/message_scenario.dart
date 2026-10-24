import 'dart:async';
import 'dart:convert';
import 'package:crypto/crypto.dart';

import 'account_scenario.dart' show AccountRestart;
import 'controller.dart';
import 'protocol.dart';

/// Native participants own their existing protected account and message store.
Future<void> runMessageScenario(
  Controller controller,
  List<Participant> apps, {
  required AccountRestart restart,
  int Function(List<String>)? auditRecovery,
  void Function(String, Map<String, Object?>)? evidence,
  Duration timeout = const Duration(seconds: 45),
}) async {
  if (apps.length != 2) throw ArgumentError('Two app participants required');
  var a = apps[0];
  var b = apps[1];
  Map<String, Object?> object(Object? value) =>
      (value as Map).cast<String, Object?>();
  bool same(Object? left, Object? right) =>
      jsonEncode(left) == jsonEncode(right);
  void require(bool ok, String code) {
    if (!ok) throw StateError('Message scenario: $code');
  }

  Future<Object?> call(
    Participant p,
    String op, [
    Map<String, Object?> args = const {},
  ]) => p.call(op, args, timeout: timeout);
  Future<Map<String, Object?>> snapshot(Participant p) async =>
      object(await call(p, 'account_local_snapshot'));
  Future<Map<String, Object?>> until(
    Future<Map<String, Object?>> Function() read,
    bool Function(Map<String, Object?>) matches,
    String phase,
  ) async {
    final deadline = DateTime.now().add(timeout);
    while (DateTime.now().isBefore(deadline)) {
      try {
        final view = await read();
        if (matches(view)) return view;
      } on RpcError catch (error) {
        if (error.code != 'NotAvailable') rethrow;
      }
      await Future<void>.delayed(const Duration(milliseconds: 100));
    }
    throw TimeoutException('Message scenario deadline: $phase');
  }

  Future<Map<String, Object?>> create(Participant p) async {
    await until(
      () => snapshot(p),
      (v) => v['phase'] == 'Unconfigured',
      'initial_account',
    );
    final display = object(await call(p, 'account_local_begin_create'));
    final words = (display['words'] as List).cast<String>();
    final answers = (display['positions'] as List)
        .map((n) => words[(n as int) - 1])
        .toList();
    final confirm = <String, Object?>{
      'flow': display['flow'],
      'answers': answers,
    };
    try {
      await call(p, 'account_local_confirm_create', confirm);
    } finally {
      final patterns = <String>[];
      try {
        if (auditRecovery != null) {
          patterns.addAll([
            words.join(' '),
            jsonEncode(display),
            jsonEncode(words),
            jsonEncode(answers),
            jsonEncode(confirm),
          ]);
          final files = auditRecovery(patterns);
          evidence?.call('recovery_privacy_pass', {'files': files});
        }
      } finally {
        patterns.clear();
        confirm.clear();
        answers.clear();
        words.clear();
        display.clear();
      }
    }
    await call(p, 'account_local_refresh_record', {
      'peer': (await snapshot(p))['public_peer_id'],
    });
    return until(() => snapshot(p), (v) => v['phase'] == 'Active', 'created');
  }

  Future<void> contact(Participant owner, Participant recipient) async {
    Object? invite = await call(owner, 'account_local_create_invite', {
      'name': null,
    });
    try {
      await call(recipient, 'account_local_accept_invite', {
        'input': invite,
        'alias': 'private fixture',
      });
    } finally {
      invite = null;
    }
  }

  Future<void> routes() async {
    for (final pair in [(a, b), (b, a)]) {
      final network = await until(
        () async => object(await call(pair.$2, 'network_info')),
        (v) => v['ticket'] is String,
        'route',
      );
      await call(pair.$1, 'account_local_route', {'ticket': network['ticket']});
    }
  }

  Future<Map<String, Object?>> page(
    Participant p,
    Object? conversation,
  ) async => object(
    await call(p, 'message_local_load_messages', {
      'conversation': conversation,
      'before': null,
      'limit': 50,
    }),
  );
  Future<Map<String, Object?>> state(
    Participant p,
    Map<String, Object?> key,
    Set<String> states,
  ) async {
    final result = await until(
      () => page(p, key['conversation']),
      (v) => (v['items'] as List).any(
        (item) =>
            same((item as Map)['key'], key) && states.contains(item['state']),
      ),
      'message_state',
    );
    return object(
      (result['items'] as List).singleWhere(
        (item) => same((item as Map)['key'], key),
      ),
    );
  }

  var av = await create(a), bv = await create(b);
  var aPeer = av['public_peer_id'], bPeer = bv['public_peer_id'];
  require(!same(aPeer, bPeer), 'distinct_accounts');
  await contact(b, a);
  await routes();
  await call(a, 'account_local_connect_account', {'peer': bPeer});
  final blocked = object(
    await call(a, 'message_local_send_text', {
      'peer': bPeer,
      'id': List.filled(16, 1),
      'body': 'M4_PRIVATE_CONTACT_BODY_SENTINEL',
    }),
  );
  final blockedKey = object(blocked['key']);
  await state(a, blockedKey, {'Blocked'});
  await contact(a, b);
  await call(a, 'message_local_retry_send', {'key': blockedKey});
  await state(a, blockedKey, {'Delivered'});
  final contactRows =
      (await page(b, blockedKey['conversation']))['items'] as List;
  require(
    contactRows
            .where((item) => same((item as Map)['key'], blockedKey))
            .length ==
        1,
    'contact_retry_one_receiver',
  );
  evidence?.call('contact_block_then_explicit_retry', {
    'key': blockedKey,
    'receiver_rows': contactRows.length,
  });

  // Separate the completed contact case from the ACK-loss case. Old stores
  // remain retained; only the later sender restarts prove same-store persistence.
  final contactA = a, contactB = b;
  final contactAPeer = aPeer, contactBPeer = bPeer;
  a = await restart(a, freshStore: true);
  b = await restart(b, freshStore: true);
  require(
    a.instance != contactA.instance && b.instance != contactB.instance,
    'fresh_store_instances',
  );
  av = await create(a);
  bv = await create(b);
  aPeer = av['public_peer_id'];
  bPeer = bv['public_peer_id'];
  require(
    !same(aPeer, contactAPeer) &&
        !same(bPeer, contactBPeer) &&
        !same(aPeer, bPeer),
    'fresh_store_identities',
  );
  await contact(b, a);
  await contact(a, b);
  final freshConversation = object(
    await call(a, 'message_local_open_dm', {'peer': bPeer}),
  )['id'];
  require(
    ((await page(a, freshConversation))['items'] as List).isEmpty &&
        ((await page(b, freshConversation))['items'] as List).isEmpty,
    'fresh_store_no_messages',
  );
  evidence?.call('fresh_owned_stores_for_lost_ack', {
    'sender_instance': a.instance,
    'receiver_instance': b.instance,
    'receiver_rows': 0,
  });

  Future<void> wakeSenderBackoff() async {
    // Cover the existing scheduler's maximum retained retry delay. This moves
    // time only; the normal scheduler and current proof still authorize work.
    await call(a, 'clock_advance', {'seconds': 300});
    bool proved(Map<String, Object?> view) => (view['contacts'] as List).any(
      (contact) => (contact as Map)['device_unverified'] == false,
    );
    if (!proved(await snapshot(a)) || !proved(await snapshot(b))) {
      await call(a, 'account_local_connect_account', {'peer': bPeer});
    }
    for (final participant in [a, b]) {
      await until(() => snapshot(participant), proved, 'proof_after_clock');
    }
  }

  await call(b, 'set_offline', {'offline': true});
  await call(a, 'set_offline', {'offline': true});
  const retainedBody = 'M4_PRIVATE_RESTART_BODY_SENTINEL';
  final sent = object(
    await call(a, 'message_local_send_text', {
      'peer': bPeer,
      'id': List.filled(16, 2),
      'body': retainedBody,
    }),
  );
  final key = object(sent['key']);
  require(sent['state'] == 'Queued', 'offline_enqueue');
  final bodyHash = sha256.convert(utf8.encode(retainedBody)).toString();
  final ackBaseline = object(await call(b, 'message_local_test_write_counts'));
  await call(b, 'message_local_test_ack_loss_arm', {'key': key});
  final old = a;
  a = await restart(a);
  require(a.instance != old.instance, 'new_core_instance');
  final reopened = await until(
    () => snapshot(a),
    (v) => v['public_peer_id'] != null,
    'restart_account',
  );
  require(
    same(reopened['public_peer_id'], aPeer) &&
        same(reopened['installation'], av['installation']),
    'same_store_identity',
  );
  final retained = await state(a, key, {'Queued', 'Blocked', 'SentDirect'});
  require(
    retained['body'] == retainedBody && retained['lamport'] == sent['lamport'],
    'retained_message',
  );
  evidence?.call('restart_same_store_keeps_queued_message', {
    'key': key,
    'lamport': retained['lamport'],
    'instance': a.instance,
    'body_sha256': bodyHash,
  });
  await call(b, 'set_offline', {'offline': false});
  await call(a, 'set_offline', {'offline': false});
  await call(a, 'network_start');
  await routes();
  await call(a, 'account_local_connect_account', {'peer': bPeer});
  await wakeSenderBackoff();
  String? lastProgress;
  await until(
    () async {
      final receiver = await page(b, key['conversation']);
      final sender = (await page(a, key['conversation']))['items'] as List;
      final writes = object(await call(a, 'message_local_test_write_counts'));
      final view = await snapshot(a);
      final progress = <String, Object?>{
        for (final state in ['Queued', 'Blocked', 'SentDirect', 'Failed'])
          'sender_$state': sender
              .where(
                (item) =>
                    same((item as Map)['key'], key) && item['state'] == state,
              )
              .length,
        'sender_clock_advance_seconds': 300,
        'payload_writes': writes['payload_writes'],
        'response_writes': writes['response_writes'],
        'ready_contacts': (view['contacts'] as List)
            .where((contact) => (contact as Map)['device_unverified'] == false)
            .length,
        'receiver_rows': (receiver['items'] as List)
            .where((item) => same((item as Map)['key'], key))
            .length,
      };
      final signature = jsonEncode(progress);
      if (signature != lastProgress) {
        lastProgress = signature;
        evidence?.call('receiver_commit_progress', progress);
      }
      return receiver;
    },
    (v) => (v['items'] as List).any((item) => same((item as Map)['key'], key)),
    'receiver_commit',
  );
  await call(b, 'clock_advance', {'seconds': 1});
  final checkpoints = object(await call(b, 'message_local_test_ack_loss_wait'));
  require(
    checkpoints['response_checkpoint'] == true &&
        checkpoints['receipt_checkpoint'] == true,
    'ack_checkpoints',
  );
  final ackHeld = object(await call(b, 'message_local_test_write_counts'));
  require(same(ackHeld, ackBaseline), 'ack_written_before_kill');
  await state(a, key, {'Queued', 'SentDirect'});
  final firstWrites = object(await call(a, 'message_local_test_write_counts'));
  require(firstWrites['payload_writes'] == 1, 'first_actual_payload');
  final beforeDuplicate = object(
    await call(b, 'message_local_open_dm', {'peer': aPeer}),
  );
  evidence?.call('lost_ack_before_owned_kill', {
    'key': key,
    'receiver_rows': 1,
    'payload_writes': firstWrites['payload_writes'],
    'ack_payload_delta': 0,
    'ack_response_delta': 0,
  });

  // Keep receiver time frozen: the held old receipt cannot outrun the retained
  // sender's normal retransmission after fresh proof. No manual attempt RPC.
  final lostInstance = a.instance;
  a = await restart(a);
  require(a.instance != lostInstance, 'lost_ack_new_instance');
  final afterLoss = await until(
    () => snapshot(a),
    (v) => v['public_peer_id'] != null,
    'lost_ack_restart',
  );
  require(
    same(afterLoss['public_peer_id'], aPeer) &&
        same(afterLoss['installation'], av['installation']),
    'lost_ack_same_store',
  );
  await call(a, 'network_start');
  await routes();
  await call(a, 'account_local_connect_account', {'peer': bPeer});
  await wakeSenderBackoff();
  await state(a, key, {'Delivered'});
  final secondWrites = object(await call(a, 'message_local_test_write_counts'));
  require(
    (secondWrites['payload_writes'] as int) >= 1,
    'actual_retransmission',
  );
  final afterDuplicate = object(
    await call(b, 'message_local_open_dm', {'peer': aPeer}),
  );
  require(
    afterDuplicate['unread'] == beforeDuplicate['unread'],
    'duplicate_unread_stable',
  );
  await call(b, 'message_local_test_ack_loss_release');
  evidence?.call('native_payload_retransmit_deduplicated', {
    'key': key,
    'first_instance': lostInstance,
    'second_instance': a.instance,
    'first_payload_writes': firstWrites['payload_writes'],
    'second_payload_writes': secondWrites['payload_writes'],
    'sender_clock_advance_seconds': 300,
    'unread': afterDuplicate['unread'],
    'body_sha256': bodyHash,
  });
  final duplicate = object(
    await call(a, 'message_local_send_text', {
      'peer': bPeer,
      'id': List.filled(16, 2),
      'body': retainedBody,
    }),
  );
  require(same(duplicate['key'], key), 'idempotent_send');
  final received = await page(b, key['conversation']);
  final matching = (received['items'] as List)
      .where((item) => same((item as Map)['key'], key))
      .toList();
  require(matching.length == 1, 'one_receiver_row');
  require(
    sha256
            .convert(utf8.encode((matching.single as Map)['body'] as String))
            .toString() ==
        bodyHash,
    'receiver_body_hash',
  );
  await call(b, 'message_local_mark_read', {
    'conversation': key['conversation'],
    'keys': [key],
  });
  await state(a, key, {'Read'});
  evidence?.call('durable_ack_and_read', {
    'key': key,
    'receiver_rows': matching.length,
    'revision': received['revision'],
    'body_sha256': bodyHash,
  });
}
