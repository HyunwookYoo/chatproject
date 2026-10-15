import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:args/args.dart';
import 'package:chat_e2e/src/android_sdk.dart';
import 'package:chat_e2e/src/controller.dart';
import 'package:chat_e2e/src/emulators.dart';
import 'package:chat_e2e/src/owned_resources.dart';
import 'package:chat_e2e/src/process_runner.dart';
import 'package:chat_e2e/src/scenario.dart';
import 'package:path/path.dart' as p;

Future<void> main(List<String> arguments) async {
  final parser = ArgParser()
    ..addOption(
      'profile',
      defaultsTo: Platform.isWindows ? 'mixed' : 'android',
      allowed: ['mixed', 'android', 'windows'],
    )
    ..addOption('repo-root', mandatory: true);
  late ArgResults options;
  try {
    options = parser.parse(arguments);
    if (options.rest.isNotEmpty ||
        !p.isAbsolute(options['repo-root'] as String) ||
        (!Platform.isWindows && options['profile'] != 'android')) {
      throw const FormatException(
        'Profile requires a supported host and absolute repo root',
      );
    }
  } on FormatException {
    stderr.writeln(
      'Usage: dart run bin/run.dart --profile mixed|android|windows --repo-root <absolute>',
    );
    exitCode = 64;
    return;
  }
  final root = p.normalize(options['repo-root'] as String);
  final profile = options['profile'] as String;
  final logs = await Directory(
    p.join(
      root,
      'build',
      'e2e',
      '${DateTime.now().microsecondsSinceEpoch}',
      profile,
    ),
  ).create(recursive: true);
  final run = _Run(logs);
  Controller? controller;
  StreamSubscription<ProcessSignal>? signal;
  Future<void>? interrupted;
  var finished = false;
  var userInterrupted = false;
  run.token.cancelled.then((_) {
    if (!finished) interrupted = run.interrupt(controller);
  });
  try {
    signal = ProcessSignal.sigint.watch().listen((_) {
      userInterrupted = true;
      run.token.cancel();
    });
  } on UnsupportedError {
    stderr.writeln(
      'Handled Ctrl+C unavailable; hard termination retains ownership records.',
    );
  }
  var code = 0;
  try {
    if (!File(p.join(root, 'Cargo.toml')).existsSync()) {
      throw StateError('Invalid repository root');
    }
    for (final port in [3340, 47000]) {
      final socket = await ServerSocket.bind(
        InternetAddress.loopbackIPv4,
        port,
        shared: false,
      );
      await socket.close();
    }
    // Compile before any AVD launch; every Rust invocation uses the pinned toolchain.
    if (Platform.isWindows &&
        (Platform.environment['CARGOKIT_TOOLCHAIN'] != '1.92.0' ||
            Platform.environment['CARGOKIT_VERBOSE'] != '1')) {
      throw StateError(
        'Set CARGOKIT_TOOLCHAIN=1.92.0 and CARGOKIT_VERBOSE=1 for Windows Debug builds',
      );
    }
    await run.command(
      'rustup',
      [
        'run',
        '1.92.0',
        'cargo',
        'build',
        '--locked',
        '-p',
        'chat_node',
        '--features',
        'harness',
      ],
      cwd: root,
      timeout: const Duration(minutes: 20),
    );
    final flutter = Platform.isWindows ? 'flutter.bat' : 'flutter';
    final androidCount = profile == 'windows'
        ? 0
        : profile == 'android'
        ? 2
        : 1;
    final windowsCount = profile == 'android'
        ? 0
        : profile == 'windows'
        ? 2
        : 1;
    if (windowsCount > 0) {
      await run.command(
        flutter,
        ['build', 'windows', '--debug'],
        cwd: p.join(root, 'app'),
        timeout: const Duration(minutes: 30),
      );
    }
    if (androidCount > 0) {
      await run.command(
        flutter,
        ['build', 'apk', '--debug', '--target-platform', 'android-x64'],
        cwd: p.join(root, 'app'),
        timeout: const Duration(minutes: 30),
      );
    }
    final node = p.join(
      root,
      'target',
      'debug',
      Platform.isWindows ? 'chat_node.exe' : 'chat_node',
    );
    final relay = await run.start(node, ['relay'], logDir: logs);
    await run.until(() async {
      final output = File(p.join(logs.path, '${relay.pid}.stdout.log'));
      if (!output.existsSync()) return false;
      return output.readAsLinesSync().any(
        (line) =>
            line.contains('"role":"relay"') && line.contains('"type":"ready"'),
      );
    }, const Duration(seconds: 10));
    run.token.check();
    controller = await Controller.start(logDir: logs);
    run.owned.add('controller', controller.close);
    final control = controller;
    final participants = <Participant>[];
    for (var i = 1; i <= windowsCount; i++) {
      final name = 'windows-$i';
      final directory = p.join(logs.path, name);
      await Directory(directory).create();
      await run.start(
        p.join(
          root,
          'app',
          'build',
          'windows',
          'x64',
          'runner',
          'Debug',
          'chat_app.exe',
        ),
        [
          '--data-dir',
          directory,
          '--control-url',
          'ws://127.0.0.1:47000',
          '--name',
          name,
        ],
        logDir: logs,
      );
      participants.add(await run.wait(control.waitParticipant(name)));
    }
    if (androidCount > 0) {
      final sdk = AndroidSdk.locate();
      final specs = testEmulators.take(androidCount).toList();
      final config = AvdConfig.load();
      if (!Directory(sdk.systemImage(config)).existsSync()) {
        throw StateError('Required installed system image missing');
      }
      await run.command(sdk.emulator, ['-accel-check']);
      final emulators = Emulators(
        sdk: sdk,
        runner: run,
        avdHome: defaultAvdHome(),
        logDir: logs,
        config: config,
        checkCancelled: run.token.check,
        onStarted: (spec, process) async {
          run.annotate(process.pid, {'avd': spec.avd, 'serial': spec.serial});
        },
      );
      for (final spec in specs) {
        // E2E installs/launches only on newly owned emulator processes.
        if (await emulators.isRunning(spec)) {
          throw StateError(
            'Test AVD already running; preserved without installing or launching',
          );
        }
      }
      await emulators.preflight(specs);
      for (final spec in specs) {
        await run.wait(
          emulators.ensure(spec),
          timeout: const Duration(minutes: 5),
        );
      }
      await run.wait(emulators.up(specs), timeout: const Duration(minutes: 5));
      for (var i = 0; i < specs.length; i++) {
        final spec = specs[i];
        for (final port in [3340, 47000]) {
          await run.reverse(sdk, spec, port);
        }
        final name = 'android-${i + 1}';
        final adb = ['-s', spec.serial];
        await run.command(sdk.adb, [
          ...adb,
          'install',
          '-r',
          p.join(
            root,
            'app',
            'build',
            'app',
            'outputs',
            'flutter-apk',
            'app-debug.apk',
          ),
        ], timeout: const Duration(minutes: 2));
        const package = 'dev.chatproject.chat_app';
        final activity = <String, Object?>{
          'serial': spec.serial,
          'package': package,
          'name': name,
        };
        run.activities.add(activity);
        run.save();
        run.owned.add('Android activity', () async {
          await run.command(sdk.adb, [
            ...adb,
            'shell',
            'am',
            'force-stop',
            package,
          ], cleanup: true);
          run.activities.remove(activity);
          run.save();
        });
        await run.command(sdk.adb, [
          ...adb,
          'shell',
          'am',
          'start',
          '-W',
          '-n',
          '$package/.MainActivity',
          '--es',
          'chat_control_url',
          'ws://127.0.0.1:47000',
          '--es',
          'chat_name',
          name,
        ]);
        var appPid = '';
        await run.until(() async {
          final result = await run.command(sdk.adb, [
            ...adb,
            'shell',
            'pidof',
            package,
          ], requireSuccess: false);
          appPid = (result.stdout as String).trim();
          return RegExp(r'^\d+$').hasMatch(appPid);
        }, const Duration(seconds: 15));
        activity['pid'] = appPid;
        run.save();
        await run.start(sdk.adb, [
          ...adb,
          'logcat',
          '--pid=$appPid',
          '-v',
          'threadtime',
        ], logDir: logs);
        final watching = Completer<void>();
        run.monitorStops.add(watching);
        run.monitorTasks.add(() async {
          try {
            while (!watching.isCompleted && !run.token.isCancelled) {
              await Future.any([
                Future<void>.delayed(const Duration(seconds: 1)),
                watching.future,
                run.token.cancelled,
              ]);
              if (watching.isCompleted || run.token.isCancelled) return;
              final result = await run.command(sdk.adb, [
                ...adb,
                'shell',
                'pidof',
                package,
              ], requireSuccess: false);
              if ((result.stdout as String).trim() != appPid) {
                throw StateError('Android owned activity exited');
              }
            }
          } catch (error, stack) {
            if (error is! RunCancelled ||
                (!watching.isCompleted && !run.token.isCancelled)) {
              run.fail(error, stack);
            }
          }
        }());
        run.owned.add('Android diagnostics', () async {
          final screenshot = '/sdcard/chat-e2e-$name.png';
          await run.command(sdk.adb, [
            ...adb,
            'shell',
            'screencap',
            '-p',
            screenshot,
          ], cleanup: true);
          try {
            await run.command(sdk.adb, [
              ...adb,
              'pull',
              screenshot,
              p.join(logs.path, '$name.png'),
            ], cleanup: true);
          } finally {
            await run.command(sdk.adb, [
              ...adb,
              'shell',
              'rm',
              screenshot,
            ], cleanup: true);
          }
        });
        participants.add(
          await run.wait(
            control.waitParticipant(name),
            timeout: const Duration(seconds: 30),
          ),
        );
      }
    }
    OwnedProcess? nodeProcess;
    Future<Participant> launchNode([Participant? old]) async {
      run.token.check();
      final previous = nodeProcess;
      if (previous != null) await run.stop(previous);
      run.token.check();
      nodeProcess = await run.start(node, [
        'puppet',
        '--control-url',
        'ws://127.0.0.1:47000',
        '--name',
        'node',
        '--data-dir',
        p.join(logs.path, 'node'),
      ], logDir: logs);
      Participant? next;
      await run.until(() async {
        next = await control.waitParticipant('node');
        return old == null || next!.instance != old.instance;
      }, const Duration(seconds: 15));
      return next!;
    }

    participants.add(await launchNode());
    await run.wait(
      runScenario(control, participants, restart: launchNode),
      timeout: const Duration(minutes: 4),
    );
  } catch (error, stack) {
    code = userInterrupted ? 130 : 1;
    run.token.cancel();
    await run.interrupt(controller);
    File(
      p.join(logs.path, 'failure.log'),
    ).writeAsStringSync('$error\n$stack', flush: true);
    stderr.writeln(
      '$profile scenario failed; details retained in ignored build/e2e logs.',
    );
  } finally {
    finished = true;
    await signal?.cancel();
    try {
      await run.stopMonitors();
    } catch (error) {
      run.cleanupFailures.add('monitor drain: $error');
    }
    await interrupted;
    await run.unwind();
    run.finishing = true;
    final failures = [...run.cleanupFailures, ...await run.owned.close()];
    if (failures.isNotEmpty) {
      File(
        p.join(logs.path, 'cleanup.log'),
      ).writeAsStringSync(failures.join('\n'), flush: true);
      if (code == 0) code = 1;
      stderr.writeln(
        'Owned cleanup failed; private ownership records retained.',
      );
    }
    if (run.firstFailure != null) {
      File(
        p.join(logs.path, 'lifecycle.log'),
      ).writeAsStringSync('${run.firstFailure}', flush: true);
      if (code == 0) code = 1;
    }
    if (userInterrupted && code == 0) code = 130;
    if (code == 0) {
      stdout.writeln(
        '$profile scenario PASS: six directions, offline recovery, no replay, Disconnect and fresh restart address.',
      );
    }
    stdout.writeln(
      'Run status: $code. Private diagnostics: build/e2e/<run>/$profile.',
    );
  }
  exitCode = code;
}

class _Run extends IoProcessRunner {
  _Run(this.logs) {
    unawaited(
      fatal.future.then<void>((_) {}, onError: (Object _, StackTrace _) {}),
    );
  }
  final Directory logs;
  final owned = OwnedResources();
  final token = RunCancellation();
  final active = <OwnedProcess>[];
  final stopping = <OwnedProcess>{};
  final processes = <Map<String, Object?>>[];
  final activities = <Map<String, Object?>>[];
  final mappings = <Map<String, Object?>>[];
  final cleanupFailures = <String>[];
  final fatal = Completer<Never>();
  final pending = <Future<dynamic>>{};
  final monitorStops = <Completer<void>>[];
  final monitorTasks = <Future<void>>[];
  Future<void>? _monitorClosing;
  Object? firstFailure;
  Future<void>? _interrupting;
  bool finishing = false;

  Future<void> stopMonitors() {
    // Close admission synchronously, then let already-started acquisitions
    // register their cleanup before OwnedResources closes its own admission.
    finishing = true;
    for (final stop in monitorStops) {
      if (!stop.isCompleted) stop.complete();
    }
    return _monitorClosing ??= Future.wait(monitorTasks).then<void>((_) {});
  }

  void save() {
    File(p.join(logs.path, 'ownership.json')).writeAsStringSync(
      jsonEncode({
        'version': 1,
        'ownerPid': pid,
        'processes': processes,
        'activities': activities,
        'reverse': mappings,
      }),
      flush: true,
    );
  }

  void annotate(int pid, Map<String, Object?> values) {
    processes.singleWhere((v) => v['pid'] == pid).addAll(values);
    save();
  }

  void fail(Object error, StackTrace stack) {
    firstFailure ??= error;
    if (!fatal.isCompleted) fatal.completeError(error, stack);
  }

  Future<T> wait<T>(
    Future<T> work, {
    Duration timeout = const Duration(seconds: 30),
    bool cleanup = false,
  }) async {
    if (cleanup) return work.timeout(timeout);
    pending.add(work);
    unawaited(
      work.then<void>(
        (_) {
          pending.remove(work);
        },
        onError: (Object _, StackTrace _) {
          pending.remove(work);
        },
      ),
    );
    try {
      token.check();
      return await Future.any<T>([
        work,
        fatal.future,
        token.cancelled.then<T>((_) => throw RunCancelled()),
      ]).timeout(timeout);
    } catch (_) {
      token.cancel();
      rethrow;
    }
  }

  Future<void> unwind() async {
    while (pending.isNotEmpty) {
      await Future.wait(
        pending.toList().map(
          (f) => f.then<void>((_) {}, onError: (Object _, StackTrace _) {}),
        ),
      );
    }
  }

  Future<void> until(Future<bool> Function() ready, Duration timeout) =>
      wait(() async {
        while (true) {
          token.check();
          if (await ready()) return;
          await Future<void>.delayed(const Duration(milliseconds: 100));
        }
      }(), timeout: timeout);

  Future<OwnedProcess> acquire(
    String executable,
    List<String> args, {
    String? cwd,
    String? stdin,
    bool persistent = false,
    bool cleanup = false,
  }) async {
    if (!cleanup) {
      token.check();
      if (finishing) throw RunCancelled();
    }
    File(p.join(logs.path, 'commands.jsonl')).writeAsStringSync(
      '${jsonEncode({'at': DateTime.now().toUtc().toIso8601String(), 'executable': executable, 'args': args, 'cwd': cwd})}\n',
      mode: FileMode.append,
      flush: true,
    );
    final process = await super.start(
      executable,
      args,
      logDir: logs,
      workingDirectory: cwd,
      stdin: stdin,
    );
    active.add(process);
    processes.add({'pid': process.pid, 'path': executable, 'cwd': cwd});
    try {
      // Final cleanup commands reclaim themselves; never reopen admission.
      if (!cleanup) owned.add('process', () => stop(process));
      save();
    } catch (error, stack) {
      try {
        await stop(process);
      } catch (secondary) {
        cleanupFailures.add('acquisition stop: $secondary');
      }
      Error.throwWithStackTrace(error, stack);
    }
    if (persistent) {
      unawaited(
        process.exitCode.then(
          (code) {
            if (!stopping.contains(process)) {
              fail(
                StateError('Owned child exited ($code)'),
                StackTrace.current,
              );
            }
          },
          onError: (Object error, StackTrace stack) {
            fail(error, stack);
          },
        ),
      );
    }
    if (!cleanup) token.check();
    return process;
  }

  @override
  Future<OwnedProcess> start(
    String executable,
    List<String> arguments, {
    required Directory logDir,
    String? workingDirectory,
    String? stdin,
    bool retainOutput = false,
  }) async => _Managed(
    await acquire(
      executable,
      arguments,
      cwd: workingDirectory,
      stdin: stdin,
      persistent: true,
    ),
    this,
  );
  Future<void> stop(OwnedProcess process) async {
    if (process is _Managed) {
      await process.stop();
      return;
    }
    if (!active.contains(process)) return;
    stopping.add(process);
    await process.stop();
    active.remove(process);
    processes.removeWhere((v) => v['pid'] == process.pid);
    save();
  }

  Future<void> interrupt(Controller? controller) =>
      _interrupting ??= _interrupt(controller);
  Future<void> _interrupt(Controller? controller) async {
    // Close event waits, then stop current owned processes; acquisition registers
    // before checking cancellation and the final unwind reclaims any late handle.
    try {
      await controller?.close();
    } catch (error) {
      cleanupFailures.add('controller interrupt: $error');
    }
    for (final process in active.reversed.toList()) {
      try {
        await stop(process);
      } catch (error) {
        cleanupFailures.add('interrupt: $error');
      }
    }
  }

  Future<ProcessResult> command(
    String executable,
    List<String> args, {
    String? cwd,
    String? stdin,
    Duration timeout = const Duration(seconds: 30),
    bool requireSuccess = true,
    bool cleanup = false,
  }) async {
    final process = await acquire(
      executable,
      args,
      cwd: cwd,
      stdin: stdin,
      cleanup: cleanup,
    );
    Object? primary;
    try {
      final code = await wait(
        process.exitCode,
        timeout: timeout,
        cleanup: cleanup,
      );
      final result = ProcessResult(
        process.pid,
        code,
        File(p.join(logs.path, '${process.pid}.stdout.log')).readAsStringSync(),
        File(p.join(logs.path, '${process.pid}.stderr.log')).readAsStringSync(),
      );
      if (requireSuccess && code != 0) {
        throw StateError('Command failed ($code): $executable');
      }
      return result;
    } catch (error) {
      primary = error;
      rethrow;
    } finally {
      try {
        await stop(process);
      } catch (error) {
        if (primary == null) rethrow;
        cleanupFailures.add('command stop: $error');
      }
    }
  }

  @override
  Future<ProcessResult> run(
    String executable,
    List<String> arguments, {
    String? stdin,
    String? workingDirectory,
  }) => command(
    executable,
    arguments,
    stdin: stdin,
    cwd: workingDirectory,
    requireSuccess: false,
  );
  Future<void> reverse(AndroidSdk sdk, EmulatorSpec spec, int port) async {
    final endpoint = 'tcp:$port';
    final adb = ['-s', spec.serial];
    final listed = await command(sdk.adb, [...adb, 'reverse', '--list']);
    for (final line in (listed.stdout as String).split('\n')) {
      final fields = line.trim().split(RegExp(r'\s+'));
      if (fields.length >= 3 && fields[1] == endpoint) {
        if (fields[2] != endpoint) {
          throw StateError('Existing reverse conflict');
        }
        return;
      }
    }
    await command(sdk.adb, [
      ...adb,
      'reverse',
      '--no-rebind',
      endpoint,
      endpoint,
    ]);
    final mapping = <String, Object?>{
      'serial': spec.serial,
      'local': endpoint,
      'remote': endpoint,
    };
    mappings.add(mapping);
    owned.add('reverse', () async {
      await command(sdk.adb, [
        ...adb,
        'reverse',
        '--remove',
        endpoint,
      ], cleanup: true);
      mappings.remove(mapping);
      save();
    });
    save();
    token.check();
  }
}

class _Managed implements OwnedProcess {
  _Managed(this.process, this.run);
  final OwnedProcess process;
  final _Run run;
  Future<void>? _stopping;
  @override
  int get pid => process.pid;
  @override
  Future<int> get exitCode => process.exitCode;
  @override
  Future<void> stop() => _stopping ??= run.stop(process);
}
