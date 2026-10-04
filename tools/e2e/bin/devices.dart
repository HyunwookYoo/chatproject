import 'dart:io';

import 'package:args/args.dart';
import 'package:chat_e2e/src/android_sdk.dart';
import 'package:chat_e2e/src/emulators.dart';
import 'package:chat_e2e/src/process_runner.dart';
import 'package:path/path.dart' as p;

const usage = '''
Usage: dart run tools/e2e/bin/devices.dart <command> [options] [flutter test arguments]

Commands:
  ensure   create the test AVDs and their clean snapshots if missing
  up       boot the test emulators and wait until they finish booting
  down     stop the test emulators
  test     boot, run `flutter test <arguments> -d emulator-5554` in app/, then stop
           the emulators this command started

Options:
  --count <1|2>   how many test emulators (default 1)
  --keep          test: leave the emulators running afterwards
''';

Future<void> main(List<String> arguments) async {
  final parser = ArgParser()
    ..addOption('count', defaultsTo: '1', allowed: ['1', '2'])
    ..addFlag('keep', negatable: false);
  final ArgResults options;
  try {
    options = parser.parse(arguments);
  } on FormatException catch (e) {
    stderr.writeln('${e.message}\n\n$usage');
    exit(64);
  }
  if (options.rest.isEmpty) {
    stderr.write(usage);
    exit(64);
  }

  final specs = testEmulators.take(int.parse(options['count'] as String)).toList();
  final emulators = Emulators(sdk: AndroidSdk.locate(), runner: const IoProcessRunner(), avdHome: defaultAvdHome());

  switch (options.rest.first) {
    case 'ensure':
      for (final spec in specs) {
        await emulators.ensure(spec);
      }
    case 'up':
      for (final spec in specs) {
        await emulators.ensure(spec);
      }
      final started = await emulators.up(specs);
      stdout.writeln('ready: ${specs.map((s) => s.serial).join(' ')} (started ${started.length})');
    case 'down':
      for (final spec in specs) {
        if (await emulators.isRunning(spec)) await emulators.stop(spec);
      }
    case 'test':
      for (final spec in specs) {
        await emulators.ensure(spec);
      }
      final started = await emulators.up(specs);
      final appDir = p.normalize(p.join(p.dirname(Platform.script.toFilePath()), '..', '..', '..', 'app'));
      final flutterArgs = ['test', ...options.rest.skip(1), '-d', specs.first.serial];
      final code = await const IoProcessRunner().runInherited('flutter', flutterArgs, workingDirectory: appDir);
      if (!(options['keep'] as bool)) {
        for (final spec in started) {
          await emulators.stop(spec);
        }
      }
      exit(code);
    default:
      stderr.write(usage);
      exit(64);
  }
}
