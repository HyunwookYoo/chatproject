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
  })  : _sleep = sleep ?? Future<void>.delayed,
        _now = now ?? DateTime.now;

  final AndroidSdk sdk;
  final ProcessRunner runner;
  final String avdHome;
  final String flutter;
  final Duration pollInterval;
  final Duration bootTimeout;
  final Future<void> Function(Duration) _sleep;
  final DateTime Function() _now;

  Future<Map<String, String>> devices() async {
    final result = await runner.run(sdk.adb, ['devices']);
    return parseAdbDevices(result.stdout as String);
  }

  /// Name of the AVD running on [spec]'s port, or null when the port is free.
  Future<String?> runningAvd(EmulatorSpec spec) async {
    if ((await devices())[spec.serial] != 'device') return null;
    final result = await runner.run(sdk.adb, ['-s', spec.serial, 'emu', 'avd', 'name']);
    return (result.stdout as String).split(RegExp(r'\r?\n')).first.trim();
  }

  /// True when [spec] already runs. Throws when another AVD holds its port, so tests
  /// never run on (or stop) the user's own emulator.
  Future<bool> isRunning(EmulatorSpec spec) async {
    final name = await runningAvd(spec);
    if (name == null) return false;
    if (name != spec.avd) {
      throw StateError('${spec.serial} is taken by AVD "$name". Stop that emulator and run again.');
    }
    return true;
  }

  bool hasCleanSnapshot(EmulatorSpec spec) =>
      Directory(p.join(avdHome, '${spec.avd}.avd', 'snapshots', cleanSnapshot)).existsSync();

  /// Creates the AVD and its clean snapshot when they are missing.
  Future<void> ensure(EmulatorSpec spec) async {
    final list = await runner.run(sdk.emulator, ['-list-avds']);
    final avds = (list.stdout as String).split(RegExp(r'\r?\n')).map((line) => line.trim()).toSet();
    if (!avds.contains(spec.avd)) {
      final created = await runner.run(flutter, ['emulators', '--create', '--name', spec.avd]);
      if (created.exitCode != 0) throw StateError('Cannot create ${spec.avd}: ${created.stderr}');
    }
    if (hasCleanSnapshot(spec)) return;
    if (!await isRunning(spec)) {
      await runner.startDetached(sdk.emulator, firstBootArgs(spec));
      await waitBooted(spec);
    }
    await runner.run(sdk.adb, ['-s', spec.serial, 'emu', 'avd', 'snapshot', 'save', cleanSnapshot]);
    await stop(spec);
  }

  /// Boots the emulators in [specs] that are not running; returns the ones it started.
  Future<List<EmulatorSpec>> up(List<EmulatorSpec> specs) async {
    final started = <EmulatorSpec>[];
    for (final spec in specs) {
      if (await isRunning(spec)) continue;
      await runner.startDetached(sdk.emulator, bootArgs(spec));
      started.add(spec);
    }
    for (final spec in started) {
      await waitBooted(spec);
    }
    return started;
  }

  Future<void> waitBooted(EmulatorSpec spec) async {
    final deadline = _now().add(bootTimeout);
    while (true) {
      final result = await runner.run(sdk.adb, ['-s', spec.serial, 'shell', 'getprop', 'sys.boot_completed']);
      if (result.exitCode == 0 && (result.stdout as String).trim() == '1') return;
      if (!_now().isBefore(deadline)) {
        throw TimeoutException('${spec.serial} did not finish booting within ${bootTimeout.inSeconds} s');
      }
      await _sleep(pollInterval);
    }
  }

  Future<void> stop(EmulatorSpec spec) async {
    await runner.run(sdk.adb, ['-s', spec.serial, 'emu', 'kill']);
    final deadline = _now().add(const Duration(seconds: 30));
    while ((await devices()).containsKey(spec.serial)) {
      if (!_now().isBefore(deadline)) throw TimeoutException('${spec.serial} did not stop within 30 s');
      await _sleep(pollInterval);
    }
  }
}
