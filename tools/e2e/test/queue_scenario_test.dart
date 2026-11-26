import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:chat_e2e/src/controller.dart';
import 'package:chat_e2e/src/diagnostic.dart';
import 'package:chat_e2e/src/protocol.dart';
import 'package:chat_e2e/src/queue_scenario.dart';
import 'package:test/test.dart';
import 'controller_test.dart' show MockParticipant;

const privateMarker = 'PRIVATE_M8_QUEUE_ID_AND_KEY_SENTINEL';

Future<ProcessResult> cli(String profile, String root) =>
    Process.run(Platform.resolvedExecutable, [
      'run',
      'bin/run.dart',
      '--profile',
      profile,
      '--scenario',
      'm8',
      '--repo-root',
      root,
    ]);

Future<void> run(QueueFixture fixture, {Duration? timeout}) => runQueueScenario(
  fixture.controller,
  fixture.ordered,
  server: fixture.server,
  evidence: (stage, data) => fixture.stages.add((stage, data)),
  timeout: timeout ?? const Duration(seconds: 2),
  slow: timeout ?? const Duration(seconds: 2),
);

void main() {
  test('CLI admits m8 on android and preserves failure cleanup', () async {
    final root = await Directory.systemTemp.createTemp('m8-cli-');
    try {
      final result = await cli('android', root.path);
      expect(result.exitCode, 1);
      expect(result.stdout, isNot(contains('PASS')));
      final records = root.listSync(recursive: true).whereType<File>();
      expect(records.any((file) => file.path.endsWith('failure.log')), isTrue);
    } finally {
      await root.delete(recursive: true);
    }
  });
  for (final profile in ['windows', 'mixed']) {
    test('CLI refuses m8 on $profile: it needs an Android phone', () async {
      final root = await Directory.systemTemp.createTemp('m8-cli-');
      try {
        final result = await cli(profile, root.path);
        expect(result.exitCode, 64);
        expect(result.stderr, contains('m8'));
        expect(root.listSync(recursive: true), isEmpty);
      } finally {
        await root.delete(recursive: true);
      }
    });
  }
  test('queue counts parse only the fixed line', () {
    final read = QueueServerCounts.parse('{"queues":3,"entries":[2,2,0]}\n');
    expect(read.queues, 3);
    expect(read.entries, [2, 2, 0]);
    expect(read.held, 4);
    for (final bad in [
      '{"queues":2,"entries":[2,2,0]}',
      '{"queues":3,"entries":[0,2,2]}',
      '{"queues":1,"entries":[-1]}',
      '{"queues":1,"entries":["$privateMarker"]}',
      '{"queues":1,"entries":[1],"id":"$privateMarker"}',
      '[]',
    ]) {
      expect(() => QueueServerCounts.parse(bad), throwsFormatException);
    }
  });
  for (final op in ['set_mode', 'state', 'test_counts']) {
    test('queue $op result and error are wholly redacted', () async {
      final logs = await Directory.systemTemp.createTemp('m8-private-');
      final controller = await Controller.start(port: 0, logDir: logs);
      final peer = await MockParticipant.connect(controller);
      try {
        await peer.next();
        final participant = await controller.waitParticipant('node');
        final call = participant.call('queue_local_$op', {});
        final command = await peer.next();
        peer.result(command['id'] as int, {'value': privateMarker});
        expect(await call, {'value': privateMarker});
        final failure = expectLater(
          participant.call('queue_local_$op', {}),
          throwsA(
            isA<RpcError>()
                .having((e) => e.detail, 'detail', '[redacted]')
                .having((e) => e.op, 'op', 'queue_local_$op'),
          ),
        );
        final next = await peer.next();
        peer.send({
          'type': 'result',
          'id': next['id'],
          'ok': false,
          'error': {'code': 'NotAvailable', 'detail': privateMarker},
        });
        await failure;
        await controller.close();
        final retained = await File('${logs.path}/wire.jsonl').readAsString();
        expect(retained, isNot(contains(privateMarker)));
      } finally {
        await peer.close();
        await controller.close();
        await logs.delete(recursive: true);
      }
    });
  }
  test('the steps run in order and leave counts only', () async {
    final fixture = QueueFixture();
    await fixture.start();
    waitMs.clear();
    try {
      await run(fixture);
      expect(fixture.stages.map((s) => s.$1), [
        'queues',
        'held',
        'phone_received',
        'sender_delivered',
        'withdrawn',
      ]);
      // Step 3 found the queue the only way to the phone: B1 was off first.
      expect(fixture.actions, [
        'phone stop',
        'other stop',
        'node stop',
        'phone start',
        'node start',
        'other start',
        'other mailbox',
        'other uptime 950',
      ]);
      expect(fixture.stages[1].$2['server'], {
        'queues': 3,
        'entries': [2, 2, 0],
      });
      expect(fixture.stages[3].$2['server'], {
        'queues': 3,
        'entries': [2, 0, 0],
      });
      expect(fixture.texts['phone']!.length, 3);
      expect(fixture.texts['other']!.length, 3);
      final safe = queueStats.toSafeJson()!;
      expect(safe.keys, [
        'server_queues',
        'server_entries',
        'sends_message',
        'sends_standing',
        'fetched',
        'acked',
        'first_text_after_start_ms',
      ]);
      expect(safe.values.every((v) => v == null || v is int), isTrue);
      expect(safe['fetched'], 2);
      expect(safe['sends_message'], 2);
      expect(
        jsonEncode([
          for (final (stage, data) in fixture.stages) [stage, data],
        ]),
        isNot(contains(privateMarker)),
      );
      expect(
        waitMs.keys,
        containsAll([
          'Queue scenario deadline: server_held',
          'Queue scenario deadline: phone_received',
          'Queue scenario deadline: sender_withdrawn',
        ]),
      );
    } finally {
      waitMs.clear();
      await fixture.close();
    }
  });
  test('texts not held at the server note fixed facts', () async {
    final fixture = QueueFixture()..fault = 'not_held';
    await fixture.start();
    failureFacts.clear();
    try {
      await expectLater(
        run(fixture, timeout: const Duration(seconds: 1)),
        throwsA(
          isA<ScenarioDeadline>().having(
            (e) => e.message,
            'text',
            'Queue scenario deadline: server_held',
          ),
        ),
      );
      expect(failureFacts, {
        'm8_network_phone': 'Stopped',
        'm8_network_other': 'Stopped',
        'm8_network_node': 'Online',
        'm8_server_queues': 'q3plus',
        'm8_server_entries': 'e0',
      });
      expect(safeFacts(failureFacts, loadSafeFacts()), failureFacts);
    } finally {
      failureFacts.clear();
      await fixture.close();
    }
  });
  for (final (fault, code) in [
    ('delivered_early', 'Queue scenario: sent_not_delivered'),
    ('duplicate', 'Queue scenario: phone_once'),
    ('second_notification', 'Queue scenario: one_notification'),
    ('deleted_early', 'Queue scenario: retiring_kept'),
  ]) {
    test('the scenario fails on $fault', () async {
      final fixture = QueueFixture()..fault = fault;
      await fixture.start();
      try {
        await expectLater(
          run(fixture),
          throwsA(isA<StateError>().having((e) => e.message, 'code', code)),
        );
        expect(loadSafeDetails(), contains(code));
      } finally {
        await fixture.close();
      }
    });
  }
}

// Orchestration fixture only: no native account, MLS, queue server or network claim. A1
// ('phone') and A2 ('other') take B1's ('node') texts directly when on line, from their queue
// when they come back.
class QueueFixture {
  late Directory logs;
  late Controller controller;
  final sockets = <WebSocket>[];
  final participants = <String, Participant>{};
  final actions = <String>[];
  final stages = <(String, Map<String, Object?>)>[];
  String? fault;
  final phase = {
    'phone': 'Unconfigured',
    'other': 'Unconfigured',
    'node': 'Unconfigured',
  };
  final online = {'phone': true, 'other': true, 'node': true};
  final texts = <String, List<Map<String, Object?>>>{
    'phone': [],
    'other': [],
    'node': [],
  };
  // Entries in each device's queue at the server.
  final queue = <String, List<Map<String, Object?>>>{
    'phone': [],
    'other': [],
    'node': [],
  };
  var phoneAlerts = 0;
  var phoneAlerted = 0;
  var fetched = 0;
  var acked = 0;
  var sends = 0;
  var mailbox = false;
  var seeded = false;
  var paired = false;
  var confirmed = 0;
  var lamport = 0;
  final conversation = List.filled(32, 5);
  List<int> peer(String name) => List.filled(32, name == 'node' ? 2 : 1);
  List<int> install(String name) =>
      List.filled(32, {'phone': 3, 'other': 4, 'node': 6}[name]!);

  List<Participant> get ordered => [
    participants['phone']!,
    participants['other']!,
    participants['node']!,
  ];

  bool get judged => mailbox && seeded && online['other']!;

  Future<QueueServerCounts> server() async {
    final entries = [for (final q in queue.values) q.length]
      ..sort((a, b) => b.compareTo(a));
    if (fault == 'deleted_early' && judged) entries.removeLast();
    return QueueServerCounts(entries.length, entries);
  }

  Future<void> start() async {
    logs = await Directory.systemTemp.createTemp('m8-scenario-');
    controller = await Controller.start(
      port: 0,
      logDir: logs,
      accountRecords: true,
    );
    for (final name in ['phone', 'other', 'node']) {
      await join(name);
    }
  }

  void receive(String name, Map<String, Object?> text) {
    texts[name]!.add(text);
    if (name == 'phone') {
      phoneAlerts++;
      phoneAlerted++;
    }
  }

  // [name] comes back on line and takes what its queue holds.
  void fetch(String name) {
    final held = queue[name]!;
    if (held.isEmpty) return;
    if (name == 'node') {
      for (final text in texts['node']!) {
        text['state'] = 'Delivered';
      }
    } else {
      texts[name]!.addAll(held);
      if (fault == 'duplicate' && name == 'phone') texts[name]!.add(held.first);
      if (name == 'phone') {
        phoneAlerts += fault == 'second_notification' ? 2 : 1;
        phoneAlerted += held.length;
        fetched += held.length;
        acked += 1;
        // Its receipt goes into B1's queue: B1 is off.
        queue['node']!.add({'receipt': true});
      }
    }
    held.clear();
  }

  Object? handle(String name, String op, Map args) {
    switch (op) {
      case 'account_local_snapshot':
        final active = phase[name] == 'Active';
        return {
          'phase': phase[name],
          'public_peer_id': active || name == 'phone' ? peer(name) : null,
          'installation': install(name),
          'pair': paired ? {'sas': 'fixture', 'phase': _pairPhase} : null,
          'publication': {
            'state': 'Observed',
            'accepted': {
              'devices': [
                {'installation_id': install('phone')},
                if (confirmed == 3) {'installation_id': install('other')},
              ],
            },
          },
          'contacts': [],
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
        if (name == 'other' && confirmed == 3) phase[name] = 'Active';
      case 'account_local_prepare_candidate':
        phase[name] = 'Candidate';
      case 'account_local_pair_begin':
        return {'flow': List.filled(16, 7), 'qr': privateMarker};
      case 'account_local_pair_scan':
        paired = true;
        return List.filled(16, 7);
      case 'account_local_pair_confirm_code':
        confirmed++;
      case 'account_local_pair_approve':
        confirmed++;
      case 'account_local_create_invite':
        return privateMarker;
      case 'account_local_accept_invite':
        return {'verified': false};
      case 'account_local_route' || 'account_local_connect_account':
        break;
      case 'network_info':
        return {
          'ticket': online[name]! ? 'ticket-$name' : null,
          'endpoint_id': 'endpoint-$name',
          'state': online[name]! ? 'Online' : 'Stopped',
          'peers': [],
        };
      case 'network_stop':
        online[name] = false;
        actions.add('$name stop');
      case 'network_start':
        online[name] = true;
        actions.add('$name start');
        fetch(name);
      case 'mailbox_local_sync_now':
        break;
      case 'mailbox_local_set':
        mailbox = args['enabled'] as bool;
        actions.add('$name mailbox');
      case 'mailbox_local_test_seed_uptime':
        seeded = true;
        actions.add('$name uptime ${args['permille']}');
      case 'mailbox_local_test_counters':
        return {
          'missing': 0,
          'notifications': name == 'phone' ? phoneAlerts : 0,
          'notified': name == 'phone' ? phoneAlerted : 0,
        };
      case 'message_local_open_dm':
        return {'id': conversation};
      case 'message_local_send_text':
        final text = <String, Object?>{
          'key': {
            'conversation': conversation,
            'sender': peer(name),
            'installation': install(name),
            'id': args['id'],
          },
          'lamport': ++lamport,
          'state': fault == 'delivered_early' ? 'Delivered' : 'SentDirect',
        };
        texts['node']!.add(text);
        for (final to in ['phone', 'other']) {
          if (online[to]!) {
            receive(to, Map.of(text));
          } else if (fault != 'not_held') {
            queue[to]!.add(Map.of(text));
          }
        }
        if (!online['phone']! || !online['other']!) sends++;
        return text;
      case 'message_local_load_messages':
        return {'revision': 1, 'items': texts[name], 'next': null};
      case 'queue_local_state':
        final has = judged;
        return {
          'mode': 'auto',
          'has_mailbox': has,
          'using_queue': !has,
          'uptime_7d_permille': name == 'other' && seeded ? 950 : 0,
        };
      case 'queue_local_test_counts':
        final withdrawn =
            name == 'node' &&
            judged &&
            jsonEncode(args['peer']) == jsonEncode(install('phone'));
        return {
          'acked': name == 'phone' ? acked : 0,
          'created': 1,
          'fetched': name == 'phone' ? fetched : 0,
          'own_active': name == 'phone' && judged ? 0 : 1,
          'own_retiring': name == 'phone' && judged ? 1 : 0,
          'peer_dead': 0,
          'peer_live': withdrawn ? 0 : 1,
          'peer_withdrawn': withdrawn ? 1 : 0,
          'refs_pending': 0,
          'sends_message': name == 'node' ? sends : 0,
          'sends_standing': 0,
        };
      default:
        throw StateError('Unexpected fixture operation $op');
    }
    return null;
  }

  String get _pairPhase => confirmed >= 2 ? 'AwaitingApproval' : 'Code';

  Future<Participant> join(String name) async {
    final socket = await WebSocket.connect('ws://127.0.0.1:${controller.port}');
    sockets.add(socket);
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
        'instance': name,
        'protocol': puppetProtocol,
        'relay': localRelay,
      }),
    );
    return participants[name] = await controller.waitParticipant(name);
  }

  Future<void> close() async {
    for (final socket in sockets) {
      await socket.close();
    }
    try {
      await controller.close();
    } catch (_) {}
    await logs.delete(recursive: true);
  }
}
