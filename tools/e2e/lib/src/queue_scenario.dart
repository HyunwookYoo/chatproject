import 'dart:async';
import 'dart:convert';

import 'controller.dart';
import 'diagnostic.dart';
import 'protocol.dart';

/// One read of the local queue server's file (`queue_server --harness-counts`):
/// how many queues it holds and the entries each holds, most first. Counts
/// only: no queue id, key or ciphertext.
class QueueServerCounts {
  QueueServerCounts(this.queues, this.entries);
  final int queues;
  final List<int> entries;

  int get held => entries.fold(0, (sum, n) => sum + n);

  /// The line the server prints, or a [FormatException]: exactly
  /// `{"queues":N,"entries":[...]}` with N entries, each a count, most first.
  static QueueServerCounts parse(String line) {
    final value = jsonDecode(line.trim());
    if (value is! Map ||
        value.length != 2 ||
        value['queues'] is! int ||
        value['entries'] is! List) {
      throw const FormatException('queue counts');
    }
    final entries = (value['entries'] as List).whereType<int>().toList();
    final queues = value['queues'] as int;
    final sorted = [...entries]..sort((a, b) => b.compareTo(a));
    if (entries.length != (value['entries'] as List).length ||
        entries.length != queues ||
        entries.any((n) => n < 0) ||
        jsonEncode(sorted) != jsonEncode(entries)) {
      throw const FormatException('queue counts');
    }
    return QueueServerCounts(queues, entries);
  }

  Map<String, Object?> toJson() => {'queues': queues, 'entries': entries};
}

/// What the m8 run noted, counts only (diagnostic `queue`): the queue server's
/// last read, B1's `SEND` requests, A1's fetched entries and taken `ACK`
/// items, and how long A1 took to show the first text after its start.
class QueueStats {
  int? serverQueues;
  int? serverEntries;
  int? sendsMessage;
  int? sendsStanding;
  int? fetched;
  int? acked;
  int? firstTextAfterStartMs;
  var _used = false;

  void clear() {
    serverQueues = serverEntries = sendsMessage = sendsStanding = null;
    fetched = acked = firstTextAfterStartMs = null;
    _used = false;
  }

  void touch() => _used = true;

  /// The fixed `queue` diagnostic, null when the run did no queue work.
  Map<String, Object?>? toSafeJson() => !_used
      ? null
      : {
          'server_queues': serverQueues,
          'server_entries': serverEntries,
          'sends_message': sendsMessage,
          'sends_standing': sendsStanding,
          'fetched': fetched,
          'acked': acked,
          'first_text_after_start_ms': firstTextAfterStartMs,
        };
}

final queueStats = QueueStats();

/// M8 ④ (spec 10): A = [A1 the Android app, A2 a node, A's other device],
/// B = [B1 a node]; none is a mailbox, so each device makes a queue at the
/// local queue server ([server] reads its file).
///
/// 1. A1 and A2 judge "no mailbox" and make queues; B1 keeps both `QueueRef`s.
/// 2. A1 and A2 are taken off line (`Stopped`); B1's two texts go into their
///    queues: two entries in each. B1 shows them not delivered.
/// 3. B1 is taken off line too, so the queue is the only way to A1. A1 back on
///    line takes both texts once, with one alert. B1 back on line shows both
///    delivered; then only A2's queue holds anything.
/// 4. A2 back on line as a mailbox with a qualified week (the uptime hook):
///    A1 judges "has mailbox", stops using its queue (`Retiring`, still at the
///    server), and B1 holds A1's withdrawal. The 48 h `DELETE` is not run here:
///    the server checks each request's signed time within ±10 min of its own
///    clock, so a `clock_advance`d device's `DELETE` is refused; the core test
///    `queue_lifecycle` `mailbox_recovery_withdraws_and_deletes_after_48h`
///    runs it against the real router (plan §6).
Future<void> runQueueScenario(
  Controller controller,
  List<Participant> participants, {
  required Future<QueueServerCounts> Function() server,
  void Function(String, Map<String, Object?>)? evidence,
  Duration timeout = const Duration(seconds: 60),
  Duration slow = const Duration(minutes: 6),
}) async {
  if (participants.length != 3) {
    throw ArgumentError('Phone, other device and node participants required');
  }
  final a1 = participants[0];
  final a2 = participants[1];
  final b1 = participants[2];
  Map<String, Object?> object(Object? value) =>
      (value as Map).cast<String, Object?>();
  bool same(Object? left, Object? right) =>
      jsonEncode(left) == jsonEncode(right);
  void require(bool ok, String code) {
    if (!ok) throw StateError('Queue scenario: $code');
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

  Future<QueueServerCounts> held() async {
    final read = await server();
    queueStats
      ..touch()
      ..serverQueues = read.queues
      ..serverEntries = read.held;
    return read;
  }

  // On a deadline: each side's network state and what the server holds now, as fixed buckets.
  Future<void> noteFacts() async {
    for (final (side, p) in [('phone', a1), ('other', a2), ('node', b1)]) {
      Object? state;
      try {
        final info = await p.call(
          'network_info',
          const {},
          timeout: const Duration(seconds: 5),
          failOnTimeout: false,
        );
        state = info is Map ? info['state'] : null;
      } catch (_) {
        state = null;
      }
      failureFacts['m8_network_$side'] =
          const {'Stopped', 'Starting', 'Online', 'Failed'}.contains(state)
          ? state as String
          : 'unknown';
    }
    // A fresh read; the last one, if any, is from before the step that timed out.
    try {
      await held().timeout(const Duration(seconds: 10));
    } catch (_) {}
    failureFacts['m8_server_queues'] = factBucket(
      queueStats.serverQueues,
      'q',
      3,
    );
    failureFacts['m8_server_entries'] = factBucket(
      queueStats.serverEntries,
      'e',
      4,
    );
  }

  Future<Map<String, Object?>> snapshot(Participant p) async =>
      object(await call(p, 'account_local_snapshot'));
  Future<T> until<T>(
    Future<T> Function() read,
    bool Function(T) matches,
    String phase, {
    Duration? within,
  }) async {
    final name = 'Queue scenario deadline: $phase';
    final took = Stopwatch()..start();
    final deadline = DateTime.now().add(within ?? timeout);
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
      await Future<void>.delayed(const Duration(milliseconds: 250));
    }
    noteWait(name, took.elapsedMilliseconds);
    await noteFacts();
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

  Future<Map<String, Object?>> network(Participant p) async =>
      object(await call(p, 'network_info'));
  Future<Object?> ticket(Participant p) async => (await until(
    () => network(p),
    (v) => v['ticket'] is String,
    'ticket',
  ))['ticket'];

  // A1's account shows the code; A2 joins it (M3).
  Future<void> link() async {
    await view(a2, (v) => v['phase'] == 'Unconfigured', 'new_device');
    await call(a2, 'account_local_prepare_candidate');
    final display = object(
      await call(a1, 'account_local_pair_begin', {
        'direction': 'ExistingDisplays',
        'resume': null,
      }),
    );
    final flow = display['flow'];
    await call(a2, 'account_local_route', {'ticket': await ticket(a1)});
    final scanned = await call(a2, 'account_local_pair_scan', {
      'qr': display['qr'],
    });
    display.clear();
    require(same(flow, scanned), 'pair_flow');
    bool coded(Map<String, Object?> v) =>
        v['pair'] is Map && (v['pair'] as Map)['sas'] != null;
    final pv = await view(a1, coded, 'pair_code');
    final mv = await view(a2, coded, 'pair_code');
    require(
      (pv['pair'] as Map)['sas'] == (mv['pair'] as Map)['sas'],
      'pair_sas',
    );
    for (final p in [a1, a2]) {
      await call(p, 'account_local_pair_confirm_code', {
        'flow': flow,
        'matches': true,
      });
    }
    await view(
      a1,
      (v) =>
          v['pair'] is Map && (v['pair'] as Map)['phase'] == 'AwaitingApproval',
      'pair_awaiting_approval',
    );
    await call(a1, 'account_local_pair_approve', {'flow': flow});
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

    await view(a1, listed, 'pair_listed');
    await call(a2, 'account_local_refresh_record', {'peer': peer});
    final joined = await view(a2, (v) => v['phase'] == 'Active', 'pair_joined');
    require(same(joined['public_peer_id'], peer), 'pair_same_account');
  }

  Future<void> contact(Participant owner, Participant recipient) async {
    Object? invite = await call(owner, 'account_local_create_invite', {
      'name': null,
    });
    try {
      await call(recipient, 'account_local_accept_invite', {
        'input': invite,
        'alias': 'queue fixture',
      });
    } finally {
      invite = null;
    }
  }

  // Every other participant learns [to]'s current addresses.
  Future<void> routes(Iterable<Participant> to) async {
    for (final target in to) {
      final address = await ticket(target);
      for (final p in [a1, a2, b1]) {
        if (p != target) {
          await call(p, 'account_local_route', {'ticket': address});
        }
      }
    }
  }

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

  List<int> id(int n) => List.filled(16, 0x80 + n);
  List<Object?> idsFrom(List<Map<String, Object?>> items, Object? sender) => [
    for (final m in items)
      if (same((m['key'] as Map)['sender'], sender)) (m['key'] as Map)['id'],
  ];
  Future<Map<String, Object?>> state(Participant p) async =>
      object(await call(p, 'queue_local_state'));
  Future<Map<String, Object?>> counts(Participant p, [Object? peer]) async =>
      object(await call(p, 'queue_local_test_counts', {'peer': ?peer}));
  Future<Map<String, Object?>> alerts(Participant p) async =>
      object(await call(p, 'mailbox_local_test_counters'));

  Future<void> stopped(Participant p, String side) async {
    await until(
      () => network(p),
      (v) => v['state'] == 'Stopped',
      'stopped_$side',
    );
  }

  // [p] no longer sees a session with any of [endpoints].
  Future<void> isolated(Participant p, List<Object?> endpoints) async {
    await until(
      () => network(p),
      (v) => !(v['peers'] as List).any(
        (e) =>
            endpoints.contains((e as Map)['peer_id']) &&
            const {'Connected', 'Connecting'}.contains(e['phase']),
      ),
      'isolated',
    );
  }

  // Setup: A = [A1, A2], B = [B1], one conversation with A's two devices.
  queueStats
    ..clear()
    ..touch();
  await create(a1);
  await link();
  final aPeer = (await snapshot(a1))['public_peer_id'];
  await create(b1);
  final bPeer = (await snapshot(b1))['public_peer_id'];
  require(!same(aPeer, bPeer), 'distinct_accounts');
  final a1Id = (await snapshot(a1))['installation'];
  final a2Id = (await snapshot(a2))['installation'];
  final b1Id = (await snapshot(b1))['installation'];
  await contact(b1, a1);
  await contact(b1, a2);
  await contact(a1, b1);
  await routes([a1, a2, b1]);
  for (final p in [a1, a2]) {
    await call(p, 'account_local_connect_account', {'peer': bPeer});
  }
  for (final p in [a1, a2]) {
    await call(p, 'mailbox_local_sync_now');
  }
  final conversation = object(
    await call(b1, 'message_local_open_dm', {'peer': aPeer}),
  )['id'];
  for (final p in [a1, a2]) {
    final own = object(
      await call(p, 'message_local_open_dm', {'peer': bPeer}),
    )['id'];
    require(same(own, conversation), 'one_conversation');
  }
  // The first text makes the group with both of A's devices.
  await call(b1, 'message_local_send_text', {
    'peer': aPeer,
    'id': id(0),
    'body': 'm8 setup text',
  });
  for (final p in [a1, a2]) {
    await until(
      () => messages(p, conversation),
      (items) => idsFrom(items, bPeer).length == 1,
      'first_text',
    );
  }

  // 1. No mailbox: each device makes a queue; B1 keeps A1's and A2's `QueueRef`s.
  for (final p in [a1, a2]) {
    final s = await until(
      () => state(p),
      (v) => v['using_queue'] == true,
      'using_queue',
    );
    require(s['mode'] == 'auto' && s['has_mailbox'] == false, 'no_mailbox');
  }
  // Every device keeps each other's `QueueRef` and has none left to deliver: one still on
  // its way to a device that goes off in step 2 would go into its queue beside the texts.
  final ids = {a1: a1Id, a2: a2Id, b1: b1Id};
  for (final p in [a1, a2, b1]) {
    for (final other in [a1, a2, b1]) {
      if (other == p) continue;
      await until(
        () => counts(p, ids[other]),
        (v) => v['peer_live'] == 1 && v['refs_pending'] == 0,
        'queue_refs',
      );
    }
  }
  final made = await until(held, (c) => c.queues == 3, 'server_queues');
  evidence?.call('queues', {'server': made.toJson()});

  // 2. A1 and A2 off line; B1's two texts go into their queues.
  final before = await alerts(a1);
  final endpoints = [
    for (final p in [a1, a2]) (await network(p))['endpoint_id'],
  ];
  for (final (p, side) in [(a1, 'phone'), (a2, 'other')]) {
    await call(p, 'network_stop');
    await stopped(p, side);
  }
  await isolated(b1, endpoints);
  // The app may start its network on its own; it is `Stopped` right before B1 sends.
  await stopped(a1, 'phone');
  final keys = <Object?>[];
  for (final n in [1, 2]) {
    keys.add(
      object(
        await call(b1, 'message_local_send_text', {
          'peer': aPeer,
          'id': id(n),
          'body': 'm8 queued text $n',
        }),
      )['key'],
    );
  }
  final queued = await until(
    held,
    (c) => c.entries.length >= 2 && c.entries[0] == 2 && c.entries[1] == 2,
    'server_held',
  );
  final sent = await messages(b1, conversation);
  for (final key in keys) {
    final text = sent.firstWhere((m) => same(m['key'], key));
    require(
      !const {'Delivered', 'Read'}.contains(text['state']),
      'sent_not_delivered',
    );
  }
  final sender = await counts(b1);
  queueStats
    ..sendsMessage = sender['sends_message'] as int?
    ..sendsStanding = sender['sends_standing'] as int?;
  evidence?.call('held', {
    'server': queued.toJson(),
    'sends_message': sender['sends_message'],
    'sends_standing': sender['sends_standing'],
  });

  // 3. B1 off line too, so the queue is the only way to A1; A1 back on line.
  await call(b1, 'network_stop');
  await stopped(b1, 'node');
  final start = Stopwatch()..start();
  await call(a1, 'network_start');
  final expected = [id(0), id(1), id(2)];
  final got = await until(() => messages(a1, conversation), (items) {
    final ids = idsFrom(items, bPeer);
    if (ids.length > 1) {
      queueStats.firstTextAfterStartMs ??= start.elapsedMilliseconds;
    }
    return ids.length >= 3;
  }, 'phone_received');
  require(same(idsFrom(got, bPeer), expected), 'phone_once');
  // Nothing more arrives afterwards (no second copy, no second alert).
  await Future<void>.delayed(const Duration(seconds: 3));
  require(
    same(idsFrom(await messages(a1, conversation), bPeer), expected),
    'phone_no_duplicate',
  );
  final after = await alerts(a1);
  final notifications =
      (after['notifications'] as int) - (before['notifications'] as int);
  final notified = (after['notified'] as int) - (before['notified'] as int);
  require(notifications == 1 && notified == 2, 'one_notification');
  final phone = await until(
    () => counts(a1),
    (v) => (v['fetched'] as int) >= 2 && (v['acked'] as int) >= 1,
    'phone_acked',
  );
  queueStats
    ..fetched = phone['fetched'] as int?
    ..acked = phone['acked'] as int?;
  evidence?.call('phone_received', {
    'texts': 2,
    'notifications': notifications,
    'notified': notified,
    'fetched': phone['fetched'],
    'acked': phone['acked'],
    'first_text_after_start_ms': queueStats.firstTextAfterStartMs,
  });
  // B1 back on line: A1's receipts reach it (directly, or from its own queue).
  await call(b1, 'network_start');
  await routes([b1]);
  await call(b1, 'account_local_route', {'ticket': await ticket(a1)});
  await until(
    () => messages(b1, conversation),
    (items) => keys.every(
      (key) => items.any(
        (m) =>
            same(m['key'], key) &&
            const {'Delivered', 'Read'}.contains(m['state']),
      ),
    ),
    'sender_delivered',
    within: slow,
  );
  // Only A2's two entries are left: A1 and B1 took and acknowledged theirs.
  final drained = await until(
    held,
    (c) => same(c.entries, [2, 0, 0]),
    'phone_queue_empty',
    within: slow,
  );
  evidence?.call('sender_delivered', {'server': drained.toJson()});

  // 4. A2 back on line as a qualified mailbox: A1 stops using its queue.
  await call(a2, 'network_start');
  await routes([a2]);
  await call(a2, 'account_local_route', {'ticket': await ticket(a1)});
  await call(a2, 'mailbox_local_set', {'enabled': true});
  await call(a2, 'mailbox_local_test_seed_uptime', {
    'permille': 950,
    'recorded_ms': 7 * 86400000,
  });
  final other = await until(
    () => messages(a2, conversation),
    (items) => idsFrom(items, bPeer).length >= 3,
    'other_received',
  );
  require(same(idsFrom(other, bPeer), expected), 'other_once');
  // A2's `Announce` rides A1's session with it; every few seconds A1 dials its own devices.
  var reads = 0;
  final judged = await until(
    () async {
      if (reads++ % 20 == 0) await call(a1, 'mailbox_local_sync_now');
      return state(a1);
    },
    (v) => v['has_mailbox'] == true && v['using_queue'] == false,
    'phone_has_mailbox',
  );
  final retired = await until(
    () => counts(a1),
    (v) => v['own_active'] == 0 && v['own_retiring'] == 1,
    'phone_retiring',
  );
  await call(a1, 'account_local_connect_account', {'peer': bPeer});
  await until(
    () => counts(b1, a1Id),
    (v) => v['peer_withdrawn'] == 1 && v['peer_live'] == 0,
    'sender_withdrawn',
    within: slow,
  );
  // The `Retiring` queue stays at the server until its `DELETE`, 48 h on.
  final kept = await held();
  require(kept.queues == 3, 'retiring_kept');
  evidence?.call('withdrawn', {
    'has_mailbox': judged['has_mailbox'],
    'using_queue': judged['using_queue'],
    'own_retiring': retired['own_retiring'],
    'server': kept.toJson(),
  });
}
