import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:chat_e2e/src/controller.dart';
import 'package:chat_e2e/src/diagnostic.dart';
import 'package:chat_e2e/src/mailbox_scenario.dart';
import 'package:chat_e2e/src/protocol.dart';
import 'package:test/test.dart';
import 'controller_test.dart' show MockParticipant;

const privateMarker = 'PRIVATE_M6_BODY_AND_PHRASE_SENTINEL';

Future<ProcessResult> cli(String profile, String root) =>
    Process.run(Platform.resolvedExecutable, [
      'run',
      'bin/run.dart',
      '--profile',
      profile,
      '--scenario',
      'm6',
      '--repo-root',
      root,
    ]);

void main() {
  for (final profile in ['mixed', 'android']) {
    test('CLI admits m6 with $profile and preserves failure cleanup', () async {
      final root = await Directory.systemTemp.createTemp('m6-cli-');
      try {
        final result = await cli(profile, root.path);
        final admitted = Platform.isWindows || profile == 'android';
        expect(result.exitCode, admitted ? 1 : 64);
        expect(result.stdout, isNot(contains('PASS')));
        if (admitted) {
          final records = root.listSync(recursive: true).whereType<File>();
          expect(
            records.any((file) => file.path.endsWith('failure.log')),
            isTrue,
          );
        }
      } finally {
        await root.delete(recursive: true);
      }
    });
  }
  test('CLI refuses m6 without an Android phone', () async {
    final root = await Directory.systemTemp.createTemp('m6-cli-');
    try {
      final result = await cli('windows', root.path);
      expect(result.exitCode, 64);
      expect(result.stderr, contains('m6'));
      expect(root.listSync(recursive: true), isEmpty);
    } finally {
      await root.delete(recursive: true);
    }
  });
  test('notify events are admitted and kept out of diagnostics', () async {
    final logs = await Directory.systemTemp.createTemp('m6-notify-');
    final controller = await Controller.start(port: 0, logDir: logs);
    final peer = await MockParticipant.connect(controller);
    try {
      await peer.next();
      final participant = await controller.waitParticipant('node');
      peer.send({
        'type': 'event',
        'seq': 1,
        'instance': 'one',
        'event': {
          'kind': 'notify',
          'items': [
            {
              'conversation': List.filled(32, 5),
              'peer': List.filled(32, 2),
              'new_messages': 3,
              'unread': 3,
            },
          ],
        },
      });
      final event = await participant.expectEvent((e) => e['kind'] == 'notify');
      expect((event['items'] as List).single['new_messages'], 3);
      await controller.close();
      final retained = await File('${logs.path}/wire.jsonl').readAsString();
      expect(retained, contains(r'\"event\":\"[redacted]\"'));
      expect(retained, isNot(contains('new_messages')));
    } finally {
      await peer.close();
      await controller.close();
      await logs.delete(recursive: true);
    }
  });
  for (final op in [
    'set',
    'sync_now',
    'background',
    'state',
    'test_counters',
  ]) {
    test('mailbox $op result and error are wholly redacted', () async {
      final logs = await Directory.systemTemp.createTemp('m6-private-');
      final controller = await Controller.start(port: 0, logDir: logs);
      final peer = await MockParticipant.connect(controller);
      try {
        await peer.next();
        final participant = await controller.waitParticipant('node');
        final call = participant.call('mailbox_local_$op', {});
        final command = await peer.next();
        peer.result(command['id'] as int, {'value': privateMarker});
        expect(await call, {'value': privateMarker});
        final failure = expectLater(
          participant.call('mailbox_local_$op', {}),
          throwsA(
            isA<RpcError>()
                .having((e) => e.detail, 'detail', '[redacted]')
                .having((e) => e.op, 'op', 'mailbox_local_$op')
                .having((e) => e.participant, 'participant', 'node'),
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
  test('scenario passes when the phone gets the kept texts once', () async {
    final fixture = MailboxFixture();
    await fixture.start();
    waitMs.clear();
    try {
      final stages = <String>[];
      await runMailboxScenario(
        fixture.controller,
        fixture.ordered,
        airplane: fixture.airplane,
        foreground: fixture.foreground,
        evidence: (stage, data) => stages.add(stage),
        timeout: const Duration(seconds: 2),
      );
      expect(stages, [
        'linked',
        'mailbox_on',
        'phone_airplane',
        'mailbox_received',
        'phone_received',
      ]);
      expect(fixture.actions, ['airplane on', 'airplane off', 'foreground']);
      expect(fixture.texts['phone']!.length, 4);
      // Pass or fail, each deadline wait notes how long it took.
      expect(
        waitMs.keys,
        containsAll([
          'Mailbox scenario deadline: first_text',
          'account_local_pair_scan',
        ]),
      );
      waitMs.clear();
    } finally {
      await fixture.close();
    }
  });
  test('a failed pair scan notes both sides as fixed facts', () async {
    final fixture = MailboxFixture()..fault = 'pair_scan';
    await fixture.start();
    failureFacts.clear();
    try {
      await expectLater(
        runMailboxScenario(
          fixture.controller,
          fixture.ordered,
          airplane: fixture.airplane,
          foreground: fixture.foreground,
          timeout: const Duration(seconds: 2),
        ),
        throwsA(
          isA<RpcError>()
              .having((e) => e.code, 'code', 'AuthFailed')
              .having((e) => e.op, 'op', 'account_local_pair_scan'),
        ),
      );
      expect(failureFacts, {
        'owner_phase': 'Active',
        'owner_pair': 'none',
        'owner_publication': 'Observed',
        'owner_network': 'unknown',
        'scanner_network': 'unknown',
        'scanner_phase': 'Candidate',
        'scanner_pair': 'none',
        'scanner_publication': 'Observed',
      });
    } finally {
      failureFacts.clear();
      await fixture.close();
    }
  });
  test('a delivery deadline notes every side group as fixed facts', () async {
    final fixture = MailboxFixture()..fault = 'mailbox_missing';
    await fixture.start();
    failureFacts.clear();
    try {
      await expectLater(
        runMailboxScenario(
          fixture.controller,
          fixture.ordered,
          airplane: fixture.airplane,
          foreground: fixture.foreground,
          timeout: const Duration(seconds: 1),
        ),
        throwsA(
          isA<ScenarioDeadline>().having(
            (e) => e.message,
            'text',
            'Mailbox scenario deadline: first_text',
          ),
        ),
      );
      expect(failureFacts, {
        'm6_group_node': 'm2',
        'm6_epoch_node': 'e1',
        'm6_received_node': 'p0',
        'm6_deliveries_node': 'd1',
        'm6_holds_node': 'h0',
        'm6_attempted_node': 'r1',
        'm6_group_phone': 'm2',
        'm6_epoch_phone': 'e1',
        'm6_received_phone': 'p1',
        'm6_deliveries_phone': 'd0',
        'm6_holds_phone': 'h0',
        'm6_attempted_phone': 'r0',
        'm6_group_mailbox': 'none',
        'm6_epoch_mailbox': 'none',
        'm6_received_mailbox': 'p0',
        'm6_deliveries_mailbox': 'd0',
        'm6_holds_mailbox': 'h0',
        'm6_attempted_mailbox': 'r0',
      });
    } finally {
      failureFacts.clear();
      await fixture.close();
    }
  });
  for (final fault in ['duplicate', 'second_notification', 'not_isolated']) {
    test('scenario fails on $fault', () async {
      final fixture = MailboxFixture()..fault = fault;
      await fixture.start();
      try {
        await expectLater(
          runMailboxScenario(
            fixture.controller,
            fixture.ordered,
            airplane: fixture.airplane,
            foreground: fixture.foreground,
            timeout: const Duration(seconds: 2),
          ),
          throwsA(
            isA<StateError>().having(
              (e) => e.message,
              'code',
              startsWith('Mailbox scenario: '),
            ),
          ),
        );
      } finally {
        await fixture.close();
      }
    });
  }
}

// Orchestration fixture only: no native account, MLS, sync or network claim.
class MailboxFixture {
  late Directory logs;
  late Controller controller;
  final sockets = <WebSocket>[];
  final participants = <String, Participant>{};
  final actions = <String>[];
  String? fault;
  final phase = {
    'phone': 'Unconfigured',
    'mailbox': 'Unconfigured',
    'node': 'Unconfigured',
  };
  final offline = {'phone': false, 'mailbox': false, 'node': false};
  final texts = <String, List<Map<String, Object?>>>{
    'phone': [],
    'mailbox': [],
    'node': [],
  };
  final notifications = {'phone': 0, 'mailbox': 0, 'node': 0};
  final notified = {'phone': 0, 'mailbox': 0, 'node': 0};
  var paired = false;
  var confirmed = 0;
  var enabled = false;
  var lamport = 0;
  final conversation = List.filled(32, 5);
  List<int> peer(String name) => List.filled(32, name == 'node' ? 2 : 1);
  List<int> install(String name) =>
      List.filled(32, {'phone': 3, 'mailbox': 4, 'node': 6}[name]!);

  List<Participant> get ordered => [
    participants['phone']!,
    participants['mailbox']!,
    participants['node']!,
  ];

  Future<void> airplane(Participant p, bool on) async {
    expect(p.name, 'phone');
    actions.add('airplane ${on ? 'on' : 'off'}');
  }

  Future<void> foreground(Participant p) async {
    expect(p.name, 'phone');
    actions.add('foreground');
  }

  Future<void> start() async {
    logs = await Directory.systemTemp.createTemp('m6-scenario-');
    controller = await Controller.start(
      port: 0,
      logDir: logs,
      accountRecords: true,
    );
    for (final name in ['phone', 'mailbox', 'node']) {
      await join(name);
    }
  }

  /// Stores `text` on `name` the way a sync or a direct delivery would.
  void store(String name, Map<String, Object?> text, {bool notify = true}) {
    texts[name]!.add(text);
    if (notify) {
      notifications[name] = notifications[name]! + 1;
      notified[name] = notified[name]! + 1;
    }
  }

  void sync(String name) {
    if (name != 'phone' || offline['phone']!) return;
    final missing = texts['mailbox']!
        .where(
          (t) => !texts['phone']!.any(
            (m) => jsonEncode(m['key']) == jsonEncode(t['key']),
          ),
        )
        .toList();
    if (missing.isEmpty) return;
    texts['phone']!.addAll(missing);
    if (fault == 'duplicate') texts['phone']!.add(missing.first);
    notifications['phone'] = notifications['phone']! + 1;
    notified['phone'] = notified['phone']! + missing.length;
    if (fault == 'second_notification') {
      notifications['phone'] = notifications['phone']! + 1;
    }
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
                if (confirmed == 3) {'installation_id': install('mailbox')},
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
        if (name == 'mailbox' && confirmed == 3) phase[name] = 'Active';
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
        return {'ticket': offline[name]! ? null : 'ticket-$name'};
      case 'set_offline':
        offline[name] = args['offline'] as bool;
        return {'state': 'Online'};
      case 'mailbox_local_sync_now':
        sync(name);
      case 'mailbox_local_set':
        enabled = args['enabled'] as bool;
      case 'mailbox_local_state':
        return {'enabled': enabled};
      case 'mailbox_local_test_counters':
        return {
          'missing': 0,
          'notifications': notifications[name],
          'notified': notified[name],
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
          'state': 'Delivered',
        };
        texts['node']!.add(text);
        if (fault != 'mailbox_missing') store('mailbox', text);
        if (!offline['phone']! || fault == 'not_isolated') store('phone', text);
        return text;
      case 'message_local_test_group_facts':
        return {
          'group': name != 'mailbox',
          'epoch': name == 'mailbox' ? null : 1,
          'members': name == 'mailbox' ? 0 : 2,
          'processed_mls': name == 'phone' ? 1 : 0,
          'deliveries': name == 'node' ? 1 : 0,
          'holds': 0,
          'rejoins': 0,
          'attempted': args['key'] == null ? 0 : 1,
        };
      case 'message_local_load_messages':
        return {'revision': 1, 'items': texts[name], 'next': null};
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
      if (fault == 'pair_scan' && request['op'] == 'account_local_pair_scan') {
        socket.add(
          jsonEncode({
            'type': 'result',
            'id': request['id'],
            'ok': false,
            'error': {'code': 'AuthFailed', 'detail': privateMarker},
          }),
        );
        return;
      }
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
