import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:chat_e2e/src/controller.dart';
import 'package:chat_e2e/src/message_scenario.dart';
import 'package:chat_e2e/src/protocol.dart';
import 'package:test/test.dart';
import 'controller_test.dart' show MockParticipant;

const privateMarker = 'PRIVATE_M4_BODY_AND_PHRASE_SENTINEL';

void main() {
  test('controlled time is opt in and requires local records', () async {
    final logs = await Directory.systemTemp.createTemp('m4-config-');
    try {
      await expectLater(
        Controller.start(port: 0, logDir: logs, controlledTime: true),
        throwsArgumentError,
      );
      final controller = await Controller.start(
        port: 0,
        logDir: logs,
        accountRecords: true,
        controlledTime: true,
      );
      final peer = await MockParticipant.connect(controller);
      expect(await peer.next(), {
        'type': 'config',
        'relay': localRelay,
        'records': 'ws://127.0.0.1:47000/records',
        'controlled_time': true,
      });
      await peer.close();
      await controller.close();
    } finally {
      await logs.delete(recursive: true);
    }
  });
  for (final scenario in ['m2', 'm3', 'm3-auth', 'm4']) {
    test('CLI admits $scenario and preserves failure cleanup', () async {
      final root = await Directory.systemTemp.createTemp('m4-cli-');
      try {
        final result = await Process.run(Platform.resolvedExecutable, [
          'run',
          'bin/run.dart',
          '--profile',
          'windows',
          '--scenario',
          scenario,
          '--repo-root',
          root.path,
        ]);
        expect(result.exitCode, Platform.isWindows ? 1 : 64);
        expect(result.stdout, isNot(contains('PASS')));
        if (Platform.isWindows) {
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
  for (final mode in [(false, false), (true, false), (false, true)]) {
    final (loseStore, reuseFreshStore) = mode;
    test(
      'restart scenario validates persistence loseStore=$loseStore reuseFreshStore=$reuseFreshStore',
      () async {
        final fixture = MessageFixture(loseStore: loseStore);
        await fixture.start();
        try {
          var recoveryAudits = 0;
          final scenario = runMessageScenario(
            fixture.controller,
            fixture.participants.values.toList(),
            auditRecovery: (patterns) {
              recoveryAudits++;
              expect(patterns.length, 5);
              expect(
                patterns.every((value) => value.contains(privateMarker)),
                isTrue,
              );
              expect(fixture.active.values.any((value) => value), isTrue);
              return 1;
            },
            restart: (old, {bool freshStore = false}) async {
              if (freshStore) {
                fixture.freshRestarts++;
                if (reuseFreshStore) return fixture.join(old.name);
                fixture.generations[old.name] =
                    fixture.generations[old.name]! + 1;
                fixture.active[old.name] = false;
                fixture.contacts[old.name] = false;
                fixture.rows.clear();
                fixture.received.clear();
                return fixture.join(old.name);
              }
              if (fixture.restarts == 0) expect(fixture.offline['a'], isTrue);
              fixture.restarts++;
              expect(fixture.rows.length, fixture.freshRestarts == 2 ? 1 : 2);
              if (loseStore) fixture.rows.removeLast();
              return fixture.join(old.name);
            },
            evidence: (stage, data) => fixture.stages.add(stage),
            timeout: const Duration(milliseconds: 350),
          );
          if (loseStore || reuseFreshStore) {
            await expectLater(scenario, throwsA(isA<TimeoutException>()));
          } else {
            await scenario;
            expect(recoveryAudits, 4);
            expect(fixture.freshRestarts, 2);
            expect(fixture.restarts, 2);
            expect(fixture.stages, contains('receiver_commit_progress'));
            expect(
              fixture.stages.where(
                (stage) =>
                    stage != 'receiver_commit_progress' &&
                    stage != 'recovery_privacy_pass',
              ),
              [
                'contact_block_then_explicit_retry',
                'fresh_owned_stores_for_lost_ack',
                'restart_same_store_keeps_queued_message',
                'lost_ack_before_owned_kill',
                'native_payload_retransmit_deduplicated',
                'durable_ack_and_read',
              ],
            );
            final log = await File(
              '${fixture.logs.path}/wire.jsonl',
            ).readAsString();
            expect(log, isNot(contains('M4_PRIVATE')));
            expect(log, isNot(contains(privateMarker)));
          }
        } finally {
          await fixture.close();
        }
      },
    );
  }
  for (final confirmFails in [false, true]) {
    test(
      'recovery audit failure clears patterns after confirm failure=$confirmFails',
      () async {
        final fixture = MessageFixture(loseStore: false)
          ..failConfirm = confirmFails;
        await fixture.start();
        List<String>? retainedPublicFixturePatterns;
        try {
          await expectLater(
            runMessageScenario(
              fixture.controller,
              fixture.participants.values.toList(),
              restart: (old, {bool freshStore = false}) async => old,
              auditRecovery: (patterns) {
                expect(fixture.confirmAttempts, 1);
                retainedPublicFixturePatterns = patterns;
                expect(patterns.length, 5);
                throw StateError('Recovery privacy audit failed');
              },
            ),
            throwsA(
              isA<StateError>().having(
                (e) => e.message,
                'fixed error',
                'Recovery privacy audit failed',
              ),
            ),
          );
          expect(retainedPublicFixturePatterns != null, isTrue);
          expect(retainedPublicFixturePatterns?.isEmpty, isTrue);
        } finally {
          await fixture.close();
        }
      },
    );
  }
  for (final op in [
    'open_dm',
    'list_conversations',
    'load_messages',
    'send_text',
    'cancel_send',
    'retry_send',
    'mark_read',
    'test_ack_loss_arm',
    'test_ack_loss_wait',
    'test_ack_loss_release',
    'test_write_counts',
  ]) {
    test('message $op result and error are wholly redacted', () async {
      final logs = await Directory.systemTemp.createTemp('m4-private-');
      final controller = await Controller.start(port: 0, logDir: logs);
      final peer = await MockParticipant.connect(controller);
      try {
        await peer.next();
        final participant = await controller.waitParticipant('node');
        final call = participant.call('message_local_$op', {
          'body': privateMarker,
        });
        final command = await peer.next();
        peer.result(command['id'] as int, {'body': privateMarker});
        expect(await call, {'body': privateMarker});
        final failure = expectLater(
          participant.call('message_local_$op', {}),
          throwsA(
            isA<RpcError>().having(
              (e) => e.detail == '[redacted]',
              'safe detail',
              true,
            ),
          ),
        );
        final next = await peer.next();
        peer.send({
          'type': 'result',
          'id': next['id'],
          'ok': false,
          'error': {'code': 'InvalidMessage', 'detail': privateMarker},
        });
        await failure;
        await controller.close();
        final retained = await File('${logs.path}/wire.jsonl').readAsString();
        expect(retained, isNot(contains(privateMarker)));
        final replies = const LineSplitter()
            .convert(retained)
            .map((line) => jsonDecode(line) as Map)
            .where((line) => line['direction'] == 'received')
            .map((line) => jsonDecode(line['wire'] as String) as Map)
            .where((wire) => wire['type'] == 'result');
        expect(
          replies.map((wire) => wire['payload']),
          everyElement('[redacted]'),
        );
      } finally {
        await peer.close();
        await controller.close();
        await logs.delete(recursive: true);
      }
    });
  }
  test('RPC timeout terminates the run before a late result', () async {
    final logs = await Directory.systemTemp.createTemp('m4-timeout-');
    final controller = await Controller.start(port: 0, logDir: logs);
    final peer = await MockParticipant.connect(controller);
    try {
      await peer.next();
      final participant = await controller.waitParticipant('node');
      await expectLater(
        participant.call(
          'message_local_load_messages',
          {},
          timeout: const Duration(milliseconds: 20),
        ),
        throwsA(isA<TimeoutException>()),
      );
      await expectLater(
        participant.call('network_info', {}),
        throwsA(isA<TimeoutException>()),
      );
      await expectLater(controller.close(), throwsA(isA<TimeoutException>()));
    } finally {
      await peer.close();
      try {
        await controller.close();
      } catch (_) {}
      await logs.delete(recursive: true);
    }
  });
}

// Orchestration fixture only: no native SQL, cryptography or network-ACK claim.
class MessageFixture {
  MessageFixture({this.loseStore = false});
  final bool loseStore;
  late Directory logs;
  late Controller controller;
  final sockets = <WebSocket>[];
  final participants = <String, Participant>{};
  final stages = <String>[];
  final offline = {'a': false, 'b': false};
  final networkStarted = {'a': false, 'b': false};
  final active = {'a': false, 'b': false};
  final contacts = {'a': false, 'b': false};
  final rows = <Map<String, Object?>>[];
  var serial = 0;
  var restarts = 0;
  var freshRestarts = 0;
  var failConfirm = false;
  var confirmAttempts = 0;
  final generations = {'a': 0, 'b': 0};
  var held = false;
  var consumed = false;
  final payloadWrites = {'a': 0, 'b': 0};
  final received = <String>{};
  List<int> peer(String name) =>
      List.filled(32, (name == 'a' ? 1 : 2) + 10 * generations[name]!);
  List<int> install(String name) => List.filled(16, name == 'a' ? 3 : 4);
  Future<void> start() async {
    logs = await Directory.systemTemp.createTemp('m4-scenario-');
    controller = await Controller.start(
      port: 0,
      logDir: logs,
      accountRecords: true,
      controlledTime: true,
    );
    await join('a');
    await join('b');
  }

  Future<Participant> join(String name) async {
    payloadWrites[name] = 0;
    networkStarted[name] = false;
    final socket = await WebSocket.connect('ws://127.0.0.1:${controller.port}');
    sockets.add(socket);
    socket.listen((raw) {
      final request = jsonDecode(raw as String) as Map;
      if (request['type'] != 'cmd') return;
      final args = request['args'] as Map;
      Object? result;
      switch (request['op']) {
        case 'account_local_snapshot':
          result = {
            'phase': active[name]! ? 'Active' : 'Unconfigured',
            'public_peer_id': active[name]! ? peer(name) : null,
            'installation': active[name]! ? install(name) : null,
            'contacts': contacts.values.every((v) => v)
                ? [
                    {'device_unverified': false},
                  ]
                : [],
          };
        case 'account_local_begin_create':
          result = {
            'flow': List.filled(16, 9),
            'words': List.filled(24, privateMarker),
            'positions': [1, 2, 3],
          };
        case 'account_local_confirm_create':
          confirmAttempts++;
          if (failConfirm) {
            socket.add(
              jsonEncode({
                'type': 'result',
                'id': request['id'],
                'ok': false,
                'error': {'code': 'NotAvailable', 'detail': privateMarker},
              }),
            );
            return;
          }
          active[name] = true;
          networkStarted[name] = true;
        case 'account_local_refresh_record':
          break;
        case 'account_local_create_invite':
          result = privateMarker;
        case 'account_local_accept_invite':
          contacts[name] = true;
          result = {'verified': false};
        case 'network_start':
          networkStarted[name] = true;
        case 'network_info':
          result = {
            'ticket': networkStarted[name]! ? 'fixture-ticket' : null,
            'endpoint_id': 'fixture-endpoint',
          };
        case 'account_local_route':
          expect(args['ticket'], 'fixture-ticket');
        case 'account_local_connect_account':
          break;
        case 'message_local_test_ack_loss_arm':
          held = true;
        case 'message_local_test_ack_loss_wait':
          expect(consumed, isTrue);
          result = {'response_checkpoint': true, 'receipt_checkpoint': true};
        case 'message_local_test_ack_loss_release':
          held = false;
        case 'message_local_test_write_counts':
          result = {
            'payload_writes': payloadWrites[name],
            'response_writes': 0,
          };
        case 'message_local_open_dm':
          result = {'unread': rows.length, 'id': List.filled(32, 5)};
        case 'set_offline':
          offline[name] = args['offline'] as bool;
        case 'clock_advance':
          final seconds = args['seconds'] as int;
          expect(seconds, greaterThanOrEqualTo(1));
          // A restored outbox can retain a second-attempt 2s backoff after
          // acquiring while its reopened transport was still stopped.
          if (name == 'a' && seconds >= 2) {
            if (!offline.values.any((v) => v) &&
                contacts.values.every((v) => v)) {
              for (final row in rows) {
                if (row['state'] == 'Queued' || row['state'] == 'SentDirect') {
                  payloadWrites[name] = payloadWrites[name]! + 1;
                  received.add(jsonEncode(row['key']));
                  if (held && !consumed) {
                    row['state'] = 'SentDirect';
                    consumed = true;
                  } else {
                    row['state'] = 'Delivered';
                  }
                }
              }
            }
          }
          result = {'unix_seconds': 1800000001, 'monotonic_ms': 1000};
        case 'message_local_send_text':
          final existing = rows.where(
            (r) =>
                jsonEncode((r['key'] as Map)['id']) == jsonEncode(args['id']),
          );
          if (existing.isNotEmpty) {
            result = existing.single;
            break;
          }
          final row = <String, Object?>{
            'key': {
              'conversation': List.filled(32, 5),
              'sender': peer(name),
              'installation': install(name),
              'id': args['id'],
            },
            'body': args['body'],
            'lamport': rows.length + 1,
            'sent_at_ms': 1800000000000,
            'outgoing': true,
            'state': offline.values.any((v) => v)
                ? 'Queued'
                : contacts['b']!
                ? 'Delivered'
                : 'Blocked',
            'cancel_requested': false,
            'block': contacts['b']! ? null : 'ContactRequired',
          };
          rows.add(row);
          result = row;
        case 'message_local_load_messages':
          result = {
            'revision': rows.length,
            'items': rows
                .where(
                  (r) =>
                      name == 'a' ||
                      r['state'] == 'Delivered' ||
                      r['state'] == 'Read' ||
                      received.contains(jsonEncode(r['key'])),
                )
                .toList(),
            'next': null,
          };
        case 'message_local_retry_send':
          expect(contacts['b'], isTrue);
          rows.first['state'] = 'Delivered';
          result = rows.first;
        case 'message_local_mark_read':
          expect((args['keys'] as List).length, 1);
          rows.last['state'] = 'Read';
        default:
          throw StateError('Unexpected fixture operation');
      }
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
        'instance': '${++serial}',
        'protocol': puppetProtocol,
        'relay': localRelay,
      }),
    );
    Participant? next;
    do {
      next = await controller.waitParticipant(name);
      if (next.instance == '$serial') break;
      await Future<void>.delayed(Duration.zero);
    } while (true);
    return participants[name] = next;
  }

  Future<void> close() async {
    for (final socket in sockets) {
      await socket.close();
    }
    await controller.close();
    await logs.delete(recursive: true);
  }
}
