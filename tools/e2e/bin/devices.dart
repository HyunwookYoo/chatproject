import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:isolate';

import 'package:args/args.dart';
import 'package:chat_e2e/src/android_sdk.dart';
import 'package:chat_e2e/src/emulators.dart';
import 'package:chat_e2e/src/owned_resources.dart';
import 'package:chat_e2e/src/process_runner.dart';
import 'package:path/path.dart' as p;

const usage = '''
Usage: dart run tools/e2e/bin/devices.dart <command> [options] [-- flutter test arguments]

Commands:
  ensure   create the test AVDs and their clean snapshots if missing
  up       boot test emulators, retaining ownership records for manual cleanup
  down     explicitly clean ownership records; live stale processes require manual cleanup
  test     boot, run flutter test in app/, then clean resources this run acquired

Options:
  --count <1|2>   how many test emulators (default 1)
  --keep          test: leave this run's emulators running afterwards
  --              forward all following arguments literally to flutter test
''';

Future<void> main(List<String> arguments) async {
  try {
    exit(await runDevices(arguments, runner: const IoProcessRunner(), sdk: AndroidSdk.locate()));
  } catch (_) {
    stderr.writeln('Device tool initialization failed. Check Android SDK configuration.');
    exit(1);
  }
}

Future<int> runDevices(List<String> arguments, {
  required ProcessRunner runner,
  required AndroidSdk sdk,
  String? avdHome,
  Directory? logDir,
  RunCancellation? cancellation,
  Duration commandTimeout = const Duration(seconds: 30),
}) async {
  final separator = arguments.indexOf('--');
  final forwarded = separator < 0 ? <String>[] : arguments.sublist(separator + 1);
  final ownArgs = separator < 0 ? arguments : arguments.sublist(0, separator);
  final parser = ArgParser()
    ..addOption('count', defaultsTo: '1', allowed: ['1', '2'])
    ..addFlag('keep', negatable: false);
  final ArgResults options;
  try {
    options = parser.parse(ownArgs);
    if (options.rest.length != 1 || !['ensure', 'up', 'down', 'test'].contains(options.rest.single) ||
        (forwarded.isNotEmpty && options.rest.single != 'test')) {
      throw const FormatException('Expected one command; flutter arguments must follow --.');
    }
  } on FormatException catch (error) {
    stderr.writeln('${error.message}\n$usage');
    return 64;
  }

  final packageSource = Isolate.resolvePackageUriSync(Uri.parse('package:chat_e2e/src/android_sdk.dart'))!;
  final repo = p.normalize(p.join(p.dirname(packageSource.toFilePath()), '..', '..', '..', '..'));
  final root = logDir ?? Directory(p.join(repo, 'build', 'e2e', 'devices'));
  await root.create(recursive: true);
  final stale = root.listSync(recursive: true).whereType<File>().where((file) => p.basename(file.path) == 'ownership.json').toList();
  if (options.rest.single == 'down') return _cleanStale(stale, runner);
  if (stale.isNotEmpty) stderr.writeln('Stale ownership records found. Use down for explicit safe cleanup; live or uncertain resources need manual diagnosis.');
  final runDir = await Directory(p.join(root.path, '${DateTime.now().microsecondsSinceEpoch}')).create();
  final ownership = _Ownership(File(p.join(runDir.path, 'ownership.json')));
  final owned = OwnedResources();
  final token = cancellation ?? RunCancellation();
  final active = <OwnedProcess>[];
  final interruptFailures = <String>[];
  final commands = _Commands(runner, runDir, token, owned, ownership, interruptFailures, commandTimeout);
  StreamSubscription<ProcessSignal>? signal;
  var finished = false;
  Future<void>? interruptCleanup;
  token.cancelled.then((_) {
    if (finished) return;
    interruptCleanup = () async {
      await commands.interrupt();
      for (final process in active.reversed.toList()) {
        try { await process.stop(); } catch (error) { interruptFailures.add('interrupt cleanup: $error'); }
      }
    }();
  });
  try {
    signal = ProcessSignal.sigint.watch().listen((_) => token.cancel());
  } on UnsupportedError {
    stderr.writeln('This host does not support handled Ctrl+C. Hard termination leaves ownership records.');
  }
  final specs = testEmulators.take(int.parse(options['count'] as String)).toList();
  late final Emulators emulators;
  emulators = Emulators(
    sdk: sdk, runner: commands, avdHome: avdHome ?? defaultAvdHome(), logDir: runDir,
    checkCancelled: token.check,
    retainOutput: options.rest.single == 'up' || options['keep'] as bool,
    onStarted: (spec, process) async {
      active.add(process);
      owned.add('emulator', () async { await emulators.stop(spec); active.remove(process); });
      ownership.processes.add({'pid': process.pid, 'path': sdk.emulator, 'avd': spec.avd});
      ownership.save();
    },
    onStopped: (spec) async {
      ownership.processes.removeWhere((entry) => entry['avd'] == spec.avd);
      ownership.save();
    },
  );
  var code = 0;
  var retainEmulators = false;
  try {
    token.check();
    // Preflight the entire set before even the first one-time snapshot boot.
    await emulators.preflight(specs);
    for (final spec in specs) { await emulators.ensure(spec); }
    switch (options.rest.single) {
      case 'ensure':
        break;
      case 'up':
        await emulators.up(specs);
        token.check();
        retainEmulators = true;
        stdout.writeln('Test emulators ready. Ownership records retained.');
      case 'test':
        await emulators.up(specs);
        for (final spec in specs) {
          for (final port in [47000, 3340]) {
            await _reverse(commands, sdk, spec, port, token, owned, ownership);
          }
        }
        token.check();
        final executable = Platform.isWindows ? 'flutter.bat' : 'flutter';
        final process = await runner.start(executable,
          ['test', ...forwarded, '-d', specs.first.serial], logDir: runDir, workingDirectory: p.join(repo, 'app'));
        active.add(process);
        owned.add('flutter test', () async {
          await process.stop();
          active.remove(process);
          ownership.processes.removeWhere((entry) => entry['pid'] == process.pid && entry['avd'] == null);
          ownership.save();
        });
        ownership.processes.add({'pid': process.pid, 'path': executable});
        ownership.save();
        token.check();
        // This terminal wait races cancellation. The body itself unwinds
        // before final cleanup, and no later acquisition remains behind it.
        code = await Future.any([process.exitCode, token.cancelled.then((_) => 130)]);
        if (token.isCancelled) code = 130;
        retainEmulators = options['keep'] as bool && !token.isCancelled;
    }
  } on RunCancelled {
    code = 130;
  } catch (error, stack) {
    code = 1;
    File(p.join(runDir.path, 'failure.log')).writeAsStringSync('$error\n$stack');
    stderr.writeln('Device command failed. Details are in the ignored run logs.');
  } finally {
    // No body continuation can acquire resources after this point.
    finished = true;
    await signal?.cancel();
    await interruptCleanup;
    if (token.isCancelled) {
      retainEmulators = false;
      if (code == 0) code = 130;
    }
    // Retention is only for successful up / explicitly requested --keep.
    // Remove emulator cleanup callbacks by making their stop a no-op below.
    if (retainEmulators) emulators.retainOwned();
    final cleanup = await owned.close();
    final failures = [...interruptFailures, ...emulators.cleanupFailures, ...cleanup];
    if (failures.isNotEmpty) {
      File(p.join(runDir.path, 'cleanup.log')).writeAsStringSync(failures.join('\n'));
      stderr.writeln('Resource cleanup failed; ownership records and diagnostics were retained.');
      if (code == 0) code = 1;
    }
  }
  return code;
}

Future<void> _reverse(_Commands runner, AndroidSdk sdk, EmulatorSpec spec, int port,
    RunCancellation token, OwnedResources owned, _Ownership ownership) async {
  final endpoint = 'tcp:$port';
  token.check();
  final listed = await runner.run(sdk.adb, ['-s', spec.serial, 'reverse', '--list']);
  token.check();
  if (listed.exitCode != 0) throw StateError('adb reverse list failed (${listed.exitCode})');
  for (final line in (listed.stdout as String).split('\n')) {
    final fields = line.trim().split(RegExp(r'\s+'));
    if (fields.length >= 3 && fields[1] == endpoint) {
      if (fields[2] != endpoint) throw StateError('A preexisting adb reverse uses a different destination');
      return;
    }
  }
  token.check();
  final ProcessResult added;
  try {
    added = await runner.run(sdk.adb, ['-s', spec.serial, 'reverse', '--no-rebind', endpoint, endpoint]);
  } catch (error, stack) {
    // An interrupted add may have applied, but never remove an unconfirmed
    // mapping: a concurrent foreign no-rebind acquisition must stay protected.
    ownership.reverse.add({'serial': spec.serial, 'local': endpoint, 'remote': endpoint, 'uncertain': true});
    try { ownership.save(); } catch (secondary) { runner.failures.add('uncertain reverse ownership: $secondary'); }
    Error.throwWithStackTrace(error, stack);
  }
  if (added.exitCode != 0) throw StateError('adb reverse add failed (${added.exitCode})');
  final entry = <String, Object>{'serial': spec.serial, 'local': endpoint, 'remote': endpoint};
  ownership.reverse.add(entry);
  owned.add('adb reverse', () async {
    final result = await runner.command(sdk.adb, ['-s', spec.serial, 'reverse', '--remove', endpoint], cleanup: true);
    if (result.exitCode != 0) throw StateError('adb reverse remove failed (${result.exitCode})');
    ownership.reverse.remove(entry);
    ownership.save();
  });
  ownership.save();
  token.check();
}

/// Short commands keep exact managed handles until reclamation is confirmed.
/// Acquisition itself is awaited so a late handle cannot outlive final unwind.
class _Commands implements ProcessRunner {
  _Commands(this.runner, this.logs, this.token, this.owned, this.ownership, this.failures, this.timeout);
  final ProcessRunner runner;
  final Directory logs;
  final RunCancellation token;
  final OwnedResources owned;
  final _Ownership ownership;
  final List<String> failures;
  final Duration timeout;
  final active = <OwnedProcess>[];

  void track(OwnedProcess process, String executable, {required bool cleanup}) {
    active.add(process);
    ownership.processes.add({'pid': process.pid, 'path': executable});
    if (!cleanup) owned.add('command', () => stop(process));
    ownership.save();
  }

  Future<void> stop(OwnedProcess process) async {
    if (!active.contains(process)) return;
    await process.stop();
    active.remove(process);
    ownership.processes.removeWhere((entry) => entry['pid'] == process.pid && entry['avd'] == null);
    ownership.save();
  }

  Future<void> interrupt() async {
    for (final process in active.reversed.toList()) {
      try { await stop(process); } catch (error) { failures.add('command interrupt: $error'); }
    }
  }

  Future<ProcessResult> command(String executable, List<String> arguments, {
    String? stdin, String? workingDirectory, bool cleanup = false,
  }) async {
    if (!cleanup) token.check();
    final OwnedProcess process;
    try {
      process = await runner.start(executable, arguments, logDir: logs,
        stdin: stdin, workingDirectory: workingDirectory);
    } on OwnedProcessStartFailure catch (failure) {
      failures.add('command startup stop: ${failure.cleanupFailure}');
      if (failure.diagnosticFailure != null) failures.add('command startup diagnostic: ${failure.diagnosticFailure}');
      try { track(failure.process, executable, cleanup: cleanup); }
      catch (error) { failures.add('command startup ownership: $error'); }
      Error.throwWithStackTrace(failure.primary, failure.primaryStack);
    }
    Object? primary;
    try {
      track(process, executable, cleanup: cleanup);
      if (!cleanup) token.check();
      final code = await (cleanup ? process.exitCode : Future.any<int>([
        process.exitCode, token.cancelled.then<int>((_) => throw RunCancelled()),
      ])).timeout(timeout);
      return ProcessResult(process.pid, code,
        File(p.join(logs.path, '${process.pid}.stdout.log')).readAsStringSync(),
        File(p.join(logs.path, '${process.pid}.stderr.log')).readAsStringSync());
    } catch (error) {
      primary = error;
      rethrow;
    } finally {
      try { await stop(process); } catch (error) {
        if (primary == null) rethrow;
        failures.add('command stop: $error');
      }
    }
  }

  @override
  Future<ProcessResult> run(String executable, List<String> arguments, {String? stdin, String? workingDirectory}) =>
    command(executable, arguments, stdin: stdin, workingDirectory: workingDirectory);
  @override
  Future<OwnedProcess> start(String executable, List<String> arguments, {required Directory logDir,
    String? workingDirectory, String? stdin, bool retainOutput = false}) =>
    runner.start(executable, arguments, logDir: logDir, workingDirectory: workingDirectory,
      stdin: stdin, retainOutput: retainOutput);
  @override
  Future<void> startDetached(String executable, List<String> arguments) => runner.startDetached(executable, arguments);
  @override
  Future<int> runInherited(String executable, List<String> arguments, {String? workingDirectory}) =>
    runner.runInherited(executable, arguments, workingDirectory: workingDirectory);
}

class _Ownership {
  _Ownership(this.file);
  final File file;
  final processes = <Map<String, Object>>[];
  final reverse = <Map<String, Object>>[];
  void save() {
    if (processes.isEmpty && reverse.isEmpty) {
      if (file.existsSync()) file.deleteSync();
    } else {
      final temporary = File('${file.path}.tmp');
      temporary.writeAsStringSync(jsonEncode({'version': 1, 'ownerPid': pid, 'processes': processes, 'reverse': reverse}), flush: true);
      temporary.renameSync(file.path);
    }
  }
}

Future<int> _cleanStale(List<File> files, ProcessRunner runner) async {
  var code = 0;
  for (final file in files) {
    try {
      final record = jsonDecode(file.readAsStringSync()) as Map<String, dynamic>;
      final owner = record['ownerPid'];
      if (record['version'] != 1 || owner is! int || owner <= 0 || !await _knownAbsent(owner, runner)) {
        code = 1;
        continue;
      }
      final processes = record['processes'] as List<dynamic>;
      final remaining = <dynamic>[];
      for (final entry in processes) {
        final childPid = (entry as Map<String, dynamic>)['pid'];
        if (childPid is! int || childPid <= 0 || !await _knownAbsent(childPid, runner)) remaining.add(entry);
      }
      final reverse = record['reverse'] as List<dynamic>;
      if (remaining.isEmpty && reverse.isEmpty) {
        file.deleteSync();
      } else {
        record['processes'] = remaining;
        file.writeAsStringSync(jsonEncode(record), flush: true);
        code = 1;
      }
    } catch (_) { code = 1; }
  }
  if (code != 0) stderr.writeln('Live or uncertain stale resources remain. Inspect ignored ownership records and use manual process/adb diagnosis; no stale process was signaled.');
  return code;
}

Future<bool> _knownAbsent(int childPid, ProcessRunner runner) async {
  if (Platform.isLinux && Directory('/proc/self').existsSync()) return !Directory('/proc/$childPid').existsSync();
  if (Platform.isWindows) {
    final result = await runner.run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
      'try { Get-Process -Id $childPid -ErrorAction Stop | Out-Null; exit 0 } catch [Microsoft.PowerShell.Commands.ProcessCommandException] { if (\$_.FullyQualifiedErrorId -like "NoProcessFoundForGivenId*") { exit 3 }; exit 4 } catch { exit 4 }']);
    return result.exitCode == 3;
  }
  return false;
}
