import 'dart:async';
import 'dart:io';
import 'dart:convert';
import 'package:chat_e2e/src/controller.dart';
import 'package:chat_e2e/src/account_scenario.dart';
import 'package:chat_e2e/src/protocol.dart';
import 'package:test/test.dart';
import 'controller_test.dart' show MockParticipant;

// Public markers, never generated recovery words.
const marker = 'PUBLIC_MNEMONIC_QR_PSK_FIXTURE';
void main() {
  for (final changed in [false, true]) {
    test(
      'm3_restart_same_endpoint_m2_restart_different_m3_changed_$changed',
      () async {
        final fixture = AccountFixture(changedEndpoint: changed);
        await fixture.start();
        try {
          final run = fixture.run();
          if (changed) {
            await expectLater(
              run,
              throwsA(
                isA<StateError>().having(
                  (e) => e.message,
                  'code',
                  contains('M3 restart endpoint changed'),
                ),
              ),
            );
          } else {
            await run;
            expect(
              fixture.stages,
              containsAll([
                'pending_import_never_normal_sync',
                'paired_NewDisplays',
                'paired_ExistingDisplays',
                'same_peer_recovery',
                'recovery_restart',
              ]),
            );
            expect(fixture.restarts, 6);
          }
        } finally {
          await fixture.close();
        }
      },
    );
  }
  test(
    'pending_import_never_normal_sync_rejects_a_successful_normal_command',
    () async {
      final fixture = AccountFixture(allowPending: true);
      await fixture.start();
      try {
        await expectLater(
          fixture.run(),
          throwsA(
            isA<StateError>().having(
              (e) => e.message,
              'code',
              contains('pending import allowed normal account command'),
            ),
          ),
        );
      } finally {
        await fixture.close();
      }
    },
  );
  for (final mode in ['success', 'error', 'malformed', 'late']) {
    test(
      'controller_redacts_mnemonic_qr_psk_even_on_error_timeout_$mode',
      () async {
        final logs = await Directory.systemTemp.createTemp(
          'account-redaction-',
        );
        final controller = await Controller.start(port: 0, logDir: logs);
        final peer = await MockParticipant.connect(controller);
        await peer.next();
        final participant = await controller.waitParticipant('node');
        final call = participant.call('account_local_recover', {
          'words': marker,
        }, timeout: const Duration(milliseconds: 60));
        final assertion = mode == 'success'
            ? expectLater(call, completion(marker))
            : expectLater(call, throwsA(anything));
        final command = await peer.next();
        if (mode == 'error') {
          peer.send({
            'type': 'result',
            'id': command['id'],
            'ok': false,
            'error': {'code': 'InvalidConfig', 'detail': marker},
          });
        } else if (mode == 'malformed') {
          peer.socket.add('{"$marker');
        } else if (mode == 'success') {
          peer.result(command['id'] as int, marker);
        }
        await assertion;
        if (mode == 'late') {
          peer.result(command['id'] as int, marker);
          await Future<void>.delayed(const Duration(milliseconds: 30));
        }
        await peer.close();
        try {
          await controller.close();
        } catch (_) {
          /* expected protocol failure */
        }
        final recorded = await File('${logs.path}/wire.jsonl').readAsString();
        expect(recorded.contains(marker), false);
        await logs.delete(recursive: true);
      },
    );
  }
  test(
    'shared records are bounded, paused and cleaned with owned listener',
    () async {
      final logs = await Directory.systemTemp.createTemp('account-records-');
      final controller = await Controller.start(
        port: 0,
        logDir: logs,
        accountRecords: true,
      );
      Future<Map> request(Map<String, Object?> request) async {
        final socket = await WebSocket.connect(
          'ws://127.0.0.1:${controller.port}/records',
        );
        socket.add(jsonEncode(request));
        final reply = jsonDecode(await socket.first as String) as Map;
        await socket.close();
        return reply;
      }

      final packet = List<int>.filled(
        105,
        7,
      ); // Public transport fixture, not a signed-account validity proof.
      final peer = packet.take(32).toList();
      expect(await request({'op': 'lookup', 'peer': peer}), {'absent': true});
      expect(await request({'op': 'publish', 'packet': packet}), {'ok': true});
      expect((await request({'op': 'lookup', 'peer': peer}))['packet'], packet);
      controller.recordPublicationsPaused = true;
      final updated = [...packet]..[103] = 8;
      expect(await request({'op': 'publish', 'packet': updated}), {
        'unavailable': true,
      });
      expect((await request({'op': 'lookup', 'peer': peer}))['packet'], packet);
      controller.recordPublicationsPaused = false;
      expect(await request({'op': 'publish', 'packet': updated}), {'ok': true});
      expect(await request({'op': 'publish', 'packet': packet}), {
        'conflict': true,
      });
      expect(
        await request({'op': 'publish', 'packet': List<int>.filled(1001, 7)}),
        {'unavailable': true},
      );
      final port = controller.port;
      await controller.close();
      final reused = await HttpServer.bind(InternetAddress.loopbackIPv4, port);
      await reused.close(force: true);
      await logs.delete(recursive: true);
    },
  );

  test('account error toString is safe for structured failure diagnostics', () {
    expect(
      const RpcError('InvalidConfig', marker).toString(),
      isNot(contains(marker)),
    );
  });
}

// Scenario control-flow fixture only. Native stores, signatures and approval
// authority are separately exercised by the real two-app Android run.
class AccountFixture {
  AccountFixture({this.changedEndpoint = false, this.allowPending = false});
  final bool changedEndpoint, allowPending;
  late Directory logs;
  late Controller controller;
  final sockets = <WebSocket>[];
  final states = <String, Map<String, Object?>>{};
  final peers = <String, Participant>{};
  final stores = {'a': 0, 'b': 0};
  final stages = <String>[];
  var restarts = 0;
  var serial = 0;
  List<int> transport(String name) => [
    name == 'a' ? 1 : 2,
    stores[name]!,
    ...List.filled(30, 0),
  ];
  Map<String, Object?> device(String name) => {
    'installation_id': install(name),
    'transport_public_key': transport(name),
  };
  List<int> install(String name) => [name == 'a' ? 1 : 2, stores[name]!];
  Map<String, Object?> fresh(String name) => {
    'phase': 'Unconfigured',
    'public_peer_id': null,
    'installation': null,
    'devices': <Object?>[],
    'contacts': <Object?>[],
    'pair': null,
    'pending_pair_transactions': <Object?>[],
  };
  Future<void> start() async {
    logs = await Directory.systemTemp.createTemp('m3-scenario-');
    controller = await Controller.start(port: 0, logDir: logs);
    for (final name in ['a', 'b']) {
      states[name] = fresh(name);
      await join(name);
    }
  }

  Future<Participant> join(String name) async {
    final socket = await WebSocket.connect('ws://127.0.0.1:${controller.port}');
    sockets.add(socket);
    socket.listen((raw) {
      final wire = jsonDecode(raw as String) as Map;
      if (wire['type'] != 'cmd') return;
      final op = wire['op'] as String;
      final args = wire['args'] as Map;
      final state = states[name]!;
      if (state['phase'] == 'PendingImport' &&
          [
            'account_local_contacts',
            'account_local_connect_account',
            'account_local_create_invite',
          ].contains(op) &&
          !allowPending) {
        socket.add(
          jsonEncode({
            'type': 'result',
            'id': wire['id'],
            'ok': false,
            'error': {'code': 'AccountRestricted', 'detail': 'public fixture'},
          }),
        );
        return;
      }
      Object? result;
      switch (op) {
        case 'network_info':
          result = {
            'endpoint_id': state['installation'] == null
                ? null
                : transport(name)
                      .map((b) => b.toRadixString(16).padLeft(2, '0'))
                      .join()
                      .replaceRange(
                        62,
                        64,
                        changedEndpoint && restarts == 2 ? 'ff' : '00',
                      ),
            'ticket': 'public-fixture-ticket',
          };
        case 'account_local_snapshot':
          result = state;
        case 'account_local_begin_create':
          result = {
            'flow': [0],
            'words': List.generate(24, (i) => 'public$i'),
            'positions': [1, 2, 3],
          };
        case 'account_local_confirm_create':
        case 'account_local_recover':
          state.addAll({
            'phase': 'Active',
            'public_peer_id': op.endsWith('recover')
                ? [1]
                : [name == 'a' ? 1 : 2],
            'installation': install(name),
            'devices': [device(name)],
          });
        case 'account_local_create_invite':
          result = 'public-fixture-invite';
        case 'account_local_accept_invite':
          result = {'verified': false};
          state['contacts'] = [result];
        case 'account_local_safety':
          result = {
            'digest': [1],
          };
        case 'account_local_confirm_safety':
          state['contacts'] = [
            {'verified': true},
          ];
        case 'account_local_contacts':
          result = state['contacts'];
        case 'account_local_prepare_candidate':
          state.addAll({'phase': 'Candidate', 'installation': install(name)});
        case 'account_local_pair_begin':
          result = {
            'flow': [1],
            'qr': marker,
            'expires_at': 123,
          };
        case 'account_local_route':
          break;
        case 'account_local_pair_scan':
          for (final s in states.values) {
            s['pair'] = {'sas': 'PUBLICSAS', 'phase': 'AwaitingApproval'};
          }
          result = [1];
        case 'account_local_pair_confirm_code':
          break;
        case 'account_local_pair_approve':
          states['b']!['phase'] = controller.recordPublicationsPaused
              ? 'PendingImport'
              : 'Active';
          states['b']!['public_peer_id'] = [1];
          states['b']!['pending_pair_transactions'] = [
            [1],
          ];
          (states['a']!['devices'] as List).add(device('b'));
        case 'account_local_refresh_record':
          if (name == 'b' && state['phase'] == 'PendingImport') {
            state['phase'] = 'Active';
          }
        case 'account_local_revoke_device':
          (state['devices'] as List).removeWhere(
            (v) =>
                jsonEncode((v as Map)['installation_id']) ==
                jsonEncode(args['target']),
          );
        default:
          throw StateError('Unexpected fixture operation');
      }
      for (final current in states.values) {
        current['publication'] = {
          'state': 'Observed',
          'accepted': {'devices': current['devices']},
        };
      }
      socket.add(
        jsonEncode({
          'type': 'result',
          'id': wire['id'],
          'ok': true,
          'value': result,
        }),
      );
    });
    socket.add(
      jsonEncode({
        'type': 'hello',
        'protocol': puppetProtocol,
        'name': name,
        'instance': 'fixture-${++serial}',
        'relay': localRelay,
      }),
    );
    final next = await controller.waitParticipant(name);
    // The old participant may still be installed until the new hello is read.
    while (peers[name]?.instance == next.instance) {
      await Future<void>.delayed(const Duration(milliseconds: 1));
      final current = await controller.waitParticipant(name);
      if (current.instance != next.instance) {
        peers[name] = current;
        return current;
      }
    }
    peers[name] = next;
    return next;
  }

  Future<void> run() => runAccountScenario(
    controller,
    [peers['a']!, peers['b']!],
    restart: (old, {bool freshStore = false}) async {
      restarts++;
      if (freshStore) {
        stores[old.name] = stores[old.name]! + 1;
        states[old.name] = fresh(old.name);
      }
      return join(old.name);
    },
    evidence: (stage, _) => stages.add(stage),
    timeout: const Duration(seconds: 2),
  );
  Future<void> close() async {
    for (final socket in sockets) {
      await socket.close();
    }
    await controller.close();
    await logs.delete(recursive: true);
  }
}
