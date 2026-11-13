import 'dart:async';
import 'dart:convert';

import 'controller.dart';
import 'diagnostic.dart';
import 'protocol.dart';

/// Takes one device off the network (`true`) or puts it back (`false`), the
/// way the platform does (Android airplane mode).
typedef AirplaneMode = Future<void> Function(Participant, bool);

/// Brings the participant's app to the foreground.
typedef Foreground = Future<void> Function(Participant);

/// M6 ④: A = [phone (Android), mailbox (Windows app or a second Android in
/// mailbox mode)], B = [node]. While the phone is in airplane mode the node
/// sends three texts, which the mailbox takes. Out of airplane mode and in the
/// foreground, the phone gets them from the mailbox in their order, once each,
/// with one notification.
Future<void> runMailboxScenario(
  Controller controller,
  List<Participant> participants, {
  required AirplaneMode airplane,
  required Foreground foreground,
  void Function(String, Map<String, Object?>)? evidence,
  Duration timeout = const Duration(seconds: 60),
}) async {
  if (participants.length != 3) {
    throw ArgumentError('Phone, mailbox and node participants required');
  }
  final phone = participants[0];
  final mailbox = participants[1];
  final node = participants[2];
  Map<String, Object?> object(Object? value) =>
      (value as Map).cast<String, Object?>();
  bool same(Object? left, Object? right) =>
      jsonEncode(left) == jsonEncode(right);
  void require(bool ok, String code) {
    if (!ok) throw StateError('Mailbox scenario: $code');
  }

  // The participant of the latest call, named by a deadline.
  String? who;
  Future<Object?> call(
    Participant p,
    String op, [
    Map<String, Object?> args = const {},
  ]) {
    who = p.name;
    return p.call(op, args, timeout: timeout);
  }

  Future<Map<String, Object?>> snapshot(Participant p) async =>
      object(await call(p, 'account_local_snapshot'));
  Future<T> until<T>(
    Future<T> Function() read,
    bool Function(T) matches,
    String phase,
  ) async {
    final name = 'Mailbox scenario deadline: $phase';
    final took = Stopwatch()..start();
    final deadline = DateTime.now().add(timeout);
    String? last;
    while (DateTime.now().isBefore(deadline)) {
      try {
        final value = await read();
        last = observedView(value);
        if (matches(value)) {
          noteWait(name, took.elapsedMilliseconds);
          return value;
        }
      } on RpcError catch (error) {
        if (error.code != 'NotAvailable') rethrow;
        last = observedView(error);
      }
      await Future<void>.delayed(const Duration(milliseconds: 100));
    }
    noteWait(name, took.elapsedMilliseconds);
    throw ScenarioDeadline(name, lastView: last, participant: who);
  }

  Future<Map<String, Object?>> view(
    Participant p,
    bool Function(Map<String, Object?>) matches,
    String phase,
  ) => until(() => snapshot(p), matches, phase);

  Future<void> create(Participant p) async {
    await view(p, (v) => v['phase'] == 'Unconfigured', 'initial_account');
    final display = object(await call(p, 'account_local_begin_create'));
    final words = (display['words'] as List).cast<String>();
    final answers = (display['positions'] as List)
        .map((n) => words[(n as int) - 1])
        .toList();
    try {
      await call(p, 'account_local_confirm_create', {
        'flow': display['flow'],
        'answers': answers,
      });
    } finally {
      answers.clear();
      words.clear();
      display.clear();
    }
    await call(p, 'account_local_refresh_record', {
      'peer': (await snapshot(p))['public_peer_id'],
    });
    await view(p, (v) => v['phase'] == 'Active', 'created');
  }

  Future<Object?> ticket(Participant p) async => (await until(
    () async => object(await call(p, 'network_info')),
    (v) => v['ticket'] is String,
    'ticket',
  ))['ticket'];

  // The phone's existing account shows the code; the mailbox joins it (M3).
  Future<void> link() async {
    await view(mailbox, (v) => v['phase'] == 'Unconfigured', 'new_device');
    await call(mailbox, 'account_local_prepare_candidate');
    final display = object(
      await call(phone, 'account_local_pair_begin', {
        'direction': 'ExistingDisplays',
        'resume': null,
      }),
    );
    final flow = display['flow'];
    await call(mailbox, 'account_local_route', {'ticket': await ticket(phone)});
    final Object? scanned;
    try {
      scanned = await call(mailbox, 'account_local_pair_scan', {
        'qr': display['qr'],
      });
    } on RpcError {
      await notePairSide(
        'owner',
        () => snapshot(phone),
        () => call(phone, 'network_info'),
      );
      await notePairSide(
        'scanner',
        () => snapshot(mailbox),
        () => call(mailbox, 'network_info'),
      );
      rethrow;
    }
    display.clear();
    require(same(flow, scanned), 'pair_flow');
    bool coded(Map<String, Object?> v) =>
        v['pair'] is Map && (v['pair'] as Map)['sas'] != null;
    final pv = await view(phone, coded, 'pair_code');
    final mv = await view(mailbox, coded, 'pair_code');
    require(
      (pv['pair'] as Map)['sas'] == (mv['pair'] as Map)['sas'],
      'pair_sas',
    );
    for (final p in [phone, mailbox]) {
      await call(p, 'account_local_pair_confirm_code', {
        'flow': flow,
        'matches': true,
      });
    }
    await view(
      phone,
      (v) =>
          v['pair'] is Map && (v['pair'] as Map)['phase'] == 'AwaitingApproval',
      'pair_awaiting_approval',
    );
    await call(phone, 'account_local_pair_approve', {'flow': flow});
    final peer = pv['public_peer_id'];
    bool listed(Map<String, Object?> v) {
      final publication = v['publication'];
      return v['phase'] == 'Active' &&
          publication is Map &&
          publication['state'] == 'Observed' &&
          publication['accepted'] is Map &&
          ((publication['accepted'] as Map)['devices'] as List).any(
            (d) => same((d as Map)['installation_id'], mv['installation']),
          );
    }

    await view(phone, listed, 'pair_listed');
    await call(mailbox, 'account_local_refresh_record', {'peer': peer});
    final joined = await view(
      mailbox,
      (v) => v['phase'] == 'Active',
      'pair_joined',
    );
    require(same(joined['public_peer_id'], peer), 'pair_same_account');
  }

  Future<void> contact(Participant owner, Participant recipient) async {
    Object? invite = await call(owner, 'account_local_create_invite', {
      'name': null,
    });
    try {
      await call(recipient, 'account_local_accept_invite', {
        'input': invite,
        'alias': 'mailbox fixture',
      });
    } finally {
      invite = null;
    }
  }

  Future<void> routes(Iterable<Participant> to) async {
    final all = [phone, mailbox, node];
    for (final target in to) {
      final address = await ticket(target);
      for (final p in all) {
        if (p != target) {
          await call(p, 'account_local_route', {'ticket': address});
        }
      }
    }
  }

  /// `p`'s messages of the conversation in the order it shows them.
  Future<List<Map<String, Object?>>> messages(
    Participant p,
    Object? conversation,
  ) async {
    final page = object(
      await call(p, 'message_local_load_messages', {
        'conversation': conversation,
        'before': null,
        'limit': 50,
      }),
    );
    final items = (page['items'] as List).map(object).toList();
    int lamport(Map<String, Object?> m) => m['lamport'] as int;
    items.sort((x, y) => lamport(x).compareTo(lamport(y)));
    return items;
  }

  Future<Map<String, Object?>> counters(Participant p) async =>
      object(await call(p, 'mailbox_local_test_counters'));

  List<int> id(int n) => List.filled(16, 0x60 + n);
  // Texts from the node, as the phone and the mailbox store them.
  List<Object?> idsFrom(List<Map<String, Object?>> items, Object? sender) => [
    for (final m in items)
      if (same((m['key'] as Map)['sender'], sender)) (m['key'] as Map)['id'],
  ];

  await create(phone);
  await link();
  final a = await snapshot(phone);
  final aPeer = a['public_peer_id'];
  evidence?.call('linked', {
    'phone': a['installation'],
    'mailbox': (await snapshot(mailbox))['installation'],
  });
  await create(node);
  final bPeer = (await snapshot(node))['public_peer_id'];
  require(!same(aPeer, bPeer), 'distinct_accounts');
  await contact(node, phone);
  await contact(node, mailbox);
  await contact(phone, node);
  await routes([phone, mailbox, node]);
  for (final p in [phone, mailbox]) {
    await call(p, 'account_local_connect_account', {'peer': bPeer});
  }
  // One own device at a time (two dialing each other at once may both lose).
  for (final p in [phone, mailbox]) {
    await call(p, 'mailbox_local_sync_now');
  }
  final conversation = object(
    await call(node, 'message_local_open_dm', {'peer': aPeer}),
  )['id'];
  for (final p in [phone, mailbox]) {
    final own = object(
      await call(p, 'message_local_open_dm', {'peer': bPeer}),
    )['id'];
    require(same(own, conversation), 'one_conversation');
  }

  // On a delivery deadline: each side's group for the conversation, as fixed
  // buckets (the node's with its latest text's receivers).
  Object? lastKey;
  Future<T> grouped<T>(Future<T> Function() wait) async {
    try {
      return await wait();
    } on ScenarioDeadline {
      for (final (side, p) in [
        ('node', node),
        ('phone', phone),
        ('mailbox', mailbox),
      ]) {
        await noteGroup(
          side,
          () => p.call(
            'message_local_test_group_facts',
            {'conversation': conversation, 'key': p == node ? lastKey : null},
            timeout: const Duration(seconds: 5),
            failOnTimeout: false,
          ),
        );
      }
      rethrow;
    }
  }

  Future<void> sendFromNode(int n) async {
    final sent = object(
      await call(node, 'message_local_send_text', {
        'peer': aPeer,
        'id': id(n),
        'body': 'm6 text $n',
      }),
    );
    final key = sent['key'];
    lastKey = key;
    await grouped(
      () => until(
        () => messages(node, conversation),
        (items) => items.any(
          (m) =>
              same(m['key'], key) &&
              const {'Delivered', 'Read'}.contains(m['state']),
        ),
        'node_delivered_$n',
      ),
    );
  }

  // The first text makes the group with both of A's devices.
  await sendFromNode(1);
  for (final p in [phone, mailbox]) {
    await grouped(
      () => until(
        () => messages(p, conversation),
        (items) => idsFrom(items, bPeer).length == 1,
        'first_text',
      ),
    );
  }
  await call(mailbox, 'mailbox_local_set', {'enabled': true});
  final role = object(await call(mailbox, 'mailbox_local_state'));
  require(role['enabled'] == true, 'mailbox_enabled');
  evidence?.call('mailbox_on', {'enabled': role['enabled']});
  final before = await counters(phone);

  await airplane(phone, true);
  await call(phone, 'set_offline', {'offline': true});
  evidence?.call('phone_airplane', {'offline': true});
  for (var n = 2; n <= 4; n++) {
    await sendFromNode(n);
  }
  final kept = await grouped(
    () => until(
      () => messages(mailbox, conversation),
      (items) => idsFrom(items, bPeer).length == 4,
      'mailbox_received',
    ),
  );
  require(
    idsFrom(await messages(phone, conversation), bPeer).length == 1,
    'phone_isolated',
  );
  evidence?.call('mailbox_received', {'texts': idsFrom(kept, bPeer).length});

  final returned = DateTime.now();
  await airplane(phone, false);
  await call(phone, 'set_offline', {'offline': false});
  // The phone's addresses changed: the others learn them again.
  await routes([phone]);
  await call(phone, 'account_local_route', {'ticket': await ticket(mailbox)});
  await foreground(phone);
  await call(phone, 'mailbox_local_sync_now');
  final got = await grouped(
    () => until(
      () => messages(phone, conversation),
      (items) => idsFrom(items, bPeer).length >= 4,
      'phone_received',
    ),
  );
  final elapsed = DateTime.now().difference(returned);
  final expected = [for (var n = 1; n <= 4; n++) id(n)];
  require(same(idsFrom(got, bPeer), expected), 'phone_order_once');
  require(
    same(
      idsFrom(got, bPeer),
      idsFrom(await messages(node, conversation), bPeer),
    ),
    'node_order',
  );
  // Nothing more arrives afterwards (no second copy, no second notification).
  await Future<void>.delayed(const Duration(seconds: 3));
  require(
    same(idsFrom(await messages(phone, conversation), bPeer), expected),
    'phone_no_duplicate',
  );
  final after = await counters(phone);
  final notifications =
      (after['notifications'] as int) - (before['notifications'] as int);
  final notified = (after['notified'] as int) - (before['notified'] as int);
  require(notifications == 1 && notified == 3, 'one_notification');
  require(after['missing'] == before['missing'], 'nothing_missing');
  evidence?.call('phone_received', {
    'texts': 3,
    'notifications': notifications,
    'notified': notified,
    'return_to_stored_ms': elapsed.inMilliseconds,
  });
}
