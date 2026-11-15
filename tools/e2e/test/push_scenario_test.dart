import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:chat_e2e/src/controller.dart';
import 'package:chat_e2e/src/diagnostic.dart';
import 'package:chat_e2e/src/protocol.dart';
import 'package:chat_e2e/src/push_scenario.dart';
import 'package:test/test.dart';

const privateMarker = 'PRIVATE_M7_BODY_TOKEN_SENTINEL';
const tag = 'BwcHBwcHBwcHBwcHBwcHBw';

Future<ProcessResult> cli(String profile, String scenario, String root) =>
    Process.run(Platform.resolvedExecutable, [
      'run',
      'bin/run.dart',
      '--profile',
      profile,
      '--scenario',
      scenario,
      '--repo-root',
      root,
    ]);

Future<int> post(String url, String body) async {
  final client = HttpClient();
  try {
    final request = await client.postUrl(Uri.parse(url));
    request.headers.contentType = ContentType.json;
    request.write(body);
    final response = await request.close();
    await response.drain<void>();
    return response.statusCode;
  } finally {
    client.close(force: true);
  }
}

void main() {
  group('CLI', () {
    for (final scenario in ['m7', 'm7-fcm']) {
      test('admits $scenario with the android profile only', () async {
        final root = await Directory.systemTemp.createTemp('m7-cli-');
        try {
          final refused = await cli('windows', scenario, root.path);
          expect(refused.exitCode, 64);
          expect(refused.stderr, contains('m7'));
          expect(root.listSync(recursive: true), isEmpty);
          final admitted = await cli('android', scenario, root.path);
          expect(admitted.exitCode, 1);
          expect(admitted.stdout, isNot(contains('PASS')));
          expect(
            root
                .listSync(recursive: true)
                .whereType<File>()
                .any((f) => f.path.endsWith('failure.log')),
            isTrue,
          );
        } finally {
          await root.delete(recursive: true);
        }
      });
    }
  });

  group('fake upstream receiver', () {
    late FakeUpstreamReceiver upstream;
    setUp(() async {
      pushStats.clear();
      upstream = await FakeUpstreamReceiver.start();
    });
    tearDown(() => upstream.close());

    test('takes each kind as the receiver extras, in order', () async {
      expect(upstream.url, startsWith('http://127.0.0.1:'));
      for (final body in [
        {'platform': 'fcm', 't': tag, 'kind': 'cipher', 'e': 'Y2lwaGVy'},
        {'platform': 'fcm', 't': tag, 'kind': 'wake'},
        {'platform': 'fcm', 't': tag, 'kind': 'read'},
        {'platform': 'fcm', 't': tag, 'kind': 'cipher', 'e': 'c2Vjb25k'},
      ]) {
        expect(await post(upstream.url, jsonEncode(body)), 200);
      }
      const wait = Duration(seconds: 1);
      expect((await upstream.next('read', wait)).extras, {'t': tag, 'r': '1'});
      expect((await upstream.next('cipher', wait)).extras, {
        't': tag,
        'e': 'Y2lwaGVy',
      });
      expect((await upstream.next('wake', wait)).extras, {'t': tag, 'w': '1'});
      expect(upstream.pending('cipher'), 1);
      expect((await upstream.next('cipher', wait)).extras['e'], 'c2Vjb25k');
      expect(pushStats.items, {'cipher': 2, 'wake': 1, 'read': 1});
      expect(pushStats.sent, 4);
      expect(pushStats.failed, 0);
    });

    test('refuses anything but an FCM delivery of the proxy', () async {
      for (final body in [
        jsonEncode({'platform': 'apns', 't': tag, 'kind': 'wake'}),
        jsonEncode({'platform': 'fcm', 't': 'short', 'kind': 'wake'}),
        jsonEncode({'platform': 'fcm', 't': tag, 'kind': 'cipher'}),
        jsonEncode({'platform': 'fcm', 't': tag, 'kind': 'cipher', 'e': '+/'}),
        jsonEncode({'platform': 'fcm', 't': tag, 'kind': 'wake', 'e': 'x'}),
        jsonEncode({'platform': 'fcm', 't': tag, 'kind': 'other'}),
        jsonEncode({'platform': 'fcm', 't': tag, 'kind': 'read', 'x': 1}),
        'not json',
        jsonEncode([1]),
      ]) {
        expect(await post(upstream.url, body), 400, reason: body);
      }
      expect(pushStats.failed, 9);
      expect(pushStats.sent, 0);
      await expectLater(
        upstream.next('wake', const Duration(milliseconds: 200)),
        throwsA(
          isA<ScenarioDeadline>().having(
            (e) => e.message,
            'text',
            'Push scenario deadline: delivery_wake',
          ),
        ),
      );
    });
  });

  test('notifications come from the Notification List, once each', () {
    const dump = '''
NOTIFICATION MANAGER (dumpsys notification)
  Notification List:
    NotificationRecord(0x0a1: pkg=dev.chatproject.chat_app user=UserHandle{0} id=11 tag=message importance=3 key=0|dev.chatproject.chat_app|11|message|10190: Notification(channel=messages shortcut=null contentView=null vibrate=null sound=null defaults=0x0 flags=0x10 color=0x00000000 category=msg vis=PRIVATE))
      uid=10190 userId=0
      extras={
        android.title=String (\u2068push fixture\u2069)
        android.text=String (m7 push text one)
        android.reduced.images=Boolean (true)
      }
    NotificationRecord(0x0a2: pkg=com.android.systemui user=UserHandle{0} id=3 tag=null importance=2 key=0|com.android.systemui|3|null|10010: Notification(channel=ALR))
      extras={
        android.title=String (Other app)
      }
    NotificationRecord(0x0a3: pkg=dev.chatproject.chat_app user=UserHandle{0} id=0 tag=push importance=3 key=0|dev.chatproject.chat_app|0|push|10190: Notification(channel=messages))
      extras={
        android.title=String (새 메시지)
      }
  Snoozed notifications:
    NotificationRecord(0x0a4: pkg=dev.chatproject.chat_app user=UserHandle{0} id=12 tag=message importance=3 key=0|dev.chatproject.chat_app|12|message|10190: Notification(channel=messages))
      extras={
        android.title=String (snoozed)
      }
''';
    final found = parseNotifications(dump, 'dev.chatproject.chat_app');
    expect(found.map((n) => (n.tag, n.title, n.text)), [
      ('message', 'push fixture', 'm7 push text one'),
      ('push', '새 메시지', null),
    ]);
    expect(parseNotifications('', 'dev.chatproject.chat_app'), isEmpty);
  });

  test('previews follow the core rule: 48 bytes on a character boundary', () {
    expect(previewOf('short'), 'short');
    expect(previewOf('short', more: 2), 'short 외 2개');
    final long = 'm7 wake text '.padRight(16384, 'w');
    expect(previewOf(long), long.substring(0, 48));
    // 47 ASCII bytes and a 3-byte character: the character is left out.
    expect(previewOf('${'a' * 47}한글'), 'a' * 47);
    expect(utf8.encode(previewOf('🦀' * 20)).length, 48);
  });

  test('the push diagnostic holds fixed keys and counts only', () {
    pushStats.clear();
    expect(pushStats.toSafeJson(), isNull);
    pushStats
      ..touch()
      ..items['cipher'] = 2
      ..sent = 2
      ..broadcasts = 2
      ..notifications = 1;
    final safe = pushStats.toSafeJson()!;
    expect(safe.keys.toSet(), {
      'push_items',
      'push_results',
      'broadcasts',
      'notifications',
    });
    expect(safe['push_items'], {'cipher': 2, 'wake': 0, 'read': 0});
    expect(safe['push_results'], {'sent': 2, 'failed': 0});
    pushStats.clear();
  });

  test('scenario runs its steps in order and relays each push', () async {
    final fixture = PushFixture();
    await fixture.start();
    waitMs.clear();
    try {
      final stages = <String>[];
      await fixture.run(
        evidence: (stage, data) {
          stages.add(stage);
          expect(jsonEncode(data), isNot(contains(privateMarker)));
        },
      );
      expect(stages, [
        'handles',
        'phone_killed',
        'cold_notification',
        'reopened',
        'read_cleared',
        'wake_content',
      ]);
      expect(fixture.actions, [
        'kill',
        'push e,t',
        'relaunch',
        'push e,t',
        'push r,t',
        'push t,w',
      ]);
      expect(fixture.texts['android-1']!.length, 4);
      final safe = jsonEncode(pushStats.toSafeJson());
      expect(safe, isNot(contains(privateMarker)));
      expect(safe, isNot(contains(tag)));
      expect(pushStats.items, {'cipher': 2, 'wake': 1, 'read': 1});
      expect(pushStats.broadcasts, 4);
      expect(pushStats.notifications, 1);
      expect(
        waitMs.keys,
        containsAll([
          'Push scenario deadline: cold_notification',
          'Push scenario deadline: wake_content',
        ]),
      );
    } finally {
      waitMs.clear();
      await fixture.close();
    }
  });

  test('a missing read push notes what the mailbox decided', () async {
    final fixture = PushFixture()..fault = 'stale_session';
    await fixture.start();
    failureFacts.clear();
    try {
      await expectLater(
        fixture.run(),
        throwsA(
          isA<ScenarioDeadline>().having(
            (e) => e.message,
            'text',
            'Push scenario deadline: delivery_read',
          ),
        ),
      );
      expect(failureFacts, {
        'm7_read_own_handles': 'h1plus',
        'm7_read_sessions': 's0',
        'm7_read_clears': 'c1',
        'm7_read_items': 'i0',
      });
      expect(safeFacts(failureFacts, loadSafeFacts()), failureFacts);
    } finally {
      failureFacts.clear();
      await fixture.close();
    }
  });

  test(
    'a notification left after a read push notes A1 fetches and own-device sessions',
    () async {
      final fixture = PushFixture()..fault = 'no_cancel';
      await fixture.start();
      failureFacts.clear();
      try {
        await expectLater(
          fixture.run(),
          throwsA(
            isA<ScenarioDeadline>().having(
              (e) => e.message,
              'text',
              'Push scenario deadline: read_cleared',
            ),
          ),
        );
        expect(failureFacts, {
          'm7_fetch_all': 'f1',
          'm7_fetch_online': 'o1',
          'm7_fetch_done': 'd0',
          'm7_fetch_dials_ok': 'k1',
          'm7_fetch_dials_failed': 'x0',
          'm7_fetch_dial_error': 'none',
          'm7_fetch_pulls': 'p1',
          'm7_fetch_pulls_failed': 'pf1',
          'm7_fetch_pulls_stopped': 'ps0',
          'm7_mailbox_accepts_ok': 'a2',
          'm7_mailbox_accepts_failed': 'r0',
          'm7_mailbox_accept_error': 'none',
          'm7_fetch_dial_check': 'record',
          'm7_mailbox_accept_check': 'none',
          'm7_fetch_network': 'Stopped',
          'm7_mailbox_record': 'Observed',
          'm7_phone_record': 'Observed',
        });
        expect(safeFacts(failureFacts, loadSafeFacts()), failureFacts);
      } finally {
        failureFacts.clear();
        await fixture.close();
      }
    },
  );

  test('a ticket deadline notes every side network state', () async {
    final fixture = PushFixture()..fault = 'no_start';
    await fixture.start();
    failureFacts.clear();
    try {
      await expectLater(
        fixture.run(),
        throwsA(
          isA<ScenarioDeadline>()
              .having(
                (e) => e.message,
                'text',
                'Push scenario deadline: ticket',
              )
              .having((e) => e.participant, 'participant', 'android-1'),
        ),
      );
      expect(failureFacts, {
        'm7_network_phone': 'Stopped',
        'm7_network_mailbox': 'Online',
        'm7_network_node': 'Online',
      });
      expect(
        safeFacts(failureFacts, loadSafeFacts()),
        failureFacts,
        reason: 'every m7 network fact is allowlisted',
      );
    } finally {
      failureFacts.clear();
      await fixture.close();
    }
  });

  for (final (fault, failure) in [
    (
      'wrong_title',
      isA<ScenarioDeadline>().having(
        (e) => e.message,
        'text',
        'Push scenario deadline: cold_notification',
      ),
    ),
    (
      'duplicate',
      isA<StateError>().having(
        (e) => e.message,
        'text',
        'Push scenario: cold_text_once',
      ),
    ),
    (
      'no_start',
      isA<ScenarioDeadline>().having(
        (e) => e.message,
        'text',
        'Push scenario deadline: ticket',
      ),
    ),
    (
      'stale_session',
      isA<ScenarioDeadline>().having(
        (e) => e.message,
        'text',
        'Push scenario deadline: delivery_read',
      ),
    ),
    (
      'no_cancel',
      isA<ScenarioDeadline>().having(
        (e) => e.message,
        'text',
        'Push scenario deadline: read_cleared',
      ),
    ),
    (
      'no_wake_fetch',
      isA<ScenarioDeadline>().having(
        (e) => e.message,
        'text',
        'Push scenario deadline: wake_content',
      ),
    ),
  ]) {
    test('scenario fails on $fault', () async {
      final fixture = PushFixture()..fault = fault;
      await fixture.start();
      try {
        await expectLater(fixture.run(), throwsA(failure));
      } finally {
        await fixture.close();
      }
    });
  }
}

// Orchestration fixture only: no native account, MLS, proxy or Android claim.
// B1's texts to an unreachable A1 become proxy deliveries posted to the real
// receiver; the fake phone turns relayed pushes into notifications.
class PushFixture {
  late Directory logs;
  late Controller controller;
  late FakeUpstreamReceiver upstream;
  final sockets = <String, WebSocket>{};
  final participants = <String, Participant>{};
  final actions = <String>[];
  String? fault;
  final phase = {
    'android-1': 'Unconfigured',
    'mailbox': 'Unconfigured',
    'node': 'Unconfigured',
  };
  final texts = <String, List<Map<String, Object?>>>{
    'android-1': [],
    'mailbox': [],
    'node': [],
  };
  final bodies = <String, String>{};
  final unread = <String>[];
  var notifications = <AppNotification>[];
  var paired = false;
  var confirmed = 0;
  var token = false;
  var killed = false;
  var stopped = false;
  // The relaunched app's network is off until `network_start` (the core's `restore`).
  var offAfterRelaunch = false;
  var lingering = 3;
  var readClears = 0;
  var readItems = 0;
  var fetches = 0;
  var launches = 0;
  var lamport = 0;
  final conversation = List.filled(32, 5);
  List<int> peer(String name) => List.filled(32, name == 'node' ? 2 : 1);
  List<int> install(String name) =>
      List.filled(32, {'android-1': 3, 'mailbox': 4, 'node': 6}[name]!);
  bool get reachable => !killed && !stopped;

  Future<void> run({void Function(String, Map<String, Object?>)? evidence}) =>
      runPushScenario(
        controller,
        [
          participants['android-1']!,
          participants['mailbox']!,
          participants['node']!,
        ],
        phone: PushPhone(
          kill: () async {
            actions.add('kill');
            killed = true;
            await sockets.remove('android-1')!.close();
          },
          relaunch: () async {
            actions.add('relaunch');
            killed = false;
            stopped = false;
            offAfterRelaunch = true;
            return join('android-1');
          },
          push: (extras) async {
            actions.add('push ${(extras.keys.toList()..sort()).join(',')}');
            pushed(extras);
          },
          notifications: () async => notifications,
        ),
        proxyUrl: 'http://127.0.0.1:8787',
        upstream: upstream,
        evidence: evidence,
        timeout: const Duration(seconds: 2),
      );

  Future<void> start() async {
    pushStats.clear();
    logs = await Directory.systemTemp.createTemp('m7-scenario-');
    controller = await Controller.start(
      port: 0,
      logDir: logs,
      accountRecords: true,
    );
    upstream = await FakeUpstreamReceiver.start();
    for (final name in ['android-1', 'mailbox', 'node']) {
      await join(name);
    }
  }

  String idOf(Object? key) => jsonEncode((key as Map)['id']);

  void show() {
    if (unread.isEmpty) {
      notifications = [];
      return;
    }
    notifications = [
      AppNotification(
        'message',
        fault == 'wrong_title' ? 'someone' : pushAlias,
        previewOf(bodies[unread.last]!, more: unread.length - 1),
      ),
    ];
  }

  void storeOnPhone(Map<String, Object?> text) {
    final id = idOf(text['key']);
    if (texts['android-1']!.any((m) => idOf(m['key']) == id)) return;
    texts['android-1']!.add(text);
    unread.add(id);
  }

  // The fake TestPushReceiver: what the core does with each kind.
  void pushed(Map<String, String> extras) {
    final latest = texts['node']!.last;
    if (extras.containsKey('e')) {
      storeOnPhone(latest);
      show();
    } else if (extras.containsKey('w')) {
      notifications = [AppNotification('message', pushAlias, '새 메시지 1개')];
      if (fault == 'no_wake_fetch') return;
      Timer(const Duration(milliseconds: 300), () {
        storeOnPhone(latest);
        show();
      });
    } else if (extras.containsKey('r')) {
      fetches++;
      if (fault == 'no_cancel') return;
      unread.clear();
      show();
    }
  }

  // What the proxy would hand its upstream for a push to A1.
  void deliver(String kind, {String? e}) => unawaited(
    post(
      upstream.url,
      jsonEncode({'platform': 'fcm', 't': tag, 'kind': kind, 'e': ?e}),
    ),
  );

  Object? handle(String name, String op, Map args) {
    switch (op) {
      case 'account_local_snapshot':
        final active = phase[name] == 'Active';
        return {
          'phase': phase[name],
          'public_peer_id': active || name == 'android-1' ? peer(name) : null,
          'installation': install(name),
          'pair': paired ? {'sas': 'fixture', 'phase': _pairPhase} : null,
          'publication': {
            'state': 'Observed',
            'accepted': {
              'devices': [
                {'installation_id': install('android-1')},
                if (confirmed == 3) {'installation_id': install('mailbox')},
              ],
            },
          },
        };
      case 'account_local_begin_create':
        return {
          'flow': List.filled(16, 9),
          'words': List.filled(24, privateMarker),
          'positions': [1, 2, 3],
        };
      case 'account_local_confirm_create':
        phase[name] = 'Active';
      case 'account_local_refresh_record':
        if (name == 'mailbox' && confirmed == 3) phase[name] = 'Active';
      case 'account_local_prepare_candidate':
        phase[name] = 'Candidate';
      case 'account_local_pair_begin':
        return {'flow': List.filled(16, 7), 'qr': privateMarker};
      case 'account_local_pair_scan':
        paired = true;
        return List.filled(16, 7);
      case 'account_local_pair_confirm_code' || 'account_local_pair_approve':
        confirmed++;
      case 'account_local_create_invite':
        return privateMarker;
      case 'account_local_accept_invite':
        return {'verified': false};
      case 'account_local_route' ||
          'account_local_connect_account' ||
          'mailbox_local_sync_now' ||
          'mailbox_local_set' ||
          'push_local_configure' ||
          'push_local_renew_due':
        break;
      case 'push_local_set_token':
        token = true;
      case 'push_local_test_counts':
        return {
          'handles': 0,
          'announced': 0,
          'own_handle': false,
          'peer_handles': token && name == 'node' ? 1 : 0,
          'peer_withdrawn': 0,
          'own_handles': token && name == 'mailbox' ? 1 : 0,
          'deliveries': 0,
          // A2's session to A1 lingers for a few reads after A1 stops.
          'own_sessions': name == 'mailbox' && (reachable || lingering-- > 0)
              ? 1
              : 0,
          'read_clears': readClears,
          'read_items': readItems,
          'fetches': fetches,
          'fetches_online': fetches,
          'fetches_done': fault == 'no_cancel' ? 0 : fetches,
          // A1 dialed A2 for each read clear and A2 took it (and A1's setup dial); with
          // `no_cancel` each pull failed.
          'own_dials_ok': name == 'android-1' ? fetches : 0,
          'own_dials_failed': 0,
          'own_dial_error': 'none',
          'own_dial_check': fault == 'no_cancel' ? 'record' : 'none',
          'own_accepts_ok': name == 'mailbox' ? 1 + fetches : 0,
          'own_accepts_failed': 0,
          'own_accept_error': 'none',
          'own_accept_check': 'none',
          'pulls': name == 'android-1' ? fetches : 0,
          'pulls_done': name == 'android-1' && fault != 'no_cancel'
              ? fetches
              : 0,
          'pulls_failed': name == 'android-1' && fault == 'no_cancel'
              ? fetches
              : 0,
          'pulls_stopped': 0,
        };
      case 'network_start':
        if (name == 'android-1' && fault != 'no_start') {
          offAfterRelaunch = false;
        }
        return {'state': 'Online'};
      case 'network_info':
        final off = name == 'android-1' && (offAfterRelaunch || stopped);
        return {
          'state': off ? 'Stopped' : 'Online',
          'ticket': off ? null : 'ticket-$name',
          'endpoint_id': 'endpoint-$name',
          'peers': [
            if (name != 'android-1')
              {
                'peer_id': 'endpoint-android-1',
                'phase': reachable ? 'Connected' : 'Disconnected',
              },
          ],
        };
      case 'network_stop':
        stopped = true;
      case 'message_local_open_dm':
        return {'id': conversation};
      case 'message_local_send_text':
        final body = args['body'] as String;
        final text = <String, Object?>{
          'key': {
            'conversation': conversation,
            'sender': peer(name),
            'installation': install(name),
            'id': args['id'],
          },
          'lamport': ++lamport,
          'state': 'Delivered',
        };
        bodies[idOf(text['key'])] = body;
        texts['node']!.add(text);
        texts['mailbox']!.add(text);
        if (reachable) {
          storeOnPhone(text);
          show();
        } else if (token) {
          if (utf8.encode(body).length > 2900) {
            deliver('wake');
          } else {
            deliver('cipher', e: 'Y2lwaGVy');
          }
        }
        return text;
      case 'message_local_load_messages':
        final items = texts[name]!.toList();
        if (fault == 'duplicate' && name == 'android-1' && launches > 1) {
          items.add(items.last);
        }
        return {'revision': 1, 'items': items, 'next': null};
      case 'message_local_mark_read':
        if (name == 'android-1') {
          for (final key in args['keys'] as List) {
            unread.remove(idOf(key));
          }
          show();
        } else {
          // Spec 7: only with no session to A1.
          final session =
              reachable || lingering > 0 || fault == 'stale_session';
          readClears++;
          if (!session) {
            readItems++;
            deliver('read');
          }
        }
      default:
        throw StateError('Unexpected fixture operation $op');
    }
    return null;
  }

  String get _pairPhase => confirmed >= 2 ? 'AwaitingApproval' : 'Code';

  Future<Participant> join(String name) async {
    final socket = await WebSocket.connect('ws://127.0.0.1:${controller.port}');
    sockets[name] = socket;
    if (name == 'android-1') launches++;
    socket.listen((raw) {
      final request = jsonDecode(raw as String) as Map;
      if (request['type'] != 'cmd') return;
      final result = handle(
        name,
        request['op'] as String,
        request['args'] as Map,
      );
      socket.add(
        jsonEncode({
          'type': 'result',
          'id': request['id'],
          'ok': true,
          'value': result,
        }),
      );
    });
    socket.add(
      jsonEncode({
        'type': 'hello',
        'name': name,
        'instance': '$name-$launches',
        'protocol': puppetProtocol,
        'relay': localRelay,
      }),
    );
    final previous = participants[name];
    while (true) {
      final next = await controller.waitParticipant(name);
      if (previous == null || next.instance != previous.instance) {
        return participants[name] = next;
      }
      await Future<void>.delayed(const Duration(milliseconds: 20));
    }
  }

  Future<void> close() async {
    for (final socket in sockets.values) {
      await socket.close();
    }
    await upstream.close();
    try {
      await controller.close();
    } catch (_) {}
    await logs.delete(recursive: true);
    pushStats.clear();
  }
}
