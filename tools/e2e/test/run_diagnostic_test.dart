import 'dart:async';
import 'dart:io';
import 'dart:mirrors';

import 'package:test/test.dart';

import '../bin/run.dart' as cli;

void main() {
  final library =
      (reflect(cli.main) as ClosureMirror).function.owner as LibraryMirror;
  ClassMirror type(String name) =>
      library.declarations[MirrorSystem.getSymbol(name, library)]
          as ClassMirror;
  late Directory logs;
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
