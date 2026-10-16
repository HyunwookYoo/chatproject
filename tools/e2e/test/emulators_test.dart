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
  final stopped = <String>[];
  void Function()? onStarted;

  @override
  Future<OwnedProcess> start(String executable, List<String> arguments, {required Directory logDir, String? workingDirectory, String? stdin, bool retainOutput = false}) async {
    detached.add(arguments);
    onStarted?.call();
    final serial = 'emulator-${arguments[arguments.indexOf('-port') + 1]}';
    return FakeOwnedProcess(() async { stopped.add(serial); });
  }

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

class FakeOwnedProcess implements OwnedProcess {
  FakeOwnedProcess(this.onStop);
  final Future<void> Function() onStop;
  @override
  int get pid => 123;
  @override
  Future<int> get exitCode => Completer<int>().future;
  @override
  Future<void> stop() => onStop();
}

ProcessResult ok(String stdout) => ProcessResult(0, 0, stdout, '');
ProcessResult notReady() => ProcessResult(0, 1, '', "error: device 'emulator-5554' not found");

void writeConfig(Directory home, {String image = 'google_apis_playstore'}) {
  final folder = Directory('${home.path}/chat_e2e_1.avd')..createSync(recursive: true);
  File('${folder.path}/config.ini').writeAsStringSync(
    'image.sysdir.1=system-images/android-36/$image/x86_64/\nabi.type=x86_64\nhw.cpu.arch=x86_64\n');
}

void writeSnapshot(Directory home) =>
  Directory('${home.path}/chat_e2e_1.avd/snapshots/clean').createSync(recursive: true);

Emulators emulatorsWith(FakeRunner runner, {String avdHome = '/avd', DateTime Function()? now}) => Emulators(
      sdk: AndroidSdk('/sdk', windows: false),
      runner: runner,
      avdHome: avdHome,
      config: AvdConfig.load(ci: false),
      checkPort: (_) async {},
      sleep: (_) async {},
      now: now,
    );

/// `adb devices` on a machine where every emulator launched through [runner] stays
/// listed until its serial is in [killed].
ProcessResult listLaunched(FakeRunner runner, Set<String> killed) {
  final serials = runner.detached
      .map((args) => 'emulator-${args[args.indexOf('-port') + 1]}')
      .where((serial) => !killed.contains(serial));
  return ok('List of devices attached\n${serials.map((serial) => '$serial\tdevice\n').join()}');
}

void main() {
  for (final kind in ['avd_list_exit', 'avd_create_exit', 'avd_devices_exit',
    'avd_console_exit', 'avd_console_invalid', 'avd_serial_conflict',
    'avd_config_missing', 'avd_config_mismatch', 'avd_snapshot_exit', 'avd_snapshot_missing']) {
    test('$kind carries finite metadata at its real throw site', () async {
      final home = await Directory.systemTemp.createTemp('avd_diagnostic_');
      addTearDown(() => home.delete(recursive: true));
      if (kind != 'avd_config_missing') {
        writeConfig(home, image: kind == 'avd_config_mismatch' ? 'google_apis' : 'google_apis_playstore');
      }
      final runner = FakeRunner((exe, args) {
        if (args.contains('-list-avds')) {
          if (kind == 'avd_list_exit') return ProcessResult(0, -9, '', 'PRIVATE_TOKEN');
          return ok(kind == 'avd_create_exit' ? '' : 'chat_e2e_1\n');
        }
        if (args.contains('create')) return ProcessResult(0, -9, '', 'PRIVATE_TOKEN');
        if (args.first == 'devices') {
          if (kind == 'avd_devices_exit') return ProcessResult(0, -9, '', 'PRIVATE_TOKEN');
          return ok('List of devices attached\n${kind.startsWith('avd_console') || kind == 'avd_serial_conflict' ? 'emulator-5554\tdevice\n' : ''}');
        }
        if (args.contains('name')) {
          if (kind == 'avd_console_exit') return ProcessResult(0, -9, 'chat_e2e_1\n', 'PRIVATE_TOKEN');
          return ok(kind == 'avd_console_invalid' ? 'OK\n' : 'PRIVATE_TOKEN\nOK\n');
        }
        if (args.contains('getprop')) return ok('1\n');
        if (args.contains('save') && kind == 'avd_snapshot_exit') return ProcessResult(0, -9, '', 'PRIVATE_TOKEN');
        return ok('');
      });
      Object? caught;
      try {
        await emulatorsWith(runner, avdHome: home.path).ensure(testEmulators.first);
      } catch (error) {
        caught = error;
      }
      expect(caught, isA<StateError>());
      final dynamic error = caught;
      // Dynamic access keeps RED behavioral before the typed error exists.
      expect(() => error.kind.value, returnsNormally);
      expect(error.kind.value, kind);
      expect(error.code, kind.endsWith('_exit') ? -9 : null);
      final message = switch (kind) {
        'avd_list_exit' => 'list-avds failed (-9): PRIVATE_TOKEN',
        'avd_create_exit' => 'create chat_e2e_1 failed (-9): PRIVATE_TOKEN',
        'avd_devices_exit' => 'adb devices failed (-9): PRIVATE_TOKEN',
        'avd_console_exit' || 'avd_console_invalid' => 'emulator-5554 has a dead console; use manual adb diagnosis (adb -s <serial> emu avd name).',
        'avd_serial_conflict' => 'emulator-5554 is taken by AVD "PRIVATE_TOKEN" or has a dead console; use manual adb diagnosis before retrying.',
        'avd_config_missing' => 'Missing config.ini for chat_e2e_1',
        'avd_config_mismatch' => 'chat_e2e_1 has the wrong image or ABI; expected system-images;android-36;google_apis_playstore;x86_64. Existing AVD was preserved.',
        'avd_snapshot_exit' => 'snapshot save failed (-9): PRIVATE_TOKEN',
        _ => 'Snapshot save did not create the clean snapshot directory',
      };
      expect((caught as StateError).message, message);
      expect(caught.toString(), StateError(message).toString());
      expect(runner.stopped, kind.startsWith('avd_snapshot') ? ['emulator-5554'] : isEmpty);
      expect(File('${home.path}/chat_e2e_1.avd/config.ini').existsSync(), kind != 'avd_config_missing');
    });
  }

  test('a bound port fails preflight without killing its holder', () async {
    final holder = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
    final subscription = holder.listen((socket) => socket.destroy());
    addTearDown(() async { await subscription.cancel(); await holder.close(); });
    final runner = FakeRunner((exe, args) => ok('List of devices attached\n'));
    final emulators = Emulators(sdk: AndroidSdk('/sdk', windows: false), runner: runner, avdHome: '/avd');
    await expectLater(emulators.up([EmulatorSpec('chat_e2e_1', holder.port)]), throwsA(isA<SocketException>()));
    expect(runner.detached, isEmpty);
    final connection = await Socket.connect(InternetAddress.loopbackIPv4, holder.port);
    connection.destroy();
  });

  test('CI image selection is consumed by AVD creation and validation', () async {
    final home = await Directory.systemTemp.createTemp('avd_home_');
    addTearDown(() => home.delete(recursive: true));
    final runner = FakeRunner((exe, args) {
      if (args.contains('create')) { writeConfig(home, image: 'google_apis'); writeSnapshot(home); }
      return ok('');
    });
    final emulators = Emulators(sdk: AndroidSdk('/sdk', windows: false), runner: runner,
      avdHome: home.path, config: AvdConfig.load(ci: true));
    await emulators.ensure(testEmulators.first);
    expect(runner.calls.any((call) => call.endsWith('--package system-images;android-36;google_apis;x86_64')), isTrue);
    expect(runner.detached, isEmpty);
  });

  for (final failure in ['save exit', 'missing snapshot', 'boot failure']) {
    test('ensure preserves a preexisting AVD on $failure', () async {
      final home = await Directory.systemTemp.createTemp('avd_home_');
      addTearDown(() => home.delete(recursive: true));
      writeConfig(home);
      var clock = DateTime(2026);
      final runner = FakeRunner((exe, args) {
        if (args.contains('-list-avds')) return ok('chat_e2e_1\n');
        if (args.first == 'devices') return ok('List of devices attached\nemulator-5554\tdevice\n');
        if (args.contains('name')) return ok('chat_e2e_1\nOK\n');
        if (args.contains('getprop')) {
          clock = clock.add(const Duration(minutes: 4));
          return ok(failure == 'boot failure' ? '0\n' : '1\n');
        }
        if (args.contains('save') && failure == 'save exit') return ProcessResult(0, 9, '', 'snapshot rejected');
        return ok('');
      });
      await expectLater(emulatorsWith(runner, avdHome: home.path, now: () => clock).ensure(testEmulators.first), throwsA(anything));
      expect(runner.stopped, isEmpty);
      expect(runner.calls.any((call) => call.contains('emu kill')), isFalse);
    });
  }

  test('ensure rejects wrong image before launching or snapshotting', () async {
    final home = await Directory.systemTemp.createTemp('avd_home_');
    addTearDown(() => home.delete(recursive: true));
    writeConfig(home, image: 'google_apis');
    final runner = FakeRunner((exe, args) => ok(args.contains('-list-avds') ? 'chat_e2e_1\n' : ''));
    await expectLater(emulatorsWith(runner, avdHome: home.path).ensure(testEmulators.first), throwsStateError);
    expect(runner.detached, isEmpty);
  });

  test('ensure rejects list-avds failure before creation', () async {
    final runner = FakeRunner((exe, args) => ProcessResult(0, 9, '', 'list failed'));
    await expectLater(emulatorsWith(runner).ensure(testEmulators.first), throwsStateError);
    expect(runner.calls, hasLength(1));
  });

  test('empty console gets manual diagnosis without adb server reset', () async {
    final runner = FakeRunner((exe, args) => ok(args.first == 'devices' ? 'List of devices attached\nemulator-5554\tdevice\n' : '\nOK\n'));
    await expectLater(emulatorsWith(runner).up([testEmulators.first]),
      throwsA(isA<StateError>().having((e) => e.message, 'diagnostic', contains('manual adb'))));
    expect(runner.calls.any((call) => call.contains('kill-server')), isFalse);
  });

  test('failed boot stops managed process before adb ever lists it', () async {
    var clock = DateTime(2026);
    final runner = FakeRunner((exe, args) {
      if (args.first == 'devices') return ok('List of devices attached\n');
      clock = clock.add(const Duration(minutes: 4));
      return notReady();
    });
    await expectLater(emulatorsWith(runner, now: () => clock).up([testEmulators.first]), throwsA(isA<TimeoutException>()));
    expect(runner.stopped, ['emulator-5554']);
  });
  test('parses adb devices output', () {
    const out = 'List of devices attached\nemulator-5554\tdevice\r\nPHONESERIAL\tunauthorized\n\n';
    expect(parseAdbDevices(out), {'emulator-5554': 'device', 'PHONESERIAL': 'unauthorized'});
  });

  test('test boots use the clean snapshot and throw changes away', () {
    final args = bootArgs(testEmulators.first);
    expect(args, containsAllInOrder(['-avd', 'chat_e2e_1', '-port', '5554']));
    expect(args, containsAll(['-snapshot', 'clean', '-no-snapshot-save', '-no-window']));
    expect(firstBootArgs(testEmulators.first), contains('-no-snapshot-load'));
    expect(firstBootArgs(testEmulators.first), contains('-no-snapshot-save'));
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

  test('up checks every port before it launches anything', () async {
    final runner = FakeRunner((exe, args) {
      if (args.first == 'devices') return ok('List of devices attached\nemulator-5556\tdevice\n');
      if (args.contains('name')) return ok('flutter_emulator\r\nOK\r\n');
      return ok('');
    });
    await expectLater(
      emulatorsWith(runner).up(testEmulators),
      throwsA(isA<StateError>().having((e) => e.message, 'message', contains('emulator-5556'))),
    );
    expect(runner.detached, isEmpty);
  });

  test('up stops the emulators it launched when a later one fails to boot', () async {
    var clock = DateTime(2026);
    final killed = <String>{};
    late final FakeRunner runner;
    runner = FakeRunner((exe, args) {
      if (args.first == 'devices') return listLaunched(runner, killed);
      if (args.contains('getprop')) {
        if (args.contains('emulator-5554')) return ok('1\n');
        clock = clock.add(const Duration(seconds: 61));
        return ok('0\n');
      }
      if (args.contains('kill')) killed.add(args[1]);
      return ok('');
    });
    await expectLater(
      emulatorsWith(runner, now: () => clock).up(testEmulators),
      throwsA(isA<TimeoutException>()),
    );
    expect(runner.stopped, ['emulator-5556', 'emulator-5554']);
  });

  test('up waits for an emulator that adb lists but has not finished booting', () async {
    var polls = 0;
    final runner = FakeRunner((exe, args) {
      if (args.first == 'devices') return ok('List of devices attached\nemulator-5554\toffline\n');
      if (args.contains('name')) return ok('chat_e2e_1\r\nOK\r\n');
      if (args.contains('getprop')) return ++polls < 3 ? notReady() : ok('1\n');
      return ok('');
    });
    final started = await emulatorsWith(runner).up([testEmulators.first]);
    expect(started, isEmpty);
    expect(runner.detached, isEmpty);
    expect(polls, 3);
  });

  test('ensure creates a missing AVD and saves its clean snapshot', () async {
    final avdHome = await Directory.systemTemp.createTemp('avd_home_');
    addTearDown(() => avdHome.delete(recursive: true));
    final runner = FakeRunner((exe, args) {
      if (args.first == 'devices') return ok('List of devices attached\n');
      if (args.contains('getprop')) return ok('1\n');
      if (args.contains('create')) writeConfig(avdHome);
      if (args.contains('save')) writeSnapshot(avdHome);
      return ok('');
    });
    await emulatorsWith(runner, avdHome: avdHome.path).ensure(testEmulators.first);
    expect(runner.calls.any((call) => call.endsWith('create avd --name chat_e2e_1 --package system-images;android-36;google_apis_playstore;x86_64')), isTrue);
    expect(runner.detached.single, contains('-no-snapshot-load'));
    expect(runner.calls.any((c) => c.endsWith('-s emulator-5554 emu avd snapshot save clean')), isTrue);
    expect(runner.stopped, ['emulator-5554']);
  });

  test('ensure waits for a running emulator to finish booting before it saves the snapshot', () async {
    final avdHome = await Directory.systemTemp.createTemp('avd_home_');
    addTearDown(() => avdHome.delete(recursive: true));
    var polls = 0;
    writeConfig(avdHome);
    var killed = false;
    final runner = FakeRunner((exe, args) {
      if (args.contains('-list-avds')) return ok('chat_e2e_1\n');
      if (args.first == 'devices') return ok('List of devices attached\n${killed ? '' : 'emulator-5554\toffline\n'}');
      if (args.contains('name')) return ok('chat_e2e_1\r\nOK\r\n');
      if (args.contains('getprop')) return ++polls < 3 ? notReady() : ok('1\n');
      if (args.contains('kill')) killed = true;
      if (args.contains('save')) writeSnapshot(avdHome);
      return ok('');
    });
    await emulatorsWith(runner, avdHome: avdHome.path).ensure(testEmulators.first);
    expect(runner.detached, isEmpty);
    expect(polls, 3);
    final saved = runner.calls.indexWhere((c) => c.endsWith('emu avd snapshot save clean'));
    expect(saved, greaterThan(runner.calls.lastIndexWhere((c) => c.contains('getprop'))));
    expect(killed, isFalse, reason: 'ensure must preserve an already running AVD');
  });

  test('ensure stops the emulator it launched when the first boot never finishes', () async {
    final avdHome = await Directory.systemTemp.createTemp('avd_home_');
    addTearDown(() => avdHome.delete(recursive: true));
    var clock = DateTime(2026);
    writeConfig(avdHome);
    final killed = <String>{};
    late final FakeRunner runner;
    runner = FakeRunner((exe, args) {
      if (args.contains('-list-avds')) return ok('chat_e2e_1\n');
      if (args.first == 'devices') return listLaunched(runner, killed);
      if (args.contains('getprop')) {
        clock = clock.add(const Duration(seconds: 61));
        return ok('0\n');
      }
      if (args.contains('kill')) killed.add(args[1]);
      return ok('');
    });
    await expectLater(
      emulatorsWith(runner, avdHome: avdHome.path, now: () => clock).ensure(testEmulators.first),
      throwsA(isA<TimeoutException>()),
    );
    expect(runner.stopped, ['emulator-5554']);
    expect(runner.calls.any((c) => c.contains('snapshot save')), isFalse);
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
