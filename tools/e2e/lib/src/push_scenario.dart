import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'controller.dart';
import 'diagnostic.dart';
import 'protocol.dart';

/// One push the debug proxy (`push_proxy --fake-upstream-url`, Ruling R6)
/// handed to the harness instead of FCM: its kind and the extras the app's
/// debug `TestPushReceiver` takes (`t` with `e`, `w` or `r`, exactly what the
/// proxy builds for FCM `data`).
class PushDelivery {
  PushDelivery(this.kind, this.extras);
  final String kind;
  final Map<String, String> extras;
}

/// What the m7 run noted about pushes, as counts only (diagnostic `push`):
/// never a body, token, tag or handle.
class PushStats {
  final items = {'cipher': 0, 'wake': 0, 'read': 0};
  var sent = 0;
  var failed = 0;
  var broadcasts = 0;

  /// The app's message notifications at the latest read, or null.
  int? notifications;
  var _used = false;

  void clear() {
    items.updateAll((_, _) => 0);
    sent = failed = broadcasts = 0;
    notifications = null;
    _used = false;
  }

  void touch() => _used = true;

  /// The fixed `push` diagnostic, null when the run did no push work.
  Map<String, Object?>? toSafeJson() => !_used
      ? null
      : {
          'push_items': Map<String, int>.of(items),
          'push_results': {'sent': sent, 'failed': failed},
          'broadcasts': broadcasts,
          'notifications': notifications,
        };
}

final pushStats = PushStats();

final _tag = RegExp(r'^[A-Za-z0-9_-]{22}$');
final _cipher = RegExp(r'^[A-Za-z0-9_-]{1,4000}$');

/// The receiving end of the debug proxy's fake upstream: a loopback HTTP
/// server that takes one JSON delivery per POST (server/push_proxy
/// `HarnessUpstream`) and keeps it until the scenario relays it. Anything
/// else is refused and counted as failed.
class FakeUpstreamReceiver {
  FakeUpstreamReceiver._(this._server) {
    _server.listen(_handle);
  }

  static Future<FakeUpstreamReceiver> start() async => FakeUpstreamReceiver._(
    await HttpServer.bind(InternetAddress.loopbackIPv4, 0),
  );

  final HttpServer _server;
  final _queue = <PushDelivery>[];
  final _arrived = StreamController<void>.broadcast();

  /// What the proxy gets as `--fake-upstream-url`.
  String get url => 'http://127.0.0.1:${_server.port}/push';

  Future<void> _handle(HttpRequest request) async {
    PushDelivery? delivery;
    try {
      if (request.method == 'POST' && request.uri.path == '/push') {
        final bytes = <int>[];
        await for (final chunk in request) {
          bytes.addAll(chunk);
          if (bytes.length > 8192) break;
        }
        if (bytes.length <= 8192) delivery = parse(utf8.decode(bytes));
      }
    } catch (_) {
      delivery = null;
    }
    pushStats.touch();
    if (delivery == null) {
      pushStats.failed++;
      request.response.statusCode = HttpStatus.badRequest;
    } else {
      pushStats.sent++;
      pushStats.items[delivery.kind] = pushStats.items[delivery.kind]! + 1;
      _queue.add(delivery);
      _arrived.add(null);
      request.response.statusCode = HttpStatus.ok;
    }
    await request.response.close();
  }

  /// A delivery body as the proxy writes it, or null. Only FCM: the harness
  /// has one Android device.
  static PushDelivery? parse(String text) {
    final value = jsonDecode(text);
    if (value is! Map) return null;
    final kind = value['kind'];
    final keys = value.keys.toSet();
    final t = value['t'];
    if (value['platform'] != 'fcm' || t is! String || !_tag.hasMatch(t)) {
      return null;
    }
    switch (kind) {
      case 'cipher':
        final e = value['e'];
        if (keys.length != 4 || e is! String || !_cipher.hasMatch(e)) {
          return null;
        }
        return PushDelivery('cipher', {'t': t, 'e': e});
      case 'wake' || 'read':
        if (keys.length != 3) return null;
        return PushDelivery(kind as String, {
          't': t,
          if (kind == 'wake') 'w': '1' else 'r': '1',
        });
      default:
        return null;
    }
  }

  /// The oldest delivery of [kind] not taken yet; one of another kind stays.
  Future<PushDelivery> next(String kind, Duration timeout) async {
    final deadline = DateTime.now().add(timeout);
    while (true) {
      final index = _queue.indexWhere((d) => d.kind == kind);
      if (index >= 0) return _queue.removeAt(index);
      final left = deadline.difference(DateTime.now());
      if (left <= Duration.zero) {
        throw ScenarioDeadline('Push scenario deadline: delivery_$kind');
      }
      await _arrived.stream.first.timeout(left, onTimeout: () {});
    }
  }

  /// Deliveries of [kind] not taken yet.
  int pending(String kind) => _queue.where((d) => d.kind == kind).length;

  Future<void> close() async {
    await _server.close(force: true);
    await _arrived.close();
  }
}

/// One notification the app posted, from `dumpsys notification --noredact`.
class AppNotification {
  AppNotification(this.tag, this.title, this.text);
  final String? tag;
  final String? title;
  final String? text;
}

final _record = RegExp(r'NotificationRecord\(');
final _field = RegExp(r'\b(pkg|tag|key)=(\S+)');
final _extra = RegExp(r'^android\.(title|text)=\w+ \((.*)\)$');

/// The posted notifications of [package] in a `dumpsys notification
/// --noredact` dump: the records of its `Notification List`, once each by key.
/// Titles lose the directional isolates the app wraps aliases in.
List<AppNotification> parseNotifications(String dump, String package) {
  final lines = const LineSplitter().convert(dump);
  var start = lines.indexWhere((l) => l.trim() == 'Notification List:');
  int indent(String l) => l.length - l.trimLeft().length;
  final end = start < 0
      ? lines.length
      : () {
          final header = indent(lines[start]);
          final next = lines.indexWhere(
            (l) => l.trim().isNotEmpty && indent(l) <= header,
            start + 1,
          );
          return next < 0 ? lines.length : next;
        }();
  if (start < 0) start = 0;
  final found = <String, AppNotification>{};
  Map<String, String>? current;
  String? title;
  String? text;
  void flush() {
    final record = current;
    if (record != null && record['pkg'] == package) {
      final tag = record['tag'];
      found.putIfAbsent(
        record['key'] ?? '${found.length}',
        () => AppNotification(
          tag == 'null' ? null : tag,
          title?.replaceAll(RegExp('[\u2068\u2069]'), ''),
          text,
        ),
      );
    }
    current = null;
    title = text = null;
  }

  for (final line in lines.sublist(start, end)) {
    if (_record.hasMatch(line)) {
      flush();
      current = {
        for (final m in _field.allMatches(line.split('Notification(').first))
          m[1]!: m[2]!,
      };
      continue;
    }
    final extra = _extra.firstMatch(line.trim());
    if (current == null || extra == null) continue;
    if (extra[1] == 'title') {
      title ??= extra[2];
    } else {
      text ??= extra[2];
    }
  }
  flush();
  return found.values.toList();
}

/// The app on the emulator, as the m7 scenario drives it (bin/run.dart).
class PushPhone {
  PushPhone({
    required this.kill,
    required this.relaunch,
    required this.push,
    required this.notifications,
  });

  /// In the background, then its process killed (as the system or a swipe
  /// does): no session, no control connection, its notifications stay.
  final Future<void> Function() kill;

  /// A fresh process with the control connection (force-stop first): the
  /// participant it connects as.
  final Future<Participant> Function() relaunch;

  /// One push into the debug `TestPushReceiver` (adb broadcast); it starts the
  /// app's process if there is none.
  final Future<void> Function(Map<String, String>) push;

  /// The app's posted notifications.
  final Future<List<AppNotification>> Function() notifications;
}

/// The contact alias A1 gives B1, the title of its notifications.
const pushAlias = 'push fixture';

/// The notification body of a text (`chat_core` `preview_text`): its first
/// 48 bytes on a character boundary, ` 외 N개` for N other unread ones.
String previewOf(String body, {int more = 0}) {
  final bytes = utf8.encode(body);
  var end = bytes.length < 48 ? bytes.length : 48;
  while (end > 0 && end < bytes.length && (bytes[end] & 0xC0) == 0x80) {
    end--;
  }
  final prefix = utf8.decode(bytes.sublist(0, end));
  return more == 0 ? prefix : '$prefix 외 $more개';
}

/// M7 ④ (spec 11): A = [A1 the Android app, A2 a node as A's mailbox],
/// B = [B1 a node]. The pushes reach A1 through the local proxy, whose fake
/// upstream hands them to the harness, which broadcasts them to the app's
/// debug receiver ([upstream]); with [upstream] null they go through real FCM
/// (the nightly `android-fcm` job) and the run ends after step 2.
///
/// 1. A1 is onboarded with notification permission; its handle reaches B1 and
///    its own-device handle reaches A2.
/// 2. A1's process is killed; B1's text comes as a `cipher` push into a cold
///    process: a notification titled with the alias, the text as its body
///    (Ruling R19). The app opened again holds the text once.
/// 3. With A1 offline B1's next text notifies; A2 reads both, and its `read`
///    push clears A1's notification.
/// 4. A 16 KiB text comes as a `wake` push: A1 shows it after fetching
///    (first "새 메시지" when that is seen in time), and holds every text once.
Future<void> runPushScenario(
  Controller controller,
  List<Participant> participants, {
  required PushPhone phone,
  required String proxyUrl,
  FakeUpstreamReceiver? upstream,
  void Function(String, Map<String, Object?>)? evidence,
  Duration timeout = const Duration(seconds: 60),
}) async {
  if (participants.length != 3) {
    throw ArgumentError('Phone, mailbox and node participants required');
  }
  var a1 = participants[0];
  final a2 = participants[1];
  final b1 = participants[2];
  Map<String, Object?> object(Object? value) =>
      (value as Map).cast<String, Object?>();
  bool same(Object? left, Object? right) =>
      jsonEncode(left) == jsonEncode(right);
  void require(bool ok, String code) {
    if (!ok) throw StateError('Push scenario: $code');
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
    String phase, {
    Duration? within,
  }) async {
    final name = 'Push scenario deadline: $phase';
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
  // On a ticket deadline: every side's network state (`m7_network_<side>`), so the summary
  // names which one was not online and how.
  Future<void> noteNetworks() async {
    for (final (side, p) in [('phone', a1), ('mailbox', a2), ('node', b1)]) {
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
      failureFacts['m7_network_$side'] =
          const {'Stopped', 'Starting', 'Online', 'Failed'}.contains(state)
          ? state as String
          : 'unknown';
    }
  }

  // On a missing `read` push: what A2 decided (`m7_read_*`, fixed buckets).
  Future<void> noteReadClear() async {
    Object? value;
    try {
      value = await a2.call(
        'push_local_test_counts',
        const {},
        timeout: const Duration(seconds: 5),
        failOnTimeout: false,
      );
    } catch (_) {
      value = null;
    }
    final map = value is Map ? value : const {};
    String bucket(Object? n, String prefix, int top) => n is! int
        ? 'unknown'
        : n >= top
        ? '$prefix${top}plus'
        : '$prefix$n';
    failureFacts['m7_read_own_handles'] = bucket(map['own_handles'], 'h', 1);
    failureFacts['m7_read_sessions'] = bucket(map['own_sessions'], 's', 1);
    failureFacts['m7_read_clears'] = bucket(map['read_clears'], 'c', 2);
    failureFacts['m7_read_items'] = bucket(map['read_items'], 'i', 1);
  }

  // On a notification a push should have changed: A1's fetches for received pushes
  // (`m7_fetch_*`: all, online in time, done in time) and its network now.
  Future<void> noteFetches() async {
    Object? value;
    Object? info;
    try {
      value = await a1.call(
        'push_local_test_counts',
        const {},
        timeout: const Duration(seconds: 5),
        failOnTimeout: false,
      );
      info = await a1.call(
        'network_info',
        const {},
        timeout: const Duration(seconds: 5),
        failOnTimeout: false,
      );
    } catch (_) {
      /* What could not be read stays unknown. */
    }
    final map = value is Map ? value : const {};
    String bucket(Object? n, String prefix, int top) => n is! int
        ? 'unknown'
        : n >= top
        ? '$prefix${top}plus'
        : '$prefix$n';
    failureFacts['m7_fetch_all'] = bucket(map['fetches'], 'f', 2);
    failureFacts['m7_fetch_online'] = bucket(map['fetches_online'], 'o', 2);
    failureFacts['m7_fetch_done'] = bucket(map['fetches_done'], 'd', 2);
    final state = info is Map ? info['state'] : null;
    failureFacts['m7_fetch_network'] =
        const {'Stopped', 'Starting', 'Online', 'Failed'}.contains(state)
        ? state as String
        : 'unknown';
  }

  Future<Object?> ticket(Participant p) async {
    try {
      return (await until(
        () => network(p),
        (v) => v['ticket'] is String,
        'ticket',
      ))['ticket'];
    } on ScenarioDeadline {
      await noteNetworks();
      rethrow;
    }
  }

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

  Future<void> contact(
    Participant owner,
    Participant recipient,
    String alias,
  ) async {
    Object? invite = await call(owner, 'account_local_create_invite', {
      'name': null,
    });
    try {
      await call(recipient, 'account_local_accept_invite', {
        'input': invite,
        'alias': alias,
      });
    } finally {
      invite = null;
    }
  }

  // Every participant learns [to]'s current addresses.
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

  List<int> id(int n) => List.filled(16, 0x70 + n);
  List<Object?> idsFrom(List<Map<String, Object?>> items, Object? sender) => [
    for (final m in items)
      if (same((m['key'] as Map)['sender'], sender)) (m['key'] as Map)['id'],
  ];
  Object? keyOf(List<Map<String, Object?>> items, int n) =>
      items.firstWhere((m) => same((m['key'] as Map)['id'], id(n)))['key'];
  Future<Map<String, Object?>> counts(Participant p) async =>
      object(await call(p, 'push_local_test_counts'));
  Future<List<AppNotification>> shown() async {
    final all = await phone.notifications();
    final messages = all.where((n) => n.tag == 'message').toList();
    pushStats
      ..touch()
      ..notifications = messages.length;
    return messages;
  }

  Future<void> relay(String kind) async {
    final source = upstream;
    if (source == null) return; // Real FCM delivers it.
    final delivery = await source.next(kind, timeout * 2);
    await phone.push(delivery.extras);
    pushStats
      ..touch()
      ..broadcasts += 1;
  }

  // B1 and A2 no longer see a session with A1 (its network is stopped).
  Future<void> isolated(Object? endpoint) async {
    for (final p in [a2, b1]) {
      await until(
        () => network(p),
        (v) => !(v['peers'] as List).any(
          (e) =>
              (e as Map)['peer_id'] == endpoint &&
              const {'Connected', 'Connecting'}.contains(e['phase']),
        ),
        'isolated_${p == a2 ? 'mailbox' : 'sender'}',
      );
    }
  }

  // Setup: A = [A1, A2], B = [B1], one conversation with A's two devices.
  pushStats.clear();
  pushStats.touch();
  await create(a1);
  await link();
  final aPeer = (await snapshot(a1))['public_peer_id'];
  await create(b1);
  final bPeer = (await snapshot(b1))['public_peer_id'];
  require(!same(aPeer, bPeer), 'distinct_accounts');
  await contact(b1, a1, pushAlias);
  await contact(b1, a2, pushAlias);
  await contact(a1, b1, 'push sender');
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
  // The nodes send pushes only (no platform); A1's app configures itself.
  for (final p in [a2, b1]) {
    await call(p, 'push_local_configure', {
      'proxy_url': proxyUrl,
      'platform': null,
    });
  }
  // The first text makes the group with both of A's devices.
  await call(b1, 'message_local_send_text', {
    'peer': aPeer,
    'id': id(0),
    'body': 'm7 setup text',
  });
  for (final p in [a1, a2]) {
    await until(
      () => messages(p, conversation),
      (items) => idsFrom(items, bPeer).length == 1,
      'first_text',
    );
  }
  await call(a2, 'mailbox_local_set', {'enabled': true});

  // 1. A1's handles: the conversation's to B1, the own-device one to A2.
  if (upstream != null) {
    await call(a1, 'push_local_set_token', {
      'platform': 'fcm',
      'token': 'e2e-fake-token',
    });
  } else {
    // Firebase gives the app a real token; Dart hands it to the core.
    await until(
      () async => object(await call(a1, 'push_local_status')),
      (v) => v['enabled'] == true && v['platform'] == 'fcm',
      'fcm_platform',
    );
  }
  final b1Counts = await until(
    () async {
      await call(a1, 'push_local_renew_due');
      return counts(b1);
    },
    (v) => (v['peer_handles'] as int) >= 1,
    'handle_at_sender',
  );
  // The own-device handle rides A1's Announce over its session with A2 (sent again until A2
  // takes it); every few seconds A1 dials its own devices in case that session is not up.
  var reads = 0;
  final a2Counts = await until(
    () async {
      if (reads++ % 20 == 0) await call(a1, 'mailbox_local_sync_now');
      return counts(a2);
    },
    (v) => (v['own_handles'] as int) >= 1,
    'own_handle_at_mailbox',
  );
  final setup = await messages(a1, conversation);
  await call(a1, 'message_local_mark_read', {
    'conversation': conversation,
    'keys': [keyOf(setup, 0)],
  });
  await until(shown, (n) => n.isEmpty, 'setup_read');
  evidence?.call('handles', {
    'sender_peer_handles': b1Counts['peer_handles'],
    'mailbox_own_handles': a2Counts['own_handles'],
  });

  // 2. A cold process: the cipher push alone shows the text.
  await phone.kill();
  evidence?.call('phone_killed', {});
  const first = 'm7 push text one';
  await call(b1, 'message_local_send_text', {
    'peer': aPeer,
    'id': id(1),
    'body': first,
  });
  await relay('cipher');
  final cold = await until(
    shown,
    (n) => n.any((x) => x.title == pushAlias && x.text == previewOf(first)),
    'cold_notification',
    within: timeout * 2,
  );
  evidence?.call('cold_notification', {'notifications': cold.length});
  a1 = await phone.relaunch();
  // An account opened again keeps its network off until something starts it (the core's
  // `restore` installs the key without a start; the m3/m4 restarts do the same).
  await call(a1, 'network_start');
  await routes([a1]);
  for (final p in [a2, b1]) {
    await call(a1, 'account_local_route', {'ticket': await ticket(p)});
  }
  final reopened = await until(
    () => messages(a1, conversation),
    (items) => idsFrom(items, bPeer).length >= 2,
    'reopened',
  );
  require(same(idsFrom(reopened, bPeer), [id(0), id(1)]), 'cold_text_once');
  evidence?.call('reopened', {'texts': idsFrom(reopened, bPeer).length});
  if (upstream == null) return;

  // 3. A1 offline: B1's text notifies; A2 reads both, its read push clears.
  final endpoint = (await network(a1))['endpoint_id'];
  await call(a1, 'network_stop');
  await isolated(endpoint);
  const second = 'm7 push text two';
  await call(b1, 'message_local_send_text', {
    'peer': aPeer,
    'id': id(2),
    'body': second,
  });
  await relay('cipher');
  await until(
    shown,
    (n) => n.length == 1 && n.single.text == previewOf(second, more: 1),
    'offline_notification',
    within: timeout * 2,
  );
  final kept = await until(
    () => messages(a2, conversation),
    (items) => idsFrom(items, bPeer).length >= 3,
    'mailbox_texts',
  );
  // A read clear goes only to my devices without a ready session (spec 7). A1's network is
  // stopped, but A2 may hold its account session until it sees it close: wait for that.
  await until(
    () => counts(a2),
    (v) => v['own_sessions'] == 0,
    'mailbox_session_gone',
  );
  await call(a2, 'message_local_mark_read', {
    'conversation': conversation,
    'keys': [keyOf(kept, 1), keyOf(kept, 2)],
  });
  try {
    await relay('read');
  } on ScenarioDeadline {
    await noteReadClear();
    rethrow;
  }
  try {
    await until(shown, (n) => n.isEmpty, 'read_cleared', within: timeout * 2);
  } on ScenarioDeadline {
    await noteFetches();
    rethrow;
  }
  evidence?.call('read_cleared', {'notifications': 0});

  // 4. Too large for a push: `wake`, then the fetched text replaces it.
  await isolated(endpoint);
  final large = 'm7 wake text '.padRight(16384, 'w');
  await call(b1, 'message_local_send_text', {
    'peer': aPeer,
    'id': id(3),
    'body': large,
  });
  await relay('wake');
  var generic = false;
  final woken = await until(
    () async {
      final now = await shown();
      if (now.any((x) => x.text != null && x.text!.startsWith('새 메시지'))) {
        generic = true;
      }
      return now;
    },
    (n) => n.length == 1 && n.single.text == previewOf(large),
    'wake_content',
    within: timeout * 2,
  );
  require(woken.single.title == pushAlias, 'wake_title');
  // A1 back on line: every text once, in order.
  final all = await until(
    () => messages(a1, conversation),
    (items) => idsFrom(items, bPeer).length >= 4,
    'phone_texts',
  );
  require(
    same(idsFrom(all, bPeer), [for (var n = 0; n <= 3; n++) id(n)]),
    'phone_once',
  );
  await Future<void>.delayed(const Duration(seconds: 2));
  require((await shown()).length == 1, 'one_notification');
  require(
    upstream.pending('cipher') + upstream.pending('wake') == 0,
    'no_second_push',
  );
  evidence?.call('wake_content', {
    'generic_seen': generic,
    'notifications': 1,
    'texts': 4,
  });
}
