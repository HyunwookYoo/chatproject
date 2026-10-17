import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:chat_e2e/src/android_sdk.dart';
import 'package:chat_e2e/src/owned_resources.dart';
import 'package:chat_e2e/src/process_runner.dart';
import 'package:test/test.dart';

import '../bin/devices.dart';

class CliChild implements OwnedProcess {
  CliChild(this.pid, {int? code, this.onStop}) {
    if (code != null) done.complete(code);
  }
  @override
  final int pid;
  final done = Completer<int>();
  final void Function()? onStop;
  @override
  Future<int> get exitCode => done.future;
  @override
  Future<void> stop() async {
    onStop?.call();
    if (!done.isCompleted) done.complete(-1);
  }
}

class CliRunner implements ProcessRunner {
  final commands = <List<String>>[];
  final launches = <List<String>>[];
  final removed = <String>[];
  final mappings = <String>{};
  int code = 0;
  bool failCleanup = false;
  bool existing = true;
  Future<OwnedProcess> Function(List<String>)? startChild;
  Future<OwnedProcess?> Function(List<String>)? shortChild;
  @override
  Future<ProcessResult> run(
    String executable,
    List<String> arguments, {
    String? stdin,
    String? workingDirectory,
  }) async {
    commands.add(arguments);
    String out = '';
    if (arguments.contains('-list-avds')) out = 'chat_e2e_1\nchat_e2e_2\n';
    if (arguments.first == 'devices') out = 'List of devices attached\n${existing ? 'emulator-5554\tdevice\n' : ''}';
    if (arguments.contains('name')) out = 'chat_e2e_1\nOK\n';
    if (arguments.contains('getprop')) out = '1\n';
    if (arguments.contains('--list')) out = mappings.map((port) => 'emulator-5554 $port $port').join('\n');
    if (arguments.contains('reverse') && !arguments.contains('--list')) {
      if (arguments.contains('--remove')) {
        removed.add(arguments.last);
        if (failCleanup && arguments.last == 'tcp:3340') return ProcessResult(1, 7, '', 'remove failed');
        mappings.remove(arguments.last);
      } else {
        mappings.add(arguments.last);
      }
    }
    return ProcessResult(1, 0, out, '');
  }

  @override
  Future<OwnedProcess> start(
    String executable,
    List<String> arguments, {
    required Directory logDir,
    String? workingDirectory,
    String? stdin,
    bool retainOutput = false,
  }) async {
    if (!arguments.contains('-avd') && arguments.first != 'test') {
      final controlled = await shortChild?.call(arguments);
      if (controlled != null) return controlled;
      final result = await run(executable, arguments, stdin: stdin, workingDirectory: workingDirectory);
      final childPid = 1000 + commands.length;
      File('${logDir.path}/$childPid.stdout.log').writeAsStringSync(result.stdout as String);
      File('${logDir.path}/$childPid.stderr.log').writeAsStringSync(result.stderr as String);
      return CliChild(childPid, code: result.exitCode);
    }
    launches.add(arguments);
    return startChild == null ? CliChild(100, code: code) : await startChild!(arguments);
  }

  @override
  Future<void> startDetached(String executable, List<String> arguments) async => throw StateError('unowned start');
  @override
  Future<int> runInherited(String executable, List<String> arguments, {String? workingDirectory}) async =>
      throw StateError('unowned run');
}

void main() {
  late Directory temp;
  late Directory logs;
  late CliRunner runner;
  setUp(() async {
    temp = await Directory.systemTemp.createTemp('devices_cli_');
    logs = Directory('${temp.path}/logs');
    runner = CliRunner();
    final avd = Directory('${temp.path}/chat_e2e_1.avd')..createSync();
    final flavor = Platform.environment['CI'] == 'true' ? 'google_apis' : 'google_apis_playstore';
    File('${avd.path}/config.ini').writeAsStringSync(
      'image.sysdir.1=system-images/android-36/$flavor/x86_64/\nabi.type=x86_64\nhw.cpu.arch=x86_64\n',
    );
    Directory('${avd.path}/snapshots/clean').createSync(recursive: true);
  });
  tearDown(() => temp.delete(recursive: true));

  Future<int> run(List<String> args, {RunCancellation? cancellation}) => runDevices(
    args,
    runner: runner,
    sdk: AndroidSdk('/sdk', windows: false),
    avdHome: temp.path,
    logDir: logs,
    cancellation: cancellation,
  );

  test('separator forwards literal flutter arguments and preserves existing AVD', () async {
    expect(await run(['test', '--', '--name', 'needle']), 0);
    expect(runner.launches.single, ['test', '--name', 'needle', '-d', 'emulator-5554']);
    expect(runner.commands.any((args) => args.contains('kill')), isFalse);
  });
  test('unknown option before separator fails before device access', () async {
    expect(await run(['test', '--name', 'needle']), 64);
    expect(runner.commands, isEmpty);
  });
  test('cleanup attempts all reverses and preserves primary code', () async {
    runner.code = 23;
    runner.failCleanup = true;
    expect(await run(['test']), 23);
    expect(runner.removed, ['tcp:3340', 'tcp:47000']);
  });
  test('cleanup changes success to one', () async {
    runner.failCleanup = true;
    expect(await run(['test']), 1);
  });
  test('preexisting reverse is never claimed or removed', () async {
    runner.mappings.add('tcp:47000');
    expect(await run(['test']), 0);
    expect(runner.removed, ['tcp:3340']);
    expect(runner.mappings, contains('tcp:47000'));
  });
  test('cancel while test runs stops child then removes all acquired reverse mappings', () async {
    final cancellation = RunCancellation();
    final ready = Completer<void>();
    var stops = 0;
    runner.startChild = (_) async {
      ready.complete();
      return CliChild(
        101,
        onStop: () {
          stops++;
        },
      );
    };
    final result = run(['test'], cancellation: cancellation);
    await ready.future;
    cancellation.cancel();
    expect(await result, 130);
    expect(stops, greaterThan(0));
    expect(runner.removed, ['tcp:3340', 'tcp:47000']);
  });

  test('cancel still unwinds when stopping the test child fails', () async {
    final cancellation = RunCancellation();
    final ready = Completer<void>();
    final child = CliChild(101, onStop: () => throw StateError('identity unavailable'));
    runner.startChild = (_) async {
      ready.complete();
      return child;
    };
    final result = run(['test'], cancellation: cancellation);
    await ready.future;
    // Let start return and ownership register before requesting cancellation.
    await Future<void>.delayed(Duration.zero);
    cancellation.cancel();
    try {
      expect(await result.timeout(const Duration(seconds: 1)), 130);
      expect(runner.removed, ['tcp:3340', 'tcp:47000']);
    } finally {
      child.done.completeError(StateError('late child exit error'));
      await result;
    }
    expect(runner.launches, hasLength(1));
  });
  test('cancel during pending acquisition registers then stops child without later launch', () async {
    runner.existing = false;
    final cancellation = RunCancellation();
    final acquired = Completer<void>();
    final release = Completer<void>();
    var stopped = 0;
    runner.startChild = (_) async {
      acquired.complete();
      await release.future;
      return CliChild(
        101,
        onStop: () {
          stopped++;
        },
      );
    };
    final result = run(['test'], cancellation: cancellation);
    await acquired.future;
    cancellation.cancel();
    release.complete();
    expect(await result, 130);
    expect(stopped, greaterThan(0));
    expect(runner.launches, hasLength(1));
    expect(runner.commands.any((args) => args.contains('reverse')), isFalse);
  });

  for (final failStop in [false, true]) {
    test('cancel pending short acquisition retains ownership until stop confirmed ($failStop)', () async {
      final token = RunCancellation();
      final ready = Completer<void>();
      final release = Completer<void>();
      var lists = 0;
      var stops = 0;
      final child = CliChild(202, onStop: () {
        stops++;
        if (failStop) throw StateError('identity unavailable');
      });
      runner.shortChild = (args) async {
        if (!args.contains('--list') || ++lists != 2) return null;
        ready.complete();
        await release.future;
        return child;
      };
      final result = run(['test'], cancellation: token);
      await ready.future;
      token.cancel();
      release.complete();
      try {
        expect(await result.timeout(const Duration(seconds: 1)), 130);
        expect(stops, greaterThan(0));
        expect(runner.removed, ['tcp:47000']);
        expect(runner.launches, isEmpty);
        final records = logs.listSync(recursive: true).whereType<File>().where((file) => file.path.endsWith('ownership.json')).toList();
        if (failStop) {
          expect(records, hasLength(1));
          final record = jsonDecode(records.single.readAsStringSync()) as Map<String, dynamic>;
          expect((record['processes'] as List).single['pid'], 202);
          expect(record['reverse'], isEmpty);
        } else {
          expect(records, isEmpty);
        }
      } finally {
        if (!child.done.isCompleted) child.done.complete(-1);
        await result;
      }
    });
  }

  test('failed short startup retains exact child and original failure despite cancellation', () async {
    final token = RunCancellation();
    final child = CliChild(203, onStop: () => throw StateError('identity unavailable'));
    runner.shortChild = (args) async {
      if (!args.contains('--list')) return null;
      token.cancel();
      throw OwnedProcessStartFailure(StateError('original input failure'), StackTrace.current,
        child, StateError('identity unavailable'), StateError('diagnostic failure'));
    };
    try {
      expect(await run(['test'], cancellation: token), 1);
      final recordFile = logs.listSync(recursive: true).whereType<File>().singleWhere((file) => file.path.endsWith('ownership.json'));
      final record = jsonDecode(recordFile.readAsStringSync()) as Map<String, dynamic>;
      expect((record['processes'] as List).single['pid'], 203);
      expect(File('${recordFile.parent.path}/failure.log').readAsStringSync(), startsWith('Bad state: original input failure'));
      expect(File('${recordFile.parent.path}/cleanup.log').readAsStringSync(), contains('diagnostic failure'));
      expect(runner.launches, isEmpty);
    } finally {
      child.done.complete(-1);
    }
  });

  test('interrupted unconfirmed reverse add is retained without removing a foreign mapping', () async {
    final token = RunCancellation();
    final ready = Completer<void>();
    final child = CliChild(204);
    runner.shortChild = (args) async {
      if (!args.contains('--no-rebind') || args.last != 'tcp:3340') return null;
      // Another owner may have won the no-rebind race; an interrupted command
      // gives the CLI no successful claim over this mapping.
      runner.mappings.add('tcp:3340');
      ready.complete();
      return child;
    };
    final result = run(['test'], cancellation: token);
    await ready.future;
    token.cancel();
    expect(await result, 130);
    expect(await child.exitCode, -1);
    expect(runner.removed, ['tcp:47000']);
    expect(runner.mappings, contains('tcp:3340'));
    final recordFile = logs.listSync(recursive: true).whereType<File>().singleWhere((file) => file.path.endsWith('ownership.json'));
    final record = jsonDecode(recordFile.readAsStringSync()) as Map<String, dynamic>;
    expect(record['processes'], isEmpty);
    expect((record['reverse'] as List).single, {
      'serial': 'emulator-5554', 'local': 'tcp:3340', 'remote': 'tcp:3340', 'uncertain': true,
    });
  });

  test('cleanup startup failure retains secondary diagnostics and still removes later reverse', () async {
    runner.code = 23;
    final child = CliChild(205, onStop: () => throw StateError('identity unavailable'));
    runner.shortChild = (args) async {
      if (!args.contains('--remove') || args.last != 'tcp:3340') return null;
      throw OwnedProcessStartFailure(StateError('cleanup input failure'), StackTrace.current,
        child, StateError('cleanup identity unavailable'), StateError('cleanup diagnostic failure'));
    };
    try {
      expect(await run(['test']), 23);
      expect(runner.removed, ['tcp:47000']);
      final recordFile = logs.listSync(recursive: true).whereType<File>().singleWhere((file) => file.path.endsWith('ownership.json'));
      final record = jsonDecode(recordFile.readAsStringSync()) as Map<String, dynamic>;
      expect((record['processes'] as List).single['pid'], 205);
      expect((record['reverse'] as List).single['local'], 'tcp:3340');
      final diagnostic = File('${recordFile.parent.path}/cleanup.log').readAsStringSync();
      expect(diagnostic, contains('cleanup input failure'));
      expect(diagnostic, contains('cleanup identity unavailable'));
      expect(diagnostic, contains('cleanup diagnostic failure'));
    } finally {
      child.done.complete(-1);
    }
  });

  group('real CLI subprocess with fake command executables', () {
    late Directory fixture;
    late File executable;
    setUpAll(() async {
      fixture = await Directory.systemTemp.createTemp('devices_commands_');
      final source = File('${fixture.path}/fake.dart')..writeAsStringSync(_fakeCommandSource);
      executable = File('${fixture.path}/fake${Platform.isWindows ? '.exe' : ''}');
      final compiled = await Process.run(Platform.resolvedExecutable, [
        'compile',
        'exe',
        source.path,
        '-o',
        executable.path,
      ]);
      if (compiled.exitCode != 0) throw StateError('Fixture compiler failed: ${compiled.stderr}');
    });
    tearDownAll(() => fixture.delete(recursive: true));

    Future<Map<String, String>> environment({
      int code = 0,
      bool failCleanup = false,
      bool wait = false,
      bool existing = true,
      String hang = '',
    }) async {
      File('${temp.path}/chat_e2e_1.avd/config.ini').writeAsStringSync(
        'image.sysdir.1=system-images/android-36/google_apis_playstore/x86_64/\nabi.type=x86_64\nhw.cpu.arch=x86_64\n',
      );
      final sdk = Directory('${temp.path}/sdk');
      final adb = File('${sdk.path}/platform-tools/adb${Platform.isWindows ? '.exe' : ''}');
      final emulator = File('${sdk.path}/emulator/emulator${Platform.isWindows ? '.exe' : ''}');
      adb.parent.createSync(recursive: true);
      emulator.parent.createSync(recursive: true);
      await executable.copy(adb.path);
      await executable.copy(emulator.path);
      final flutter = File('${temp.path}/flutter${Platform.isWindows ? '.bat' : ''}');
      flutter.writeAsStringSync(
        Platform.isWindows
            ? '@echo off\r\n"${executable.path}" %*\r\n'
            : '#!/bin/sh\nexec "${executable.path}" "\$@"\n',
      );
      if (!Platform.isWindows) {
        for (final file in [adb, emulator, flutter]) {
          final result = await Process.run('chmod', ['+x', file.path]);
          expect(result.exitCode, 0);
        }
      }
      return {
        ...Platform.environment,
        'PATH': '${temp.path}${Platform.isWindows ? ';' : ':'}${Platform.environment['PATH']}',
        'ANDROID_HOME': sdk.path,
        'ANDROID_AVD_HOME': temp.path,
        'CI': 'false',
        'FAKE_STATE': temp.path,
        'FAKE_EXIT': '$code',
        'FAKE_CLEANUP_FAIL': '$failCleanup',
        'FAKE_WAIT': '$wait',
        'FAKE_EXISTING': '$existing',
        'FAKE_HANG': hang,
      };
    }

    // A tiny entry point preserves real main/exit semantics while directing
    // artifacts to this test's temporary directory instead of shared build/.
    Future<File> entryPoint() async {
      final cliUri = File('bin/devices.dart').absolute.uri;
      return File('${temp.path}/entry.dart')..writeAsStringSync('''
import 'dart:io';
import 'dart:async';
import '$cliUri';
import 'package:chat_e2e/src/android_sdk.dart';
import 'package:chat_e2e/src/process_runner.dart';
import 'package:chat_e2e/src/owned_resources.dart';
Future<void> main(List<String> args) async {
  final cancellation = RunCancellation();
  final timer = Timer.periodic(const Duration(milliseconds: 10), (_) {
    if (Platform.environment['FAKE_HANG'] == 'list' &&
        File(Platform.environment['FAKE_STATE']! + '/hang.json').existsSync()) cancellation.cancel();
  });
  try {
  exit(await runDevices(args, runner: const IoProcessRunner(), sdk: AndroidSdk.locate(),
    logDir: Directory(Platform.environment['FAKE_STATE']! + '/logs'), cancellation: cancellation,
    commandTimeout: const Duration(milliseconds: 500)));
  } finally { timer.cancel(); }
}
''');
    }

    Future<ProcessResult> invoke(List<String> args, Map<String, String> env) async {
      final entry = await entryPoint();
      return Process.run(Platform.resolvedExecutable, [
        '--packages=${File('.dart_tool/package_config.json').absolute.path}',
        entry.path,
        ...args,
      ], environment: env);
    }

    test('forwards exact arguments, keeps existing AVD, captures child output', () async {
      final result = await invoke(['test', '--', '--name', 'needle'], await environment());
      expect(result.exitCode, 0, reason: '${result.stderr}');
      expect(jsonDecode(File('${temp.path}/flutter-args.json').readAsStringSync()), [
        'test',
        '--name',
        'needle',
        '-d',
        'emulator-5554',
      ]);
      final trace = File('${temp.path}/commands.log').readAsStringSync();
      expect(trace, isNot(contains('"kill"')));
      final output = logs
          .listSync(recursive: true)
          .whereType<File>()
          .where((file) => file.path.endsWith('.stdout.log'));
      expect(output.map((file) => file.readAsStringSync()).join(), contains('fake flutter output'));
    });

    for (final primary in [0, 23]) {
      test('cleanup after primary $primary attempts remaining resources', () async {
        final result = await invoke(['test'], await environment(code: primary, failCleanup: true));
        expect(result.exitCode, primary == 0 ? 1 : primary, reason: '${result.stderr}');
        final trace = File('${temp.path}/commands.log').readAsStringSync();
        expect(trace.indexOf('"--remove","tcp:3340"'), greaterThan(-1));
        expect(trace.indexOf('"--remove","tcp:47000"'), greaterThan(trace.indexOf('"--remove","tcp:3340"')));
        expect(
          logs.listSync(recursive: true).whereType<File>().any((file) => file.path.endsWith('ownership.json')),
          isTrue,
        );
      });
    }

    test('rejects option with separator usage before any command', () async {
      final result = await invoke(['test', '--name', 'needle'], await environment());
      expect(result.exitCode, 64);
      expect(result.stderr, contains('following arguments literally'));
      expect(File('${temp.path}/commands.log').existsSync(), isFalse);
    });

    for (final scenario in [('list', 23), ('remove', 23), ('remove', 0)]) {
      final (hang, primary) = scenario;
      test('hung reverse $hang with primary $primary reclaims its child and attempts later cleanup', () async {
        final env = await environment(code: primary, hang: hang);
        final entry = await entryPoint();
        final process = await Process.start(Platform.resolvedExecutable, [
          '--packages=${File('.dart_tool/package_config.json').absolute.path}', entry.path, 'test',
        ], environment: env);
        final out = process.stdout.drain<void>();
        final err = process.stderr.transform(utf8.decoder).join();
        final hung = File('${temp.path}/hang.json');
        try {
          final deadline = DateTime.now().add(const Duration(seconds: 15));
          while (!hung.existsSync()) {
            if (DateTime.now().isAfter(deadline)) fail('Fake short command never reached the hang');
            await Future<void>.delayed(const Duration(milliseconds: 10));
          }
          final state = jsonDecode(hung.readAsStringSync()) as Map<String, dynamic>;
          final port = state['port'] as int;
          expect(await process.exitCode.timeout(const Duration(seconds: 3)), hang == 'list' ? 130 : primary == 0 ? 1 : primary);
          await out;
          await err;
          await expectLater(Socket.connect(InternetAddress.loopbackIPv4, port), throwsA(isA<SocketException>()));
          final trace = File('${temp.path}/commands.log').readAsStringSync();
          expect(trace, contains('"--remove","tcp:47000"'));
          if (hang == 'list') {
            expect(trace, isNot(contains('"--no-rebind","tcp:3340"')));
            expect(File('${temp.path}/flutter-args.json').existsSync(), isFalse);
          } else {
            final records = logs.listSync(recursive: true).whereType<File>().where((file) => file.path.endsWith('ownership.json')).toList();
            expect(records, hasLength(1));
            final record = jsonDecode(records.single.readAsStringSync()) as Map<String, dynamic>;
            expect(record['processes'], isEmpty, reason: 'The timed-out native child must actually be reclaimed');
            expect((record['reverse'] as List).single['local'], 'tcp:3340');
            expect(File('${records.single.parent.path}/cleanup.log').readAsStringSync(), contains('TimeoutException'));
          }
        } finally {
          // A release protocol owns the fixture even against the broken CLI.
          if (hung.existsSync()) {
            final state = jsonDecode(hung.readAsStringSync()) as Map<String, dynamic>;
            try {
              final socket = await Socket.connect(InternetAddress.loopbackIPv4, state['port'] as int);
              socket.write('release');
              await socket.flush();
              await socket.close();
            } on SocketException { /* Production already reclaimed this exact child. */ }
          }
          await process.exitCode.timeout(const Duration(seconds: 15));
          await out;
          await err;
        }
      });
    }

    for (final args in [
      <String>['up'],
      <String>['test', '--keep'],
    ]) {
      test('${args.join(' ')} leaves its managed emulator alive after parent exit', () async {
        final result = await invoke(args, await environment(existing: false));
        expect(result.exitCode, 0, reason: '${result.stderr}');
        final state = jsonDecode(File('${temp.path}/emulator.json').readAsStringSync()) as Map<String, dynamic>;
        final records = logs
            .listSync(recursive: true)
            .whereType<File>()
            .where((file) => file.path.endsWith('ownership.json'))
            .toList();
        expect(records, hasLength(1));
        final record = jsonDecode(records.single.readAsStringSync()) as Map<String, dynamic>;
        final processRecord = (record['processes'] as List).single as Map<String, dynamic>;
        expect(processRecord['avd'], 'chat_e2e_1');
        final output = File('${records.single.parent.path}/${processRecord['pid']}.stdout.log');
        final errors = File('${records.single.parent.path}/${processRecord['pid']}.stderr.log');
        final outputLength = output.lengthSync();
        final errorLength = errors.lengthSync();
        await Future<void>.delayed(const Duration(seconds: 1));
        final socket = await Socket.connect(InternetAddress.loopbackIPv4, state['port'] as int);
        final outputGrew = output.lengthSync() > outputLength;
        final errorsGrew = errors.lengthSync() > errorLength;
        socket.write('stop');
        await socket.flush();
        await socket.close();
        if (Platform.isWindows) {
          final childPid = processRecord['pid'] as int;
          final waited = await Process.run('powershell.exe', [
            '-NoProfile',
            '-NonInteractive',
            '-Command',
            'try { \$child = [Diagnostics.Process]::GetProcessById($childPid); if (\$child.WaitForExit(5000)) { exit 0 } else { exit 1 } } catch [ArgumentException] { exit 0 }',
          ]);
          expect(
            waited.exitCode,
            0,
            reason: 'Wait for the owned fixture executable to unlock before deleting its directory',
          );
        }
        expect(outputGrew, isTrue, reason: 'stdout must continue draining after parent exit');
        expect(errorsGrew, isTrue, reason: 'stderr must continue draining after parent exit');
      });
    }

    test('explicit down preserves a live stale process with a mismatched recorded path', () async {
      final env = await environment(wait: true);
      final child = await Process.start(executable.path, ['test'], environment: env);
      final out = child.stdout.drain<void>();
      final err = child.stderr.drain<void>();
      try {
        logs.createSync();
        File('${logs.path}/ownership.json').writeAsStringSync(
          jsonEncode({
            'version': 1,
            'ownerPid': 2147483647,
            'processes': [
              {'pid': child.pid, 'path': 'different-executable'},
            ],
            'reverse': [],
          }),
        );
        final result = await invoke(['down'], env);
        expect(result.exitCode, 1);
        expect(File('${logs.path}/ownership.json').existsSync(), isTrue);
        // The retained test child still owns its process; down never signaled it.
        expect(child.kill(), isTrue);
      } finally {
        child.kill();
        await child.exitCode;
        await Future.wait([out, err]);
      }
    });

    test('explicit down retires confirmed absent records but preserves stale reverse', () async {
      final env = await environment();
      logs.createSync();
      final file = File('${logs.path}/ownership.json');
      file.writeAsStringSync(
        jsonEncode({
          'version': 1,
          'ownerPid': 2147483647,
          'processes': [
            {'pid': 2147483647, 'path': 'gone'},
          ],
          'reverse': [],
        }),
      );
      expect((await invoke(['down'], env)).exitCode, 0);
      expect(file.existsSync(), isFalse);
      file.writeAsStringSync(
        jsonEncode({
          'version': 1,
          'ownerPid': 2147483647,
          'processes': [],
          'reverse': [
            {'serial': 'emulator-5554', 'local': 'tcp:3340', 'remote': 'tcp:3340'},
          ],
        }),
      );
      expect((await invoke(['down'], env)).exitCode, 1);
      expect(file.existsSync(), isTrue);
      expect(File('${temp.path}/commands.log').existsSync(), isFalse);
    });

    test('explicit down leaves records of a still-active owner untouched', () async {
      final env = await environment();
      logs.createSync();
      final file = File('${logs.path}/ownership.json')
        ..writeAsStringSync(
          jsonEncode({
            'version': 1,
            'ownerPid': pid,
            'processes': [
              {'pid': 2147483647, 'path': 'gone'},
            ],
            'reverse': [],
          }),
        );
      final before = file.readAsStringSync();
      expect((await invoke(['down'], env)).exitCode, 1);
      expect(file.readAsStringSync(), before);
    });

    test(
      'handled SIGINT waits for child stop and reverse cleanup',
      () async {
        final env = await environment(wait: true);
        final entry = await entryPoint();
        final process = await Process.start(Platform.resolvedExecutable, [
          '--packages=${File('.dart_tool/package_config.json').absolute.path}',
          entry.path,
          'test',
        ], environment: env);
        final out = process.stdout.drain<void>();
        final err = process.stderr.drain<void>();
        addTearDown(() {
          process.kill();
        });
        final deadline = DateTime.now().add(const Duration(seconds: 15));
        while (!File('${temp.path}/ready').existsSync()) {
          if (DateTime.now().isAfter(deadline)) fail('Fake child did not become ready');
          await Future<void>.delayed(const Duration(milliseconds: 20));
        }
        expect(process.kill(ProcessSignal.sigint), isTrue);
        expect(await process.exitCode.timeout(const Duration(seconds: 15)), 130);
        await Future.wait([out, err]);
        final trace = File('${temp.path}/commands.log').readAsStringSync();
        expect(trace, contains('"--remove","tcp:3340"'));
        expect(trace, contains('"--remove","tcp:47000"'));
      },
      skip: Platform.isWindows
          ? 'Windows Process.kill(SIGINT) cannot target this child safely; subscription alone is not interrupt delivery proof.'
          : false,
    );
  });
}

const _fakeCommandSource = r'''
import 'dart:async';
import 'dart:convert';
import 'dart:io';

Future<void> main(List<String> args) async {
  final root = Platform.environment['FAKE_STATE']!;
  File('$root/commands.log').writeAsStringSync('${jsonEncode(args)}\n', mode: FileMode.append, flush: true);
  if ((Platform.environment['FAKE_HANG'] == 'list' && args.contains('--list') && File('$root/first-reverse').existsSync()) ||
      (Platform.environment['FAKE_HANG'] == 'remove' && args.contains('--remove') && args.last == 'tcp:3340')) {
    final server = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
    File('$root/hang.staging').writeAsStringSync(jsonEncode({'pid': pid, 'port': server.port}));
    File('$root/hang.staging').renameSync('$root/hang.json');
    server.listen((socket) { socket.listen((_) => exit(0)); });
    return;
  }
  if (args.contains('--no-rebind') && args.last == 'tcp:47000') File('$root/first-reverse').writeAsStringSync('acquired');
  if (args.first == '-list-avds') { print('chat_e2e_1'); return; }
  if (args.first == 'devices') {
    print(Platform.environment['FAKE_EXISTING'] == 'false' ? 'List of devices attached' : 'List of devices attached\nemulator-5554\tdevice');
    return;
  }
  if (args.first == '-avd') {
    final server = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
    File('$root/emulator.json').writeAsStringSync(jsonEncode({'pid': pid, 'port': server.port}));
    server.listen((socket) { socket.listen((_) => exit(0)); });
    Timer.periodic(const Duration(milliseconds: 200), (_) {
      stdout.writeln('emulator heartbeat');
      stderr.writeln('emulator diagnostic');
    });
    Timer(const Duration(seconds: 20), () => exit(0));
    return;
  }
  if (args.contains('name')) { print('chat_e2e_1\nOK'); return; }
  if (args.contains('getprop')) {
    final booted = Platform.environment['FAKE_EXISTING'] != 'false' || File('$root/emulator.json').existsSync();
    print(booted ? '1' : '0');
    return;
  }
  if (args.first == 'test') {
    File('$root/flutter-args.json').writeAsStringSync(jsonEncode(args));
    print('fake flutter output');
    stderr.writeln('fake flutter stderr');
    if (Platform.environment['FAKE_WAIT'] == 'true') {
      File('$root/ready').writeAsStringSync('ready');
      Timer.periodic(const Duration(seconds: 1), (_) {});
      return;
    }
    exit(int.parse(Platform.environment['FAKE_EXIT']!));
  }
  if (args.contains('--remove') && args.last == 'tcp:3340' && Platform.environment['FAKE_CLEANUP_FAIL'] == 'true') exit(7);
}
''';
