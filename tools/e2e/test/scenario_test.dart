import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:mirrors';

import '../bin/run.dart' as harness;

import 'package:chat_e2e/src/controller.dart';
import 'package:chat_e2e/src/owned_resources.dart';
import 'package:chat_e2e/src/protocol.dart';
import 'package:chat_e2e/src/scenario.dart';
import 'package:test/test.dart';

// The wire fake replaces only external cores; command matching, event queues,
// sequence checks and diagnostics use the real Controller and scenario.
class Fixture {
  final peers = <String, FakeCore>{};
  final commands = <(String, String, Map<String, dynamic>)>[];
  late Controller controller;
  late Directory logs;
  String? fault;
  var restarted = false;
  String? failedText;
  (String, String)? suppressReply;
  String? suppressStop;
  final stopping = Completer<void>();
  Duration recoveryReplyDelay = Duration.zero;
  Timer? recoveryReplyTimer;

  Future<List<Participant>> start() async {
    logs = await Directory.systemTemp.createTemp('scenario-');
    controller = await Controller.start(port: 0, logDir: logs);
    for (final name in ['windows', 'android', 'node']) {
      await join(name, '$name-1');
    }
    return Future.wait(peers.keys.map(controller.waitParticipant));
  }

  Future<void> join(String name, String instance) async {
    final socket = await WebSocket.connect('ws://127.0.0.1:${controller.port}');
    final core = FakeCore(this, name, instance, socket);
    peers[name] = core;
    socket.listen(core.receive);
    socket.add(
      jsonEncode({
        'type': 'hello',
        'protocol': 'chat-test-hooks:v1',
        'name': name,
        'instance': instance,
        'relay': 'http://127.0.0.1:3340',
      }),
    );
    await controller.waitParticipant(name);
  }

  Future<Participant> restart(Participant old) async {
    restarted = true;
    if (fault == 'replacement RPC') suppressReply = ('node', 'network_start');
    if (fault == 'late replay') {
      peers['windows']!.event({
        'kind': 'probe_text_received',
        'peer_id': 'android-1-id',
        'text': failedText,
      });
    }
    // Replacement hello is intentionally delivered before old socket close.
    await join(
      old.name,
      fault == 'same instance' ? old.instance : '${old.name}-2',
    );
    var replacement = await controller.waitParticipant(old.name);
    while (replacement.instance == old.instance && fault != 'same instance') {
      await Future<void>.delayed(Duration.zero);
      replacement = await controller.waitParticipant(old.name);
    }
    return replacement;
  }

  Future<void> close() async {
    recoveryReplyTimer?.cancel();
    try {
      await controller.close();
    } catch (_) {
      /* asserted by scenario */
    }
    for (final core in peers.values) {
      await core.socket.close();
    }
    await logs.delete(recursive: true);
  }
}

class FakeCore {
  FakeCore(this.fixture, this.name, this.instance, this.socket);
  final Fixture fixture;
  final String name, instance;
  final WebSocket socket;
  final connected = <String>{};
  var offline = false;
  var starting = false;
  var restored = false;
  var revision = 1;
  var seq = 0;
  String get endpoint =>
      fixture.fault == 'same endpoint' && fixture.restarted && name == 'node'
      ? 'node-1-id'
      : '$instance-id';
  Map<String, Object?> get info => {
    'state': offline
        ? 'Stopped'
        : starting
        ? 'Starting'
        : 'Online',
    'revision': revision,
    'endpoint_id': endpoint,
    'ticket': '$endpoint-ticket',
    'peers': connected
        .map(
          (id) => {
            'peer_id': id,
            'phase':
                fixture.peers.values.any(
                  (p) => p.endpoint == id && (p.offline || p.starting),
                )
                ? 'Reconnecting'
                : 'Connected',
            'path': fixture.fault == 'unknown path' ? 'Unknown' : 'Direct',
            'connect_ms': 1,
            'direct_ms': 1,
          },
        )
        .toList(),
  };
  void event(Map<String, Object?> event) {
    seq += fixture.fault == 'sequence' ? 2 : 1;
    socket.add(
      jsonEncode({
        'type': 'event',
        'instance': instance,
        'seq': seq,
        'event': event,
      }),
    );
  }

  void receive(dynamic raw) {
    final record = jsonDecode(raw as String) as Map<String, dynamic>;
    if (record['type'] != 'cmd') return;
    final op = record['op'] as String;
    final args = record['args'] as Map<String, dynamic>;
    fixture.commands.add((name, op, args));
    if (fixture.suppressReply == (name, op)) return;
    if (op == 'network_stop' && fixture.suppressStop == name) {
      if (!fixture.stopping.isCompleted) fixture.stopping.complete();
      return;
    }
    Object? value;
    String? error;
    switch (op) {
      case 'network_start':
        if (fixture.fault == 'missing Online') starting = true;
        value = info;
        event({'kind': 'network_changed', ...info});
      case 'network_info':
        value = info;
      case 'connect_peer':
        final remote = fixture.peers.values.singleWhere(
          (p) => '${p.endpoint}-ticket' == args['target'],
        );
        if (fixture.fault == 'recovery ordering' &&
            remote.restored &&
            connected.contains(remote.endpoint)) {
          error = 'Busy';
          break;
        }
        connected.add(remote.endpoint);
        remote.connected.add(endpoint);
        value = {
          'peer_id': remote.endpoint,
          'phase': 'Connected',
          'path': 'Direct',
        };
      case 'set_offline':
        offline = args['offline'] as bool;
        revision++;
        if (offline && fixture.fault != 'missing offline') {
          for (final peer in fixture.peers.values.where(
            (p) => p.connected.contains(endpoint),
          )) {
            peer.event({
              'kind': 'peer_changed',
              'revision': ++peer.revision,
              'peer': {
                'peer_id': endpoint,
                'phase': 'Reconnecting',
                'path': 'Unknown',
              },
            });
          }
        } else if (fixture.fault == 'recovery ordering') {
          restored = true;
          starting = true;
          Timer(const Duration(milliseconds: 10), () {
            starting = false;
            revision++;
            event({'kind': 'network_changed', ...info});
            for (final peer in fixture.peers.values.where(
              (p) => p.connected.contains(endpoint),
            )) {
              peer.event({
                'kind': 'peer_changed',
                'revision': ++peer.revision,
                'peer': {
                  'peer_id': endpoint,
                  'phase': 'Connected',
                  'path': 'Direct',
                },
              });
            }
          });
        }
        value = info;
      case 'disconnect_peer':
        if (fixture.fault != 'retry intention') {
          connected.remove(args['peer_id']);
        }
      case 'network_stop':
        connected.clear();
      case 'send_probe_text':
        if (starting) {
          error = 'Busy';
        } else if (offline || fixture.fault == 'command') {
          error = 'PeerNotConnected';
          if (offline) fixture.failedText = args['text'] as String;
          if (offline && fixture.fault == 'replay') {
            final remote = fixture.peers.values.singleWhere(
              (p) => p.endpoint == args['peer_id'],
            );
            remote.event({
              'kind': 'probe_text_received',
              'peer_id': endpoint,
              'text': args['text'],
            });
          }
        } else {
          final remote = fixture.peers.values.singleWhere(
            (p) => p.endpoint == args['peer_id'],
          );
          if (fixture.fault == 'missing selected probe' && name == 'android' && remote.name == 'node') break;
          remote.event({
            'kind': 'probe_text_received',
            'peer_id': fixture.fault == 'wrong peer' ? 'impostor' : endpoint,
            'text': fixture.fault == 'wrong body' ? 'unrelated' : args['text'],
          });
        }
      default:
        throw StateError('Unexpected command $op');
    }
    final reply = jsonEncode(
      error == null
          ? {'type': 'result', 'id': record['id'], 'ok': true, 'value': value}
          : {
              'type': 'result',
              'id': record['id'],
              'ok': false,
              'error': {'code': error, 'detail': 'fixture failure'},
            },
    );
    if (op == 'set_offline' && args['offline'] == false &&
        fixture.recoveryReplyDelay > Duration.zero) {
      fixture.recoveryReplyTimer = Timer(fixture.recoveryReplyDelay, () => socket.add(reply));
    } else {
      socket.add(reply);
    }
  }
}

void main() {
  const short = Duration(milliseconds: 40);
  for (final entry in <(String?, (String, String)?, String?, String, String)>[
    (null, ('android', 'network_start'), 'windows', 'scenario_app2', 'timeout_network_start'),
    (null, ('windows', 'network_info'), null, 'scenario_app1', 'timeout_network_info'),
    (null, ('android', 'connect_peer'), null, 'scenario_app2', 'timeout_connect_peer'),
    (null, ('android', 'send_probe_text'), null, 'scenario_app2', 'timeout_send_probe_text'),
    (null, ('android', 'set_offline'), null, 'scenario_app2', 'timeout_set_offline'),
    (null, ('android', 'disconnect_peer'), null, 'scenario_app2', 'timeout_disconnect_peer'),
    ('replacement RPC', null, null, 'scenario_node', 'timeout_network_start'),
    ('missing selected probe', null, null, 'scenario_app2_to_node', 'timeout_probe_event'),
    ('wrong body', null, null, 'scenario_app1_to_app2', 'timeout_probe_event'),
    ('unknown path', null, null, 'scenario_app2_to_app1', 'timeout_state_event'),
    ('missing Online', null, null, 'scenario_app1', 'timeout_state_event'),
    ('missing offline', null, null, 'scenario_app2_to_app1', 'timeout_offline_event'),
    (null, null, 'node', 'scenario_node', 'timeout_network_stop'),
  ]) {
    test('origin ${entry.$4}/${entry.$5} survives final stops', () async {
      final fixture = Fixture()
        ..fault = entry.$1
        ..suppressReply = entry.$2
        ..suppressStop = entry.$3;
      final participants = await fixture.start();
      final library =
          (reflect(harness.main) as ClosureMirror).function.owner
              as LibraryMirror;
      final type =
          library.declarations[MirrorSystem.getSymbol('_Run', library)]
              as ClassMirror;
      final run = type.newInstance(const Symbol(''), [fixture.logs]);
      final runner = run.reflectee as dynamic;
      final phases =
          (library.declarations[MirrorSystem.getSymbol('_Phase', library)]
                      as ClassMirror)
                  .getField(const Symbol('values'))
                  .reflectee
              as List;
      run.setField(
        const Symbol('phase'),
        phases.singleWhere((v) => v.value == 'scenario'),
      );
      Object? original;
      StackTrace? originalStack;
      final captures = <Object>[];
      void capture(Object error, String? failurePhase, String? timeoutKind) {
        captures.add(error);
        runner.recordFailure(
          error,
          failurePhase: failurePhase,
          timeoutKind: timeoutKind,
        );
      }

      final work = runScenario(
        fixture.controller,
        participants,
        restart: fixture.restart,
        timeout: short,
        onFailure: capture,
      );
      final observed = work.catchError((Object error, StackTrace stack) {
        original = error;
        originalStack = stack;
        Error.throwWithStackTrace(error, stack);
      });
      final awaited = runner
          .wait(observed)
          .then<void>(
            (_) => fail('expected timeout'),
            onError: (Object error, StackTrace stack) {
              expect(error, same(original));
              expect(stack.toString(), originalStack.toString());
              expect(error, isA<TimeoutException>());
            },
          );
      try {
        if (entry.$3 == 'windows') {
          await awaited;
          // An RPC timeout poisons the controller; cleanup owns process stops
          // and must not resume this protocol to issue network_stop commands.
          expect(fixture.stopping.isCompleted, isFalse);
          expect(run.getField(#safeFailure).reflectee, {
            'phase': entry.$4,
            'failure_kind': entry.$5,
            'command_exit_code': null,
          });
          runner.recordFailure(const FileSystemException('PRIVATE_TOKEN'));
          runner.recordFailure(
            TimeoutException('monitor'),
            failurePhase: 'android_pid_monitor',
          );
          runner.recordFailure(
            TimeoutException('outer'),
            failurePhase: 'scenario',
          );
        }
        await awaited;
        expect(run.getField(#safeFailure).reflectee, {
          'phase': entry.$4,
          'failure_kind': entry.$5,
          'command_exit_code': null,
        });
        expect(captures, [same(original)]);
        expect(originalStack.toString(), contains('scenario.dart'));
      } finally {
        await awaited;
        await fixture.close();
      }
    });
  }

  test(
    'failed inherited drain retains ownership but never blocks remaining cleanup',
    () async {
      final logs = await Directory.systemTemp.createTemp(
        'scenario-stalled-lifetime-',
      );
      final library =
          (reflect(harness.main) as ClosureMirror).function.owner
              as LibraryMirror;
      final type =
          library.declarations[MirrorSystem.getSymbol('_Run', library)]
              as ClassMirror;
      final runner =
          type.newInstance(const Symbol(''), [logs]).reflectee as dynamic;
      final release = File('${logs.path}/release');
      final source = File('${logs.path}/inherited.dart')
        ..writeAsStringSync('''
import 'dart:async';
import 'dart:io';
Future<void> main(List<String> args) async {
  if (args[0] == 'parent') {
    await Process.start(Platform.resolvedExecutable,
      [Platform.script.toFilePath(), 'descendant', args[1]],
      mode: ProcessStartMode.inheritStdio);
    exit(0);
  }
  stdout.writeln('descendant retains output');
  final deadline = DateTime.now().add(const Duration(seconds: 30));
  while (!File(args[1]).existsSync() && DateTime.now().isBefore(deadline)) {
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
}
''');
      final controller = await Controller.start(
        port: 0,
        logDir: Directory('${logs.path}/controller'),
      );
      final holder = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
      final controllerPort = controller.port, holderPort = holder.port;
      runner.owned.add('controller', controller.close);
      runner.owned.add('socket', () async {
        await holder.close();
      });
      final command = runner.command(Platform.resolvedExecutable, [
        source.path,
        'parent',
        release.path,
      ], timeout: const Duration(seconds: 1));
      try {
        await expectLater(
          command,
          throwsA(
            isA<TimeoutException>().having(
              (e) => e.duration,
              'primary command deadline',
              const Duration(seconds: 1),
            ),
          ),
        );
        await runner.stopMonitors();
        // Safety deadlines cannot satisfy the following production-result checks.
        await runner.unwind().timeout(const Duration(seconds: 2));
        final failures = await runner.owned.close().timeout(
          const Duration(seconds: 2),
        );
        expect(failures, hasLength(1));
        expect(
          failures.single,
          contains('TimeoutException after 0:00:05.000000'),
        );
        expect(
          runner.cleanupFailures,
          contains(contains('command stop: TimeoutException')),
        );
        await expectLater(
          Socket.connect(InternetAddress.loopbackIPv4, controllerPort),
          throwsA(isA<SocketException>()),
        );
        await expectLater(
          Socket.connect(InternetAddress.loopbackIPv4, holderPort),
          throwsA(isA<SocketException>()),
        );
        final record =
            jsonDecode(File('${logs.path}/ownership.json').readAsStringSync())
                as Map;
        expect(record['processes'], hasLength(1));
        final owned = runner.active.single;
        final stopping = owned.stop();
        expect(owned.stop(), same(stopping));
        await expectLater(
          stopping,
          throwsA(
            isA<TimeoutException>().having(
              (e) => e.duration,
              'cached drain failure',
              const Duration(seconds: 5),
            ),
          ),
        );
        await expectLater(
          runner.command(Platform.resolvedExecutable, ['--version']),
          throwsA(isA<RunCancelled>()),
        );
      } finally {
        release.writeAsStringSync('release');
        try {
          await command;
        } catch (_) {
          /* primary checked above */
        }
        await runner.unwind().timeout(const Duration(seconds: 5));
        await runner.owned.close();
        await controller.close();
        await holder.close();
        for (final owned in runner.active) {
          await owned.exitCode.timeout(const Duration(seconds: 5));
        }
        await logs.delete(recursive: true);
      }
    },
  );
  test(
    'monitor drain lets an in-flight real command register before closing ownership and rejects late acquisition',
    () async {
      final logs = await Directory.systemTemp.createTemp('scenario-monitor-');
      final library =
          (reflect(harness.main) as ClosureMirror).function.owner
              as LibraryMirror;
      final type =
          library.declarations[MirrorSystem.getSymbol('_Run', library)]
              as ClassMirror;
      final runner =
          type.newInstance(const Symbol(''), [logs]).reflectee as dynamic;
      final source = File('${logs.path}/drain.dart')
        ..writeAsStringSync(
          "import 'dart:io'; void main() { stdout.write('drained-before-cleanup'); }",
        );
      Future<dynamic>? command;
      try {
        // start() is still resolving the real managed child/Windows identity when
        // teardown begins. This reproduced the previously untracked adb launch.
        command = runner.command(Platform.resolvedExecutable, [source.path]);
        runner.monitorTasks.add(command!.then<void>((_) {}));
        final draining = runner.stopMonitors();
        await expectLater(
          runner.command(Platform.resolvedExecutable, ['--version']),
          throwsA(isA<RunCancelled>()),
        );
        await draining;
        expect(await runner.owned.close(), isEmpty);
        final ownership =
            jsonDecode(File('${logs.path}/ownership.json').readAsStringSync())
                as Map;
        expect(ownership['processes'], isEmpty);
        expect(
          logs
              .listSync()
              .whereType<File>()
              .where((f) => f.path.endsWith('.stdout.log'))
              .map((f) => f.readAsStringSync())
              .join(),
          contains('drained-before-cleanup'),
        );
      } finally {
        try {
          await command;
        } catch (_) {
          /* always reclaim the fixture */
        }
        runner.finishing = true;
        await runner.owned.close();
        await logs.delete(recursive: true);
      }
    },
  );
  test(
    'cancellation unwinds readiness and rejects later process admission',
    () async {
      final logs = await Directory.systemTemp.createTemp('scenario-cancel-');
      final library =
          (reflect(harness.main) as ClosureMirror).function.owner
              as LibraryMirror;
      final type =
          library.declarations[MirrorSystem.getSymbol('_Run', library)]
              as ClassMirror;
      final runner =
          type.newInstance(const Symbol(''), [logs]).reflectee as dynamic;
      try {
        final readiness = runner.until(
          () async => false,
          const Duration(seconds: 2),
        );
        runner.token.cancel();
        await expectLater(readiness, throwsA(isA<RunCancelled>()));
        await runner.unwind();
        await expectLater(
          runner.command(Platform.resolvedExecutable, ['--version']),
          throwsA(isA<RunCancelled>()),
        );
        expect(File('${logs.path}/commands.jsonl').existsSync(), isFalse);
      } finally {
        runner.finishing = true;
        await runner.owned.close();
        await logs.delete(recursive: true);
      }
    },
  );
  test(
    'restore ignores buffered stale Online and observes automatic recovery without competing Connect',
    () async {
      final fixture = Fixture()..fault = 'recovery ordering';
      final participants = await fixture.start();
      try {
        await runScenario(
          fixture.controller,
          participants,
          restart: fixture.restart,
          timeout: short,
        );
      } finally {
        await fixture.close();
      }
    },
  );
  for (final intentional in [true, false]) {
    test(
      'managed ${intentional ? 'intentional ensure stop is accepted' : 'unexpected early exit fails readiness'}',
      () async {
        final logs = await Directory.systemTemp.createTemp(
          'scenario-lifecycle-',
        );
        final library =
            (reflect(harness.main) as ClosureMirror).function.owner
                as LibraryMirror;
        final type =
            library.declarations[MirrorSystem.getSymbol('_Run', library)]
                as ClassMirror;
        final run = type.newInstance(const Symbol(''), [logs]);
        final runner = run.reflectee as dynamic;
        final source = File('${logs.path}/child.dart')
          ..writeAsStringSync(
            intentional
                ? "import 'dart:async'; void main() { Timer.periodic(Duration(seconds: 1), (_) {}); }"
                : "import 'dart:io'; void main() { exit(73); }",
          );
        try {
          final child = await runner.start(Platform.resolvedExecutable, [
            source.path,
          ], logDir: logs);
          if (intentional) {
            final stopping = child
                .stop(); // exactly the Emulators.ensure ownership call
            expect(child.stop(), same(stopping));
            await stopping;
            await runner.until(() async => true, const Duration(seconds: 2));
          } else {
            await expectLater(
              runner.until(() async => false, const Duration(seconds: 2)),
              throwsA(
                isA<StateError>().having(
                  (e) => e.toString(),
                  'stage exit',
                  contains('73'),
                ),
              ),
            );
          }
        } finally {
          runner.finishing = true;
          await runner.owned.close();
          await logs.delete(recursive: true);
        }
      },
    );
  }
  test(
    'all three pairs exchange six fresh exact texts, recover, restart and stop',
    () async {
      final fixture = Fixture();
      final participants = await fixture.start();
      final failures = <Object>[];
      try {
        await runScenario(
          fixture.controller,
          participants,
          restart: fixture.restart,
          timeout: short,
          onFailure: (error, _, _) => failures.add(error),
        );
        expect(failures, isEmpty); // Expected offline error and absence expiries.
        final sends = fixture.commands
            .where((c) => c.$2 == 'send_probe_text')
            .toList();
        expect(sends.take(6).map((c) => '${c.$1}>${c.$3['peer_id']}'), [
          'windows>android-1-id',
          'android>windows-1-id',
          'windows>node-1-id',
          'node>windows-1-id',
          'android>node-1-id',
          'node>android-1-id',
        ]);
        expect(sends.map((c) => c.$3['text']).toSet().length, sends.length);
        expect(fixture.restarted, isTrue);
        final restartConnect = fixture.commands.where(
          (c) => c.$2 == 'connect_peer' && c.$3['target'] == 'node-2-id-ticket',
        );
        expect(restartConnect, isNotEmpty);
        expect(
          fixture.commands
              .where((c) => c.$2 == 'network_stop')
              .map((c) => c.$1)
              .toSet(),
          {'windows', 'android', 'node'},
        );
      } finally {
        await fixture.close();
      }
    },
  );

  for (final fault in [
    'wrong body',
    'wrong peer',
    'sequence',
    'command',
    'unknown path',
    'replay',
    'late replay',
    'same instance',
    'same endpoint',
    'retry intention',
  ]) {
    test(
      '$fault fails the scenario and leaves command/event diagnostics',
      () async {
        final fixture = Fixture()..fault = fault;
        if (fault == 'retry intention') {
          // Model a scheduled successful recovery reply slower than 40ms.
          fixture.recoveryReplyDelay = const Duration(milliseconds: 80);
        }
        final participants = await fixture.start();
        final failures = <(Object, String?, String?)>[];
        try {
          await expectLater(
            runScenario(
              fixture.controller,
              participants,
              restart: fixture.restart,
              // This asserts state, not RPC latency; leave room for scheduling.
              timeout: fault == 'retry intention'
                  ? const Duration(seconds: 2)
                  : short,
              onFailure: (error, phase, kind) =>
                  failures.add((error, phase, kind)),
            ),
            throwsA(switch (fault) {
              'wrong body' ||
              'wrong peer' ||
              'unknown path' => isA<TimeoutException>(),
              'command' => isA<RpcError>().having(
                (e) => e.code,
                'code',
                'PeerNotConnected',
              ),
              'sequence' => isA<StateError>().having(
                (e) => e.toString(),
                'sequence failure',
                contains('sequence'),
              ),
              'replay' || 'late replay' => isA<StateError>().having(
                (e) => e.toString(),
                'replay failure',
                contains('Forbidden'),
              ),
              'retry intention' => isA<StateError>().having(
                (e) => e.toString(),
                'Disconnect failure',
                contains('Disconnect'),
              ),
              _ => isA<StateError>().having(
                (e) => e.toString(),
                'restart failure',
                contains('Restart'),
              ),
            }),
          );
          expect(failures, hasLength(1));
          if (fault == 'retry intention') {
            expect(failures.single.$1, isA<StateError>());
            expect(failures.single.$2, isNull);
            expect(failures.single.$3, isNull);
          }
          if (fault == 'command') {
            expect(failures.single.$1, isA<RpcError>());
            expect(failures.single.$2, isNull);
            expect(failures.single.$3, isNull);
          }
          final wire = await File(
            '${fixture.logs.path}/wire.jsonl',
          ).readAsString();
          expect(wire, contains('network_start'));
          expect(wire, contains('received'));
        } finally {
          await fixture.close();
        }
      },
    );
  }
}
