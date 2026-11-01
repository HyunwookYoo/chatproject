import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:mirrors';

import 'package:test/test.dart';
import 'package:chat_e2e/src/emulators.dart';
import 'package:chat_e2e/src/process_runner.dart';

import '../bin/run.dart' as cli;
import 'emulators_test.dart' as fixtures;

class OwnedFixture implements OwnedProcess {
  OwnedFixture(this.pid, this.order);
  @override
  final int pid;
  final List<String> order;
  final done = Completer<int>();
  @override
  Future<int> get exitCode => done.future;
  @override
  Future<void> stop() async {
    order.add('emulator');
    if (!done.isCompleted) done.complete(0);
  }
}

void main() {
  final library =
      (reflect(cli.main) as ClosureMirror).function.owner as LibraryMirror;
  ClassMirror type(String name) =>
      library.declarations[MirrorSystem.getSymbol(name, library)]
          as ClassMirror;
  test('partial Android launch records PID before failing status', () async {
    final activity = <String, Object?>{'serial': 'owned-test'};
    var saved = false;
    final result =
        library.invoke(
              MirrorSystem.getSymbol('_captureAndroidLaunch', library),
              [
                ProcessResult(1, 7, '', ''),
                () async => ProcessResult(2, 0, '31415\n', ''),
                activity,
                () {
                  saved = true;
                  expect(activity['pid'], '31415');
                },
              ],
            ).reflectee
            as Future;
    await expectLater(result, throwsA(isA<StateError>()));
    expect(saved, isTrue);
  });
  late Directory logs;
  test(
    'Android build environment selection validates and composes only child overrides',
    () {
      final method = MirrorSystem.getSymbol(
        '_androidBuildEnvironment',
        library,
      );
      Map<String, String>? select(Map<String, String> inputs) =>
          library.invoke(method, [inputs]).reflectee as Map<String, String>?;
      expect(select({}), isNull);
      final perl = File('${logs.path}/perl.exe')..writeAsStringSync('fixture');
      final make = File('${logs.path}/make.exe')..writeAsStringSync('fixture');
      final inputs = {
        'E2E_ANDROID_OPENSSL_PERL': perl.path,
        'E2E_ANDROID_MAKE': make.path,
        'E2E_ANDROID_PERL_LIB': logs.path,
        'PATH': 'inherited-path',
        'MSYS2_ENV_CONV_EXCL': 'KEEP',
      };
      for (final missing in ['E2E_ANDROID_MAKE', 'E2E_ANDROID_PERL_LIB']) {
        expect(
          () => select({...inputs}..remove(missing)),
          throwsA(isA<StateError>()),
        );
      }
      expect(
        () => select({...inputs, 'E2E_ANDROID_MAKE': '${logs.path}/absent'}),
        throwsA(isA<StateError>()),
      );
      if (!Platform.isWindows) {
        expect(() => select(inputs), throwsA(isA<StateError>()));
        return;
      }
      final selected = select(inputs)!;
      expect(selected['OPENSSL_SRC_PERL'], perl.path.replaceAll('\\', '/'));
      final unix =
          '/${logs.path[0].toLowerCase()}${logs.path.substring(2).replaceAll('\\', '/')}';
      expect(selected['PERL5LIB'], unix);
      expect(selected['MSYS2_ENV_CONV_EXCL'], 'KEEP;PERL5LIB');
      expect(
        select(
          {...inputs}..remove('MSYS2_ENV_CONV_EXCL'),
        )!['MSYS2_ENV_CONV_EXCL'],
        'PERL5LIB',
      );
      expect(
        select({
          ...inputs,
          'MSYS2_ENV_CONV_EXCL': 'KEEP;PERL5LIB',
        })!['MSYS2_ENV_CONV_EXCL'],
        'KEEP;PERL5LIB',
      );
      expect(selected['PATH'], '${logs.path};${logs.path};inherited-path');
      expect(inputs.containsKey('OPENSSL_SRC_PERL'), isFalse);
      expect(
        () => select({...inputs, 'E2E_ANDROID_PERL_LIB': r'\\server\share'}),
        throwsA(isA<StateError>()),
      );
    },
  );
  test(
    'recovery audit scans direct text files and fails with fixed detail',
    () {
      const marker = 'PUBLIC_SYNTHETIC_RECOVERY_PHRASE';
      final text = File('${logs.path}/wire.jsonl')
        ..writeAsStringSync('redacted');
      Directory('${logs.path}/store').createSync();
      File('${logs.path}/store/private.log').writeAsStringSync(marker);
      File('${logs.path}/binary.db').writeAsStringSync(marker);
      final method = MirrorSystem.getSymbol('_auditRecoveryLogs', library);
      expect(
        library.invoke(method, [
          logs,
          [marker],
        ]).reflectee,
        1,
      );
      text.writeAsStringSync(marker);
      expect(
        () => library.invoke(method, [
          logs,
          [marker],
        ]),
        throwsA(
          isA<StateError>().having(
            (e) => e.message == 'Recovery privacy audit failed',
            'fixed detail',
            true,
          ),
        ),
      );
      final serialized = jsonEncode([marker]);
      text.writeAsStringSync(jsonEncode({'wire': serialized}));
      expect(
        () => library.invoke(method, [
          logs,
          [serialized],
        ]),
        throwsA(
          isA<StateError>().having(
            (e) => e.message == 'Recovery privacy audit failed',
            'fixed detail',
            true,
          ),
        ),
      );
    },
  );
  late InstanceMirror run;
  void phase(String name) {
    final values =
        type('_Phase').getField(const Symbol('values')).reflectee as List;
    run.setField(
      const Symbol('phase'),
      values.singleWhere((v) => v.value == name),
    );
  }

  Map projection() =>
      run.getField(const Symbol('safeFailure')).reflectee as Map;
  setUp(() async {
    logs = await Directory.systemTemp.createTemp('diagnostic_fixture_');
    run = type('_Run').newInstance(const Symbol(''), [logs]);
  });
  tearDown(() async {
    final runner = run.reflectee as dynamic;
    await runner.interrupt(null);
    await runner.unwind();
    await runner.owned.close();
    await logs.delete(recursive: true);
  });

  test(
    'owned Android build command override does not reach neighboring host command',
    () async {
      final runner = run.reflectee as dynamic;
      const key = 'E2E_PUBLIC_BUILD_ENV_PROBE';
      final inherited = Platform.environment[key];
      final source = File('${logs.path}/child.dart')
        ..writeAsStringSync(
          "import 'dart:io'; void main() { stdout.write(Platform.environment['$key'] ?? 'absent'); }",
        );
      phase('build_android');
      final selected =
          await runner.command(
                Platform.resolvedExecutable,
                [source.path],
                environment: <String, String>{key: 'android-only'},
              )
              as ProcessResult;
      expect(selected.stdout, 'android-only');
      phase('build_node');
      final neighbor =
          await runner.command(Platform.resolvedExecutable, [source.path])
              as ProcessResult;
      expect(neighbor.stdout, inherited ?? 'absent');
      expect(Platform.environment[key], inherited);
      expect(runner.processes, isEmpty);
    },
  );

  for (final registered in [false, true]) {
    test(
      'runner owned emulator cleanup order after acquisition registered=$registered',
      () async {
        final runner = run.reflectee as dynamic;
        final order = <String>[];
        final emulator = OwnedFixture(12345, order);
        runner.active.add(emulator);
        runner.processes.add(<String, Object?>{
          'pid': 12345,
          'avd': 'chat_e2e_1',
        });
        runner.owned.add('emulator', () async {
          await runner.stop(emulator);
        });
        if (registered) {
          runner.owned.add('reverse', () async {
            expect(order, isEmpty);
            order.add('reverse');
          });
          runner.owned.add('activity', () async {
            expect(order, isEmpty);
          });
        }
        runner.token.cancel();
        await runner.interrupt(null);
        expect(order, isEmpty);
        expect(await runner.owned.close(), isEmpty);
        expect(order, registered ? ['reverse', 'emulator'] : ['emulator']);
        expect(runner.active, isEmpty);
      },
    );
  }

  for (final origin in ['android_pid_monitor', 'scenario']) {
    test(
      '$origin deadline keeps immutable origin during node restart phase',
      () async {
        final runner = run.reflectee as dynamic;
        phase('node_participant');
        const deadline = Duration(milliseconds: 20);
        final method = origin == 'scenario' ? #wait : #command;
        final arguments = <Symbol, dynamic>{
          #timeout: deadline,
          #failurePhase: origin,
        };
        final release = Completer<void>();
        final source = File('${logs.path}/waiting.dart')
          ..writeAsStringSync(
            "import 'dart:async'; void main() { Timer.periodic(Duration(seconds: 1), (_) {}); }",
          );
        final lifecycle = runner.token.cancelled.then<void>((_) {
          phase('cleanup');
          runner.fail(
            const FileSystemException('PRIVATE_TOKEN'),
            StackTrace.current,
          );
        });
        final work =
            run
                    .invoke(
                      method,
                      origin == 'scenario'
                          ? [release.future]
                          : [
                              Platform.resolvedExecutable,
                              [source.path],
                            ],
                      arguments,
                    )
                    .reflectee
                as Future;
        try {
          await expectLater(work, throwsA(isA<TimeoutException>()));
          await lifecycle;
          runner.recordFailure(const FileSystemException('PRIVATE_TOKEN'));
          expect(projection(), {
            'phase': origin,
            'failure_kind': 'timeout',
            'command_exit_code': null,
          });
        } finally {
          release.complete();
        }
      },
    );
  }

  test(
    'AVD throw metadata projects before cleanup and keeps original stack',
    () async {
      final runner = run.reflectee as dynamic;
      phase('avd_create');
      final emulatorRunner = fixtures.FakeRunner(
        (_, _) => ProcessResult(0, -9, '', 'PRIVATE_TOKEN'),
      );
      Object? original;
      StackTrace? originalStack;
      final work = fixtures
          .emulatorsWith(emulatorRunner)
          .ensure(testEmulators.first)
          .catchError((Object error, StackTrace stack) {
            original = error;
            originalStack = stack;
            Error.throwWithStackTrace(error, stack);
          });
      try {
        await runner.wait(work);
        fail('ensure should fail');
      } catch (error, stack) {
        expect(error, same(original));
        expect(stack.toString(), originalStack.toString());
        expect(error, isA<StateError>());
      }
      phase('cleanup');
      runner.recordFailure(const FileSystemException('PRIVATE_TOKEN'));
      expect(projection(), {
        'phase': 'avd_create',
        'failure_kind': 'avd_list_exit',
        'command_exit_code': -9,
      });
    },
  );

  for (final entry in <(String, List<String>, String, int)>[
    ('chat_node', ['relay', 'PRIVATE_TOKEN'], 'owned_relay_exit', 1),
    ('chat_node', ['PRIVATE_TOKEN', 'relay'], 'owned_chat_node_exit', 1),
    ('emulator', ['PRIVATE_TOKEN'], 'owned_emulator_exit', 1),
    ('chat_app', ['PRIVATE_TOKEN'], 'owned_windows_exit', 1),
    ('chat_relay', ['PRIVATE_TOKEN'], 'owned_child_exit', 1),
    ('dart', [], 'owned_child_exit', 0),
  ]) {
    test(
      'unexpected persistent ${entry.$3}/${entry.$4} uses only fixed metadata',
      () async {
        final runner = run.reflectee as dynamic;
        phase('avd_create');
        // Tiny native fixtures exercise the real asynchronous exit callback.
        final source = Platform.isWindows
            ? '${Platform.environment['SystemRoot']}/System32/where.exe'
            : '/bin/false';
        final executable = entry.$4 == 0
            ? File(Platform.resolvedExecutable)
            : await File(source).copy(
                '${logs.path}/${entry.$1}${Platform.isWindows ? '.exe' : ''}',
              );
        if (!Platform.isWindows && entry.$4 != 0) {
          expect(
            (await Process.run('chmod', ['+x', executable.path])).exitCode,
            0,
          );
        }
        final args = entry.$4 == 0
            ? [
                (File(
                  '${logs.path}/PRIVATE_TOKEN.dart',
                )..writeAsStringSync('void main() {}')).path,
              ]
            : entry.$2;
        final failure = expectLater(
          runner.fatal.future,
          throwsA(
            isA<StateError>()
                .having(
                  (e) => e.message,
                  'private message',
                  'Owned child exited (${entry.$4})',
                )
                .having(
                  (e) => e.toString(),
                  'toString',
                  StateError('Owned child exited (${entry.$4})').toString(),
                ),
          ),
        );
        final process = await runner.acquire(
          executable.path,
          args,
          persistent: true,
        );
        await failure;
        expect(await process.exitCode, entry.$4);
        phase('cleanup');
        runner.recordFailure(const FileSystemException('PRIVATE_TOKEN'));
        expect(projection(), {
          'phase': 'avd_create',
          'failure_kind': entry.$3,
          'command_exit_code': entry.$4,
        });
        expect(projection().toString(), isNot(contains('PRIVATE_TOKEN')));
        await runner.stop(process);
        expect(runner.active, isEmpty);
        expect(runner.processes, isEmpty);
      },
    );
  }

  test(
    'wait records the original error before cancellation callbacks',
    () async {
      final runner = run.reflectee as dynamic;
      phase('scenario');
      final original = TimeoutException('PRIVATE_TOKEN');
      final originalStack = StackTrace.current;
      // Deliberate lifecycle injection through the existing fail route.
      final lifecycle = runner.token.cancelled.then<void>((_) {
        phase('cleanup');
        runner.fail(
          const FileSystemException('injected lifecycle failure'),
          StackTrace.current,
        );
      });
      Object? caught;
      StackTrace? caughtStack;
      try {
        await runner.wait(Future<void>.error(original, originalStack));
      } catch (error, stack) {
        caught = error;
        caughtStack = stack;
      }
      await lifecycle;
      expect(caught, same(original));
      expect(caughtStack.toString(), originalStack.toString());
      expect(runner.token.isCancelled, isTrue);
      expect(projection(), {
        'phase': 'scenario',
        'failure_kind': 'timeout',
        'command_exit_code': null,
      });
    },
  );

  test(
    'real command timeout survives lifecycle failure during owned stop',
    () async {
      final runner = run.reflectee as dynamic;
      phase('build_node');
      final source = File('${logs.path}/waiting.dart')
        ..writeAsStringSync('''
import 'dart:async';
void main() { Timer.periodic(const Duration(seconds: 1), (_) {}); }
''');
      const secondary = FileSystemException('injected lifecycle failure');
      // Only the lifecycle callback is injected. Acquisition, wait timeout,
      // command finally and native owned-child stop all execute for real.
      final lifecycle = runner.token.cancelled.then<void>((_) {
        expect(runner.active, isNotEmpty);
        runner.fail(secondary, StackTrace.current);
      });
      Object? original;
      try {
        await runner.command(Platform.resolvedExecutable, [
          source.path,
        ], timeout: const Duration(milliseconds: 20));
      } catch (error) {
        original = error;
        runner.recordFailure(error); // Same outer-catch boundary as main.
      }
      await lifecycle;
      expect(original, isA<TimeoutException>());
      expect(runner.firstFailure, same(secondary));
      expect(runner.active, isEmpty);
      expect(runner.processes, isEmpty);
      expect(await runner.owned.close(), isEmpty);
      expect(projection(), {
        'phase': 'build_node',
        'failure_kind': 'timeout',
        'command_exit_code': null,
      });
    },
  );

  test(
    'real nonzero command records its code before a later lifecycle failure',
    () async {
      final runner = run.reflectee as dynamic;
      phase('build_android');
      final source = File('${logs.path}/exit.dart')
        ..writeAsStringSync("import 'dart:io'; void main() { exit(17); }");
      Object? original;
      StackTrace? originalStack;
      try {
        await runner.command(Platform.resolvedExecutable, [source.path]);
      } catch (error, stack) {
        original = error;
        originalStack = stack;
      }
      // Deliberate later lifecycle injection; never manually record the primary.
      phase('cleanup');
      runner.fail(
        const FileSystemException('injected lifecycle failure'),
        StackTrace.current,
      );
      expect(original, isA<StateError>());
      expect(reflect(original).getField(const Symbol('code')).reflectee, 17);
      expect(originalStack.toString(), contains('_Run.command'));
      expect(runner.token.isCancelled, isFalse);
      expect(runner.active, isEmpty);
      expect(await runner.owned.close(), isEmpty);
      expect(projection(), {
        'phase': 'build_android',
        'failure_kind': 'command_exit',
        'command_exit_code': 17,
      });
    },
  );

  test('successful command leaves cleanup-only failure available', () async {
    final runner = run.reflectee as dynamic;
    phase('build_node');
    final source = File('${logs.path}/success.dart')
      ..writeAsStringSync('void main() {}');
    final result = await runner.command(Platform.resolvedExecutable, [
      source.path,
    ]);
    expect(result.exitCode, 0);
    expect(run.getField(const Symbol('safeFailure')).reflectee, isNull);
    phase('cleanup');
    runner.owned.add('injected cleanup', () async {
      throw const FileSystemException('injected cleanup failure');
    });
    final failures = await runner.owned.close();
    expect(failures, hasLength(1));
    if (failures.isNotEmpty) {
      runner.recordFailure(null); // main cleanup boundary.
    }
    expect(runner.active, isEmpty);
    expect(projection(), {
      'phase': 'cleanup',
      'failure_kind': 'cleanup',
      'command_exit_code': null,
    });
  });

  test(
    'first typed command failure survives cleanup without private values',
    () {
      phase('build_android');
      final error = type(
        '_CommandFailure',
      ).newInstance(const Symbol(''), [17, 'PRIVATE_TOKEN']).reflectee;
      expect(error, isA<StateError>());
      expect(
        (error as StateError).message,
        'Command failed (17): PRIVATE_TOKEN',
      );
      run.invoke(const Symbol('recordFailure'), [error]);
      phase('cleanup');
      run.invoke(const Symbol('recordFailure'), [
        const FileSystemException('PRIVATE_TOKEN'),
      ]);
      expect(projection(), {
        'phase': 'build_android',
        'failure_kind': 'command_exit',
        'command_exit_code': 17,
      });
      expect(projection().toString(), isNot(contains('PRIVATE_TOKEN')));
    },
  );

  test('cleanup-only and unknown failures use fixed categories', () {
    phase('cleanup');
    run.invoke(const Symbol('recordFailure'), [null]);
    expect(projection(), {
      'phase': 'cleanup',
      'failure_kind': 'cleanup',
      'command_exit_code': null,
    });
    run = type('_Run').newInstance(const Symbol(''), [logs]);
    phase('android_participant');
    run.invoke(const Symbol('fail'), ['PRIVATE_TOKEN', StackTrace.current]);
    expect(projection(), {
      'phase': 'android_participant',
      'failure_kind': 'other',
      'command_exit_code': null,
    });
  });
}
