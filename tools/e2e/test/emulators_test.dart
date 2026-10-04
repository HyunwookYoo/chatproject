import 'dart:async';
import 'dart:io';

import 'package:chat_e2e/src/android_sdk.dart';
import 'package:chat_e2e/src/emulators.dart';
import 'package:chat_e2e/src/process_runner.dart';
import 'package:test/test.dart';

/// Answers every command from [respond] and records what was run.
class FakeRunner implements ProcessRunner {
  FakeRunner(this.respond);

  final ProcessResult Function(String executable, List<String> arguments) respond;
  final calls = <String>[];
  final detached = <List<String>>[];

  @override
  Future<ProcessResult> run(String executable, List<String> arguments,
      {String? stdin, String? workingDirectory}) async {
    calls.add([executable, ...arguments].join(' '));
    return respond(executable, arguments);
  }

  @override
  Future<void> startDetached(String executable, List<String> arguments) async {
    detached.add(arguments);
  }

  @override
  Future<int> runInherited(String executable, List<String> arguments, {String? workingDirectory}) async => 0;
}

ProcessResult ok(String stdout) => ProcessResult(0, 0, stdout, '');
ProcessResult notReady() => ProcessResult(0, 1, '', "error: device 'emulator-5554' not found");

Emulators emulatorsWith(FakeRunner runner, {String avdHome = '/avd', DateTime Function()? now}) => Emulators(
      sdk: AndroidSdk('/sdk', windows: false),
      runner: runner,
      avdHome: avdHome,
      sleep: (_) async {},
      now: now,
    );

void main() {
  test('parses adb devices output', () {
    const out = 'List of devices attached\nemulator-5554\tdevice\r\nPHONESERIAL\tunauthorized\n\n';
    expect(parseAdbDevices(out), {'emulator-5554': 'device', 'PHONESERIAL': 'unauthorized'});
  });

  test('test boots use the clean snapshot and throw changes away', () {
    final args = bootArgs(testEmulators.first);
    expect(args, containsAllInOrder(['-avd', 'chat_e2e_1', '-port', '5554']));
    expect(args, containsAll(['-snapshot', 'clean', '-no-snapshot-save', '-no-window']));
    expect(firstBootArgs(testEmulators.first), contains('-no-snapshot-load'));
  });

  test('waitBooted returns once boot_completed is 1', () async {
    var polls = 0;
    final runner = FakeRunner((exe, args) {
      if (args.contains('getprop')) return ++polls < 3 ? notReady() : ok('1\n');
      return ok('');
    });
    await emulatorsWith(runner).waitBooted(testEmulators.first);
    expect(polls, 3);
  });

  test('waitBooted gives up after the boot timeout', () async {
    var clock = DateTime(2026);
    final runner = FakeRunner((exe, args) {
      clock = clock.add(const Duration(seconds: 61));
      return ok('0\n');
    });
    await expectLater(
      emulatorsWith(runner, now: () => clock).waitBooted(testEmulators.first),
      throwsA(isA<TimeoutException>()),
    );
  });

  test('up leaves an already running test emulator alone', () async {
    final runner = FakeRunner((exe, args) {
      if (args.first == 'devices') return ok('List of devices attached\nemulator-5554\tdevice\n');
      if (args.contains('name')) return ok('chat_e2e_1\r\nOK\r\n');
      if (args.contains('getprop')) return ok('1\n');
      return ok('');
    });
    final started = await emulatorsWith(runner).up(testEmulators);
    expect(started.map((s) => s.avd), ['chat_e2e_2']);
    expect(runner.detached.single, containsAllInOrder(['-avd', 'chat_e2e_2']));
  });

  test('up refuses a port taken by another AVD', () async {
    final runner = FakeRunner((exe, args) {
      if (args.first == 'devices') return ok('List of devices attached\nemulator-5554\tdevice\n');
      if (args.contains('name')) return ok('flutter_emulator\r\nOK\r\n');
      return ok('');
    });
    await expectLater(emulatorsWith(runner).up([testEmulators.first]), throwsA(isA<StateError>()));
    expect(runner.detached, isEmpty);
  });

  test('ensure creates a missing AVD and saves its clean snapshot', () async {
    final avdHome = await Directory.systemTemp.createTemp('avd_home_');
    addTearDown(() => avdHome.delete(recursive: true));
    final runner = FakeRunner((exe, args) {
      if (args.first == 'devices') return ok('List of devices attached\n');
      if (args.contains('getprop')) return ok('1\n');
      return ok('');
    });
    await emulatorsWith(runner, avdHome: avdHome.path).ensure(testEmulators.first);
    expect(runner.calls, contains('flutter emulators --create --name chat_e2e_1'));
    expect(runner.detached.single, contains('-no-snapshot-load'));
    expect(runner.calls.any((c) => c.endsWith('-s emulator-5554 emu avd snapshot save clean')), isTrue);
    expect(runner.calls.any((c) => c.endsWith('-s emulator-5554 emu kill')), isTrue);
  });

  test('locates the SDK from ANDROID_HOME first', () {
    final sdk = AndroidSdk.locate(environment: {'ANDROID_HOME': '/opt/sdk', 'HOME': '/home/u'}, windows: false);
    expect(sdk.root, '/opt/sdk');
  });

  test('finds the AVD folder from ANDROID_AVD_HOME, then the home folder', () {
    expect(defaultAvdHome(environment: {'ANDROID_AVD_HOME': '/avds'}, windows: false), '/avds');
    expect(defaultAvdHome(environment: {'HOME': '/home/u'}, windows: false), endsWith('avd'));
  });
}
