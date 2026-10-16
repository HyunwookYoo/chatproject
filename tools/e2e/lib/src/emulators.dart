import 'dart:async';
import 'dart:io';

import 'package:path/path.dart' as p;

import 'android_sdk.dart';
import 'process_runner.dart';

/// One test emulator: an AVD name and the console port that fixes its adb serial.
class EmulatorSpec {
  const EmulatorSpec(this.avd, this.port);

  final String avd;
  final int port;

  String get serial => 'emulator-$port';
}

/// Test-only AVDs (test automation design 5). The user's own flutter_emulator* AVDs
/// are never started or stopped by this tool.
const testEmulators = [EmulatorSpec('chat_e2e_1', 5554), EmulatorSpec('chat_e2e_2', 5556)];

/// Snapshot taken right after the first full boot; every test boot starts from it.
const cleanSnapshot = 'clean';

/// Arguments for a test boot: clean snapshot, no window, changes thrown away on exit.
List<String> bootArgs(EmulatorSpec spec) => [
      '-avd', spec.avd,
      '-port', '${spec.port}',
      '-snapshot', cleanSnapshot,
      '-no-snapshot-save',
      '-no-window', '-no-audio', '-no-boot-anim',
    ];

/// Arguments for the one-time full boot that produces the clean snapshot.
List<String> firstBootArgs(EmulatorSpec spec) => [
      '-avd', spec.avd,
      '-port', '${spec.port}',
      '-no-snapshot-load',
      '-no-snapshot-save',
      '-no-window', '-no-audio', '-no-boot-anim',
    ];

/// Parses `adb devices` output into serial → state ("device", "offline", ...).
Map<String, String> parseAdbDevices(String output) {
  final devices = <String, String>{};
  for (final line in output.split('\n').skip(1)) {
    final parts = line.trim().split(RegExp(r'\s+'));
    if (parts.length >= 2) devices[parts[0]] = parts[1];
  }
  return devices;
}

enum EmulatorFailureKind {
  listExit('avd_list_exit'),
  createExit('avd_create_exit'),
  devicesExit('avd_devices_exit'),
  consoleExit('avd_console_exit'),
  consoleInvalid('avd_console_invalid'),
  serialConflict('avd_serial_conflict'),
  configMissing('avd_config_missing'),
  configMismatch('avd_config_mismatch'),
  snapshotExit('avd_snapshot_exit'),
  snapshotMissing('avd_snapshot_missing');

  const EmulatorFailureKind(this.value);
  final String value;
}

/// Finite public metadata; the original StateError message stays private.
class EmulatorFailure extends StateError {
  EmulatorFailure(this.kind, String message, [this.code]) : super(message);
  final EmulatorFailureKind kind;
  final int? code;
}

class Emulators {
  Emulators({
    required this.sdk,
    required this.runner,
    required this.avdHome,
    this.flutter = 'flutter',
    this.pollInterval = const Duration(seconds: 2),
    this.bootTimeout = const Duration(seconds: 180),
    Future<void> Function(Duration)? sleep,
    DateTime Function()? now,
    AvdConfig? config,
    Directory? logDir,
    this.checkCancelled,
    this.onStarted,
    this.onStopped,
    this.retainOutput = false,
    Future<void> Function(int)? checkPort,
  })  : _sleep = sleep ?? Future<void>.delayed,
        _now = now ?? DateTime.now,
        config = config ?? AvdConfig.load(),
        logDir = logDir ?? Directory(p.join('build', 'e2e', 'devices')),
        _checkPort = checkPort ?? _requireFreePort;

  final AndroidSdk sdk;
  final ProcessRunner runner;
  final String avdHome;
  final String flutter;
  final Duration pollInterval;
  final Duration bootTimeout;
  final Future<void> Function(Duration) _sleep;
  final DateTime Function() _now;
  final AvdConfig config;
  final Directory logDir;
  final void Function()? checkCancelled;
  final Future<void> Function(EmulatorSpec, OwnedProcess)? onStarted;
  final Future<void> Function(EmulatorSpec)? onStopped;
  final bool retainOutput;
  final Future<void> Function(int) _checkPort;
  final _owned = <String, OwnedProcess>{};
  final cleanupFailures = <String>[];

  /// Intentionally transfer live emulators to the retained ownership record.
  void retainOwned() => _owned.clear();

  static Future<void> _requireFreePort(int port) async {
    final socket = await ServerSocket.bind(InternetAddress.loopbackIPv4, port, shared: false);
    await socket.close();
  }

  Future<ProcessResult> _run(String executable, List<String> args, {String? stdin}) async {
    checkCancelled?.call();
    final result = await runner.run(executable, args, stdin: stdin);
    checkCancelled?.call();
    return result;
  }

  void _requireSuccess(ProcessResult result, String operation, EmulatorFailureKind kind) {
    if (result.exitCode != 0) throw EmulatorFailure(kind, '$operation failed (${result.exitCode}): ${result.stderr}', result.exitCode);
  }

  Future<void> _start(EmulatorSpec spec, List<String> args) async {
    checkCancelled?.call();
    final process = await runner.start(sdk.emulator, args, logDir: logDir, retainOutput: retainOutput);
    _owned[spec.avd] = process;
    await onStarted?.call(spec, process);
    checkCancelled?.call();
  }

  /// Check all console and adb ports before any launch, including ensure boots.
  Future<void> preflight(List<EmulatorSpec> specs) async {
    for (final spec in specs) {
      checkCancelled?.call();
      if (!await isRunning(spec)) {
        await _checkPort(spec.port);
        await _checkPort(spec.port + 1);
      }
    }
  }

  Future<Map<String, String>> devices() async {
    final result = await _run(sdk.adb, ['devices']);
    _requireSuccess(result, 'adb devices', EmulatorFailureKind.devicesExit);
    return parseAdbDevices(result.stdout as String);
  }

  /// Name of the AVD on [spec]'s port, or null when adb does not list the port. An
  /// emulator that is still booting, or stuck offline, counts as listed.
  Future<String?> runningAvd(EmulatorSpec spec) async {
    if (!(await devices()).containsKey(spec.serial)) return null;
    final result = await _run(sdk.adb, ['-s', spec.serial, 'emu', 'avd', 'name']);
    final name = (result.stdout as String).split(RegExp(r'\r?\n')).first.trim();
    if (result.exitCode != 0 || name.isEmpty || name == 'OK') {
      throw EmulatorFailure(result.exitCode != 0 ? EmulatorFailureKind.consoleExit : EmulatorFailureKind.consoleInvalid,
        '${spec.serial} has a dead console; use manual adb diagnosis (adb -s <serial> emu avd name).',
        result.exitCode != 0 ? result.exitCode : null);
    }
    return name;
  }

  /// True when [spec] already runs, in any adb state. Throws when another AVD holds its
  /// port, so tests never run on (or stop) the user's own emulator.
  Future<bool> isRunning(EmulatorSpec spec) async {
    final name = await runningAvd(spec);
    if (name == null) return false;
    if (name != spec.avd) {
      throw EmulatorFailure(EmulatorFailureKind.serialConflict, '${spec.serial} is taken by AVD "$name" or has a dead console; use manual adb diagnosis before retrying.');
    }
    return true;
  }

  bool hasCleanSnapshot(EmulatorSpec spec) =>
      Directory(p.join(avdHome, '${spec.avd}.avd', 'snapshots', cleanSnapshot)).existsSync();

  /// Creates the AVD and its clean snapshot when they are missing.
  Future<void> ensure(EmulatorSpec spec) async {
    final list = await _run(sdk.emulator, ['-list-avds']);
    _requireSuccess(list, 'list-avds', EmulatorFailureKind.listExit);
    final avds = (list.stdout as String).split(RegExp(r'\r?\n')).map((line) => line.trim()).toSet();
    if (!avds.contains(spec.avd)) {
      final created = await _run(sdk.avdmanager,
        ['create', 'avd', '--name', spec.avd, '--package', config.package], stdin: 'no\n');
      _requireSuccess(created, 'create ${spec.avd}', EmulatorFailureKind.createExit);
    }
    _validateConfig(spec);
    if (hasCleanSnapshot(spec)) return;
    await preflight([spec]);
    final launched = !await isRunning(spec);
    try {
      if (launched) await _start(spec, firstBootArgs(spec));
      // An emulator that already runs may still be booting: wait for it too, so the
      // snapshot never captures a half-booted state.
      await waitBooted(spec);
      final saved = await _run(sdk.adb, ['-s', spec.serial, 'emu', 'avd', 'snapshot', 'save', cleanSnapshot]);
      _requireSuccess(saved, 'snapshot save', EmulatorFailureKind.snapshotExit);
      if (!hasCleanSnapshot(spec)) throw EmulatorFailure(EmulatorFailureKind.snapshotMissing, 'Snapshot save did not create the clean snapshot directory');
    } catch (_) {
      if (launched) await _stopQuietly([spec]);
      rethrow;
    }
    if (launched) await stop(spec);
  }

  void _validateConfig(EmulatorSpec spec) {
    final file = File(p.join(avdHome, '${spec.avd}.avd', 'config.ini'));
    if (!file.existsSync()) throw EmulatorFailure(EmulatorFailureKind.configMissing, 'Missing config.ini for ${spec.avd}');
    final values = <String, String>{};
    for (final line in file.readAsLinesSync()) {
      final separator = line.indexOf('=');
      if (separator > 0) values[line.substring(0, separator).trim()] = line.substring(separator + 1).trim();
    }
    final image = (values['image.sysdir.1'] ?? '').replaceAll('\\', '/').replaceFirst(RegExp(r'/+$'), '');
    final relative = 'system-images/android-${config.api}/${config.flavor}/${config.abi}';
    final absolute = sdk.systemImage(config).replaceAll('\\', '/');
    if ((image != relative && image != absolute) || values['abi.type'] != config.abi || values['hw.cpu.arch'] != config.abi) {
      throw EmulatorFailure(EmulatorFailureKind.configMismatch, '${spec.avd} has the wrong image or ABI; expected ${config.package}. Existing AVD was preserved.');
    }
  }

  /// Boots the emulators in [specs] that adb does not list yet and waits until every one of
  /// them has finished booting; returns the ones it started. Every port is checked before
  /// the first launch. If anything fails after a launch, the emulators it launched are
  /// stopped before the error is rethrown.
  Future<List<EmulatorSpec>> up(List<EmulatorSpec> specs) async {
    await preflight(specs);
    final toLaunch = <EmulatorSpec>[];
    for (final spec in specs) {
      if (!await isRunning(spec)) toLaunch.add(spec);
    }
    final started = <EmulatorSpec>[];
    try {
      for (final spec in toLaunch) {
        started.add(spec);
        await _start(spec, bootArgs(spec));
      }
      for (final spec in specs) {
        await waitBooted(spec);
      }
    } catch (_) {
      await _stopQuietly(started.reversed);
      rethrow;
    }
    return started;
  }

  Future<void> waitBooted(EmulatorSpec spec) async {
    final deadline = _now().add(bootTimeout);
    while (true) {
      final result = await _run(sdk.adb, ['-s', spec.serial, 'shell', 'getprop', 'sys.boot_completed']);
      if (result.exitCode == 0 && (result.stdout as String).trim() == '1') return;
      if (!_now().isBefore(deadline)) {
        throw TimeoutException('${spec.serial} did not finish booting within ${bootTimeout.inSeconds} s');
      }
      await _sleep(pollInterval);
    }
  }

  Future<void> stop(EmulatorSpec spec) async {
    final process = _owned[spec.avd];
    if (process == null) return;
    await process.stop();
    _owned.remove(spec.avd);
    await onStopped?.call(spec);
  }

  /// Stops [specs] after a failure. A stop that fails too must not hide the first error.
  Future<void> _stopQuietly(Iterable<EmulatorSpec> specs) async {
    for (final spec in specs) {
      try {
        await stop(spec);
      } catch (error) {
        cleanupFailures.add('emulator cleanup: $error');
      }
    }
  }
}
