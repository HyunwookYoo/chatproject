import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:args/args.dart';
import 'package:chat_e2e/src/android_probe.dart';
import 'package:chat_e2e/src/android_sdk.dart';
import 'package:chat_e2e/src/controller.dart';
import 'package:chat_e2e/src/account_scenario.dart';
import 'package:chat_e2e/src/diagnostic.dart';
import 'package:chat_e2e/src/mailbox_scenario.dart';
import 'package:chat_e2e/src/message_scenario.dart';
import 'package:chat_e2e/src/emulators.dart';
import 'package:chat_e2e/src/owned_resources.dart';
import 'package:chat_e2e/src/process_runner.dart';
import 'package:chat_e2e/src/protocol.dart';
import 'package:chat_e2e/src/scenario.dart';
import 'package:path/path.dart' as p;

Map<String, String>? _androidBuildEnvironment(Map<String, String> inputs) {
  final perl = inputs['E2E_ANDROID_OPENSSL_PERL'];
  final make = inputs['E2E_ANDROID_MAKE'];
  final overlay = inputs['E2E_ANDROID_PERL_LIB'];
  if (perl == null && make == null && overlay == null) return null;
  if (!Platform.isWindows ||
      perl == null ||
      make == null ||
      overlay == null ||
      !p.isAbsolute(perl) ||
      !p.isAbsolute(make) ||
      !RegExp(r'^[a-zA-Z]:[\\/]').hasMatch(overlay) ||
      !File(perl).existsSync() ||
      !File(make).existsSync() ||
      !Directory(overlay).existsSync()) {
    throw StateError('Invalid Android build tool selection');
  }
  final nativeOverlay = p.normalize(overlay).replaceAll('\\', '/');
  final exclusions = (inputs['MSYS2_ENV_CONV_EXCL'] ?? '')
      .split(';')
      .where((value) => value.isNotEmpty)
      .toList();
  if (!exclusions.contains('PERL5LIB')) exclusions.add('PERL5LIB');
  return {
    'OPENSSL_SRC_PERL': p.normalize(perl).replaceAll('\\', '/'),
    'PERL5LIB':
        '/${nativeOverlay[0].toLowerCase()}${nativeOverlay.substring(2)}',
    'PATH': '${p.dirname(make)};${p.dirname(perl)};${inputs['PATH'] ?? ''}',
    'MSYS2_ENV_CONV_EXCL': exclusions.join(';'),
  };
}

int _auditRecoveryLogs(Directory logs, List<String> patterns) {
  var files = 0;
  for (final entry in logs.listSync(followLinks: false)) {
    if (entry is! File ||
        !const ['.log', '.json', '.jsonl'].contains(p.extension(entry.path))) {
      continue;
    }
    final text = utf8.decode(entry.readAsBytesSync(), allowMalformed: true);
    for (final pattern in patterns) {
      final quoted = jsonEncode(pattern);
      if (text.contains(pattern) ||
          text.contains(quoted.substring(1, quoted.length - 1))) {
        throw StateError('Recovery privacy audit failed');
      }
    }
    files++;
  }
  return files;
}

Future<void> main(List<String> arguments) async {
  final parser = ArgParser()
    ..addOption(
      'profile',
      defaultsTo: Platform.isWindows ? 'mixed' : 'android',
      allowed: ['mixed', 'android', 'windows'],
    )
    ..addOption(
      'scenario',
      defaultsTo: 'm2',
      allowed: ['m2', 'm3', 'm3-auth', 'm4', 'm6'],
    )
    ..addOption('repo-root', mandatory: true);
  late ArgResults options;
  try {
    options = parser.parse(arguments);
    if (options.rest.isNotEmpty ||
        !p.isAbsolute(options['repo-root'] as String) ||
        (!Platform.isWindows && options['profile'] != 'android') ||
        // M6 needs an Android phone: a Windows app or a second emulator is
        // its mailbox.
        (options['scenario'] == 'm6' && options['profile'] == 'windows')) {
      throw const FormatException(
        'Profile requires a supported host and absolute repo root',
      );
    }
  } on FormatException {
    stderr.writeln(
      'Usage: dart run bin/run.dart --profile mixed|android|windows --scenario m2|m3|m3-auth|m4|m6 --repo-root <absolute>',
    );
    exitCode = 64;
    return;
  }
  final root = p.normalize(options['repo-root'] as String);
  final profile = options['profile'] as String;
  final authOnly = options['scenario'] == 'm3-auth';
  final messageScenario = options['scenario'] == 'm4';
  final mailboxScenario = options['scenario'] == 'm6';
  final accountScenario = options['scenario'] != 'm2';
  final scenarioName = options['scenario'] as String;
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
  // Presence of Android crash-buffer entries only; the entries stay private.
  Future<bool?> Function()? androidCrashed;
  // Fixed public state of each owned emulator and its app (account scenarios).
  final androidProbes = <Future<Map<String, Object?>> Function()>[];
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
      run.recordFailure(RunCancelled());
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
    final stopCiGradle = usesOwnedCiGradle(profile, Platform.environment);
    if (Platform.isWindows &&
        (Platform.environment['CARGOKIT_TOOLCHAIN'] != '1.92.0' ||
            Platform.environment['CARGOKIT_VERBOSE'] != '1')) {
      throw StateError(
        'Set CARGOKIT_TOOLCHAIN=1.92.0 and CARGOKIT_VERBOSE=1 for Windows Debug builds',
      );
    }
    run.phase = _Phase.buildNode;
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
      run.phase = _Phase.buildWindows;
      await run.command(
        flutter,
        ['build', 'windows', '--debug'],
        cwd: p.join(root, 'app'),
        timeout: const Duration(minutes: 30),
      );
    }
    if (androidCount > 0) {
      run.phase = _Phase.buildAndroid;
      await run.command(
        flutter,
        ['build', 'apk', '--debug', '--target-platform', 'android-x64'],
        environment: _androidBuildEnvironment(Platform.environment),
        cwd: p.join(root, 'app'),
        timeout: const Duration(minutes: 30),
      );
      if (stopCiGradle) {
        run.phase = _Phase.gradleStop;
        await run.command(
          Platform.isWindows ? 'gradlew.bat' : './gradlew',
          ['--stop'],
          cwd: p.join(root, 'app', 'android'),
        );
      }
    }
    final node = p.join(
      root,
      'target',
      'debug',
      Platform.isWindows ? 'chat_node.exe' : 'chat_node',
    );
    run.phase = _Phase.relayReady;
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
    run.phase = _Phase.controllerStart;
    controller = await Controller.start(
      logDir: logs,
      accountRecords: accountScenario,
      controlledTime: messageScenario,
    );
    run.owned.add('controller', controller.close);
    final control = controller;
    final participants = <Participant>[];
    final appRestarts = <String, AccountRestart>{};
    final appClocks = <String, Future<int> Function()>{};
    final appAirplane = <String, Future<void> Function(bool)>{};
    final appForeground = <String, Future<void> Function()>{};
    for (var i = 1; i <= windowsCount; i++) {
      run.phase = _Phase.windowsStart;
      final name = 'windows-$i';
      var store = 0;
      OwnedProcess? process;
      Future<Participant> launch(
        Participant? old, {
        bool freshStore = false,
      }) async {
        if (freshStore) store++;
        if (process != null) await run.stop(process!);
        final directory = p.join(logs.path, name, 'store-$store');
        await Directory(directory).create(recursive: true);
        process = await run.start(
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
        Participant? next;
        run.phase = _Phase.windowsParticipant;
        await run.until(() async {
          next = await control.waitParticipant(name);
          return old == null || next!.instance != old.instance;
        }, const Duration(seconds: 30));
        return next!;
      }

      participants.add(await launch(null));
      appRestarts[name] = (old, {bool freshStore = false}) =>
          launch(old, freshStore: freshStore);
    }
    if (androidCount > 0) {
      run.phase = _Phase.sdkPreflight;
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
      run.phase = _Phase.avdPreflight;
      for (final spec in specs) {
        // E2E installs/launches only on newly owned emulator processes.
        if (await emulators.isRunning(spec)) {
          throw StateError(
            'Test AVD already running; preserved without installing or launching',
          );
        }
      }
      await emulators.preflight(specs);
      run.phase = _Phase.avdCreate;
      for (final spec in specs) {
        // A missing snapshot: first boot (bootTimeout 3 min), settle (at most
        // 10 min), save and stop.
        await run.wait(
          emulators.ensure(spec),
          timeout: const Duration(minutes: 20),
        );
      }
      run.phase = _Phase.avdBoot;
      await run.wait(emulators.up(specs), timeout: const Duration(minutes: 5));
      androidCrashed = () async {
        var known = true;
        for (final spec in specs) {
          final result = await run.command(
            sdk.adb,
            ['-s', spec.serial, 'logcat', '-b', 'crash', '-d'],
            requireSuccess: false,
            cleanup: true,
          );
          if (result.exitCode != 0) {
            known = false;
          } else if ((result.stdout as String).trim().isNotEmpty) {
            return true;
          }
        }
        return known ? false : null;
      };
      for (var i = 0; i < specs.length; i++) {
        final spec = specs[i];
        run.phase = _Phase.adbReverse;
        for (final port in [3340, 47000]) {
          await run.reverse(sdk, spec, port);
        }
        final name = 'android-${i + 1}';
        final adb = ['-s', spec.serial];
        run.phase = _Phase.androidInstall;
        if (accountScenario) {
          // A restored snapshot may carry older crash and lmkd entries.
          await run.command(sdk.adb, [
            ...adb,
            'logcat',
            '-b',
            'crash',
            '-b',
            'main',
            '-c',
          ]);
        }
        await run.command(
          sdk.adb,
          _androidInstall(
            adb,
            p.join(
              root,
              'app',
              'build',
              'app',
              'outputs',
              'flutter-apk',
              'app-debug.apk',
            ),
          ),
          timeout: const Duration(minutes: 2),
        );
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
        var appPid = '';
        var restarting = false;
        var store = 0;
        // The slowest launch so far: `am start` until the app's control hello.
        int? launchMsMax;
        int? launchSkew;
        // Registered before the first launch, so a first launch that fails is
        // probed too.
        androidProbes.add(
          () => probeAndroid(
            (args) => run.command(
              sdk.adb,
              [...adb, ...args],
              requireSuccess: false,
              cleanup: true,
            ),
            name: name,
            package: package,
            ownedPid: appPid,
            launchSkew: launchSkew,
            launchMsMax: launchMsMax,
            hostLoad: _hostLoad(),
          ),
        );
        Future<Participant> launch(
          Participant? old, {
          bool freshStore = false,
        }) async {
          restarting = true;
          try {
            if (old != null) {
              final current = await run.command(sdk.adb, [
                ...adb,
                'shell',
                'pidof',
                package,
              ]);
              if ((current.stdout as String).trim() != appPid) {
                throw StateError('Owned Android process changed');
              }
              await run.command(sdk.adb, [
                ...adb,
                'shell',
                'am',
                'force-stop',
                package,
              ]);
            }
            if (freshStore) store++;
            run.phase = _Phase.androidLaunch;
            if (accountScenario) {
              // The probe's lifecycle covers the process launched next.
              await run.command(sdk.adb, [
                ...adb,
                'logcat',
                '-b',
                'events',
                '-c',
              ]);
            }
            final launchClock = Stopwatch()..start();
            final launched = await run.command(sdk.adb, [
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
              if (accountScenario) ...[
                '--es',
                'chat_data_dir',
                '/data/user/0/$package/files/e2e/${p.basename(logs.parent.path)}/$name/store-$store',
              ],
            ], requireSuccess: false);
            await _captureAndroidLaunch(
              launched,
              () => run.command(sdk.adb, [
                ...adb,
                'shell',
                'pidof',
                package,
              ], requireSuccess: false),
              activity,
              run.save,
            );
            run.phase = _Phase.androidPid;
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
            Participant? next;
            run.phase = _Phase.androidParticipant;
            // `am start` to the control hello took up to 17.4 s in a passing run
            // on the GitHub emulators. waitParticipant's default 10 s, counted
            // from here, was the tighter limit, not the outer 30 s.
            await run.until(() async {
              next = await control.waitParticipant(
                name,
                timeout: const Duration(seconds: 60),
              );
              return old == null || next!.instance != old.instance;
            }, const Duration(seconds: 60));
            final took = launchClock.elapsedMilliseconds;
            if (launchMsMax == null || took > launchMsMax!) launchMsMax = took;
            return next!;
          } finally {
            restarting = false;
            run.phase = _Phase.scenario;
          }
        }

        final first = await launch(null);
        try {
          final date = await run.command(sdk.adb, [
            ...adb,
            'shell',
            'date',
            '+%s',
          ], requireSuccess: false);
          launchSkew = date.exitCode == 0
              ? clockSkew(
                  date.stdout as String,
                  DateTime.now().millisecondsSinceEpoch ~/ 1000,
                )
              : null;
        } on StateError {
          /* An unread launch clock stays null. */
        }
        appRestarts[name] = (old, {bool freshStore = false}) =>
            launch(old, freshStore: freshStore);
        appAirplane[name] = (on) async {
          await run.command(sdk.adb, [
            ...adb,
            'shell',
            'cmd',
            'connectivity',
            'airplane-mode',
            on ? 'enable' : 'disable',
          ]);
        };
        // singleTop: the running activity comes to the front (onNewIntent).
        appForeground[name] = () async {
          await run.command(sdk.adb, [
            ...adb,
            'shell',
            'am',
            'start',
            '-n',
            '$package/.MainActivity',
          ], requireSuccess: false);
        };
        appClocks[name] = () async {
          final result = await run.command(sdk.adb, [
            ...adb,
            'shell',
            'date',
            '+%s',
          ]);
          final value = (result.stdout as String).trim();
          if (!RegExp(r'^\d{10,}$').hasMatch(value)) {
            throw StateError('Invalid owned clock sample');
          }
          return int.parse(value);
        };
        if (!accountScenario) {
          await run.start(sdk.adb, [
            ...adb,
            'logcat',
            '--pid=$appPid',
            '-v',
            'threadtime',
          ], logDir: logs);
        }
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
              if (restarting) continue;
              final observedPid = appPid;
              final result = await run.command(
                sdk.adb,
                [...adb, 'shell', 'pidof', package],
                requireSuccess: false,
                failurePhase: 'android_pid_monitor',
              );
              if (!restarting &&
                  appPid == observedPid &&
                  (result.stdout as String).trim() != appPid) {
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
        if (!accountScenario) {
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
        }
        participants.add(first);
      }
    }
    OwnedProcess? nodeProcess;
    Future<Participant> launchNode([Participant? old]) async {
      run.phase = _Phase.nodeStart;
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
        if (mailboxScenario) '--account',
      ], logDir: logs);
      Participant? next;
      run.phase = _Phase.nodeParticipant;
      await run.until(() async {
        next = await control.waitParticipant('node');
        return old == null || next!.instance != old.instance;
      }, const Duration(seconds: 15));
      if (old != null) run.phase = _Phase.scenario;
      return next!;
    }

    if (!accountScenario || mailboxScenario) {
      participants.add(await launchNode());
    }
    run.phase = _Phase.scenario;
    Participant named(String name) =>
        participants.singleWhere((participant) => participant.name == name);
    await run.wait(
      mailboxScenario
          ? runMailboxScenario(
              control,
              [
                named('android-1'),
                named(profile == 'mixed' ? 'windows-1' : 'android-2'),
                named('node'),
              ],
              airplane: (participant, on) {
                final action = appAirplane[participant.name];
                if (action == null) {
                  throw StateError('Airplane participant is not owned');
                }
                return action(on);
              },
              foreground: (participant) {
                final action = appForeground[participant.name];
                if (action == null) {
                  throw StateError('Foreground participant is not owned');
                }
                return action();
              },
              evidence: (stage, data) =>
                  File(
                    p.join(logs.path, 'mailbox-public-evidence.jsonl'),
                  ).writeAsStringSync(
                    '${jsonEncode({'stage': stage, ...data})}\n',
                    mode: FileMode.append,
                    flush: true,
                  ),
            )
          : messageScenario
          ? runMessageScenario(
              control,
              participants,
              auditRecovery: (patterns) => _auditRecoveryLogs(logs, patterns),
              restart: (old, {bool freshStore = false}) {
                final action = appRestarts[old.name];
                if (action == null) {
                  throw StateError('Restart participant is not owned');
                }
                return action(old, freshStore: freshStore);
              },
              evidence: (stage, data) =>
                  File(
                    p.join(logs.path, 'message-public-evidence.jsonl'),
                  ).writeAsStringSync(
                    '${jsonEncode({'stage': stage, ...data})}\n',
                    mode: FileMode.append,
                    flush: true,
                  ),
            )
          : accountScenario
          ? runAccountScenario(
              control,
              participants,
              authOnly: authOnly,
              wallClock: (participant) async =>
                  appClocks[participant.name] == null
                  ? DateTime.now().millisecondsSinceEpoch ~/ 1000
                  : await appClocks[participant.name]!(),
              restart: (old, {bool freshStore = false}) {
                final action = appRestarts[old.name];
                if (action == null) {
                  throw StateError('Restart participant is not owned');
                }
                return action(old, freshStore: freshStore);
              },
              evidence: (stage, data) =>
                  File(
                    p.join(logs.path, 'account-public-evidence.jsonl'),
                  ).writeAsStringSync(
                    '${jsonEncode({'stage': stage, ...data})}\n',
                    mode: FileMode.append,
                    flush: true,
                  ),
            )
          : runScenario(
              control,
              participants,
              restart: launchNode,
              onFailure: (error, phase, kind) => run.recordFailure(
                error,
                failurePhase: phase,
                timeoutKind: kind,
              ),
            ),
      timeout: Duration(minutes: accountScenario ? 8 : 4),
      failurePhase: 'scenario',
    );
    if (accountScenario) run.android = await _probeAll(androidProbes);
  } catch (error, stack) {
    run.recordFailure(error);
    code = userInterrupted ? 130 : 1;
    run.token.cancel();
    // Before the controller closes: the apps still run as the failure left them.
    if (accountScenario && run.android == null) {
      run.android = await _probeAll(androidProbes);
    }
    await run.interrupt(controller);
    if (accountScenario && androidCrashed != null) {
      try {
        run.androidCrash = await androidCrashed();
      } catch (_) {
        /* Unknown stays null in the retained summary. */
      }
    }
    File(
      p.join(logs.path, 'failure.log'),
    ).writeAsStringSync('${jsonEncode(run.safeFailure)}\n$stack', flush: true);
    stderr.writeln(
      '$profile scenario failed; details retained in ignored build/e2e logs.',
    );
  } finally {
    finished = true;
    run.phase = _Phase.cleanup;
    await signal?.cancel();
    try {
      await run.stopMonitors();
    } catch (error) {
      run.recordFailure(error);
      run.cleanupFailures.add('monitor drain: $error');
    }
    await interrupted;
    await run.unwind();
    run.finishing = true;
    final failures = [...run.cleanupFailures, ...await run.owned.close()];
    if (failures.isNotEmpty) {
      run.recordFailure(null);
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
      ).writeAsStringSync(jsonEncode(run.safeFailure), flush: true);
      if (code == 0) code = 1;
    }
    if (userInterrupted && code == 0) code = 130;
    if (code == 0) {
      stdout.writeln(
        mailboxScenario
            ? '$profile M6 PASS: phone in airplane mode, mailbox kept three texts, phone got them in order once with one notification.'
            : messageScenario
            ? '$profile M4 PASS: native retained message restart, durable ack and read.'
            : accountScenario
            ? authOnly
                  ? '$profile M3 auth PASS: fresh/cache proof, failed proof, disconnect and restart.'
                  : '$profile M3 PASS: local signed records, native stores, pairing, restart, revoke and recovery.'
            : '$profile scenario PASS: six directions, offline recovery, no replay, Disconnect and fresh restart address.',
      );
    }
    stdout.writeln(
      'Run status: $code. Private diagnostics: build/e2e/<run>/$profile.',
    );
  }
  // A failed verified stop may leave native pipe subscriptions alive. All
  // admitted work and cleanup have finished; retained ownership stays explicit.
  try {
    await stdout.flush();
    await stderr.flush();
  } catch (error) {
    run.recordFailure(error);
    if (code == 0) code = 1;
    File(
      p.join(logs.path, 'terminal-flush.log'),
    ).writeAsStringSync('$error', flush: true);
  } finally {
    final diagnostic = Platform.environment['E2E_DIAGNOSTIC_FILE'];
    if (diagnostic != null) {
      try {
        File(diagnostic).writeAsStringSync(
          jsonEncode({
            ...run.safeFailure ??
                {
                  'phase': _Phase.complete.value,
                  'failure_kind': null,
                  'command_exit_code': null,
                  'detail': null,
                },
            'scenario': scenarioName,
            'android_crash': run.androidCrash,
            'last_view': run.lastView,
            'participant': run.lastParticipant,
            'rpc_op': run.rpcOp,
            'facts': run.facts(),
            'android': run.android,
          }),
          flush: true,
        );
      } on FileSystemException {
        /* Missing diagnostics remain null in the retained summary. */
      }
    }
    exit(code);
  }
}

/// Only the two-emulator GitHub job may stop a daemon, in its own registry.
bool usesOwnedCiGradle(String profile, Map<String, String> environment) {
  if (profile != 'android' || environment['GITHUB_ACTIONS'] != 'true') {
    return false;
  }
  final temp = environment['RUNNER_TEMP'];
  final id = environment['GITHUB_RUN_ID'];
  final attempt = environment['GITHUB_RUN_ATTEMPT'];
  final job = environment['GITHUB_JOB'];
  if (temp == null ||
      temp.isEmpty ||
      id == null ||
      !RegExp(r'^\d+$').hasMatch(id) ||
      attempt == null ||
      !RegExp(r'^\d+$').hasMatch(attempt) ||
      job == null ||
      !RegExp(r'^[a-zA-Z0-9_-]+$').hasMatch(job)) {
    throw StateError('Missing GitHub job-owned Gradle identity');
  }
  final expected = p.join(temp, 'chatproject-$id-$attempt-$job');
  if (environment['GRADLE_USER_HOME'] != expected) {
    throw StateError(
      'Android CI requires the exact job-owned GRADLE_USER_HOME',
    );
  }
  return true;
}

enum _Phase {
  preflight('preflight'),
  buildNode('build_node'),
  buildWindows('build_windows'),
  buildAndroid('build_android'),
  gradleStop('gradle_stop'),
  relayReady('relay_ready'),
  controllerStart('controller_start'),
  windowsStart('windows_start'),
  windowsParticipant('windows_participant'),
  sdkPreflight('sdk_preflight'),
  avdPreflight('avd_preflight'),
  avdCreate('avd_create'),
  avdBoot('avd_boot'),
  adbReverse('adb_reverse'),
  androidInstall('android_install'),
  androidLaunch('android_launch'),
  androidPid('android_pid'),
  androidParticipant('android_participant'),
  nodeStart('node_start'),
  nodeParticipant('node_participant'),
  scenario('scenario'),
  cleanup('cleanup'),
  complete('complete');

  const _Phase(this.value);
  final String value;
}

class _CommandFailure extends StateError {
  _CommandFailure(this.code, String executable)
    : super('Command failed ($code): $executable');
  final int code;
}

class _OwnedChildFailure extends StateError {
  _OwnedChildFailure(this.code, String executable, List<String> args)
    : kind = switch (p.basename(executable)) {
        'chat_node' || 'chat_node.exe' =>
          args.isNotEmpty && args.first == 'relay'
              ? 'owned_relay_exit'
              : 'owned_chat_node_exit',
        'emulator' || 'emulator.exe' => 'owned_emulator_exit',
        'chat_app' || 'chat_app.exe' => 'owned_windows_exit',
        _ => 'owned_child_exit',
      },
      super('Owned child exited ($code)');
  final int code;
  final String kind;
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
  final bodies = <Future<dynamic>>{};
  final acquisitions = <Future<OwnedProcess>>{};
  final monitorStops = <Completer<void>>[];
  final monitorTasks = <Future<void>>[];
  Future<void>? _monitorClosing;
  Object? firstFailure;
  Future<void>? _interrupting;
  bool finishing = false;
  _Phase phase = _Phase.preflight;
  Map<String, Object?>? safeFailure;
  bool? androidCrash;
  String? lastView;
  String? lastParticipant;
  String? rpcOp;
  List<Map<String, Object?>>? android;
  final safeDetails = loadSafeDetails();
  final safeViews = loadSafeValues('last_view');
  final safeParticipants = loadSafeValues('participants');
  final safeFactValues = loadSafeFacts();
  Map<String, String>? facts() => safeFacts(failureFacts, safeFactValues);

  void recordFailure(
    Object? error, {
    String? failurePhase,
    String? timeoutKind,
  }) {
    if (safeFailure == null && error is ScenarioDeadline) {
      lastView = safeViews.contains(error.lastView) ? error.lastView : null;
      lastParticipant = safeParticipants.contains(error.participant)
          ? error.participant
          : null;
    }
    if (safeFailure == null && error is RpcError) {
      final op = error.op;
      rpcOp = op != null && _rpcOp.hasMatch(op) ? op : null;
      lastParticipant = safeParticipants.contains(error.participant)
          ? error.participant
          : null;
    }
    safeFailure ??= {
      'phase': failurePhase ?? phase.value,
      'failure_kind': switch (error) {
        null => 'cleanup',
        _CommandFailure() => 'command_exit',
        EmulatorFailure() => error.kind.value,
        _OwnedChildFailure() => error.kind,
        TimeoutException() => timeoutKind ?? 'timeout',
        RunCancelled() => 'cancelled',
        SocketException() => 'socket',
        FileSystemException() => 'filesystem',
        ProcessException() => 'process_spawn',
        StateError() => 'state',
        ArgumentError() => 'argument',
        UnsupportedError() => 'unsupported',
        FormatException() => 'format',
        _ => 'other',
      },
      'command_exit_code': switch (error) {
        _CommandFailure() => error.code,
        EmulatorFailure() => error.code,
        _OwnedChildFailure() => error.code,
        _ => null,
      },
      'detail': safeDetail(error, safeDetails),
    };
  }

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
    recordFailure(error);
    firstFailure ??= error;
    if (!fatal.isCompleted) fatal.completeError(error, stack);
  }

  Future<T> wait<T>(
    Future<T> work, {
    Duration timeout = const Duration(seconds: 30),
    bool cleanup = false,
    bool nativeLifetime = false,
    String? failurePhase,
  }) async {
    if (cleanup) return work.timeout(timeout);
    // A native lifetime may never finish after failed stop/output draining.
    // Only body continuations can acquire resources and must be drained.
    if (!nativeLifetime) {
      bodies.add(work);
      unawaited(
        work.then<void>(
          (_) {
            bodies.remove(work);
          },
          onError: (Object _, StackTrace _) {
            bodies.remove(work);
          },
        ),
      );
    }
    try {
      token.check();
      return await Future.any<T>([
        work,
        fatal.future,
        token.cancelled.then<T>((_) => throw RunCancelled()),
      ]).timeout(timeout);
    } catch (error) {
      recordFailure(error, failurePhase: failurePhase);
      token.cancel();
      rethrow;
    }
  }

  Future<void> unwind() async {
    while (bodies.isNotEmpty || acquisitions.isNotEmpty) {
      await Future.wait(
        [...bodies, ...acquisitions].map(
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
    bool retainOutput = false,
    Map<String, String>? environment,
  }) {
    if (!cleanup) {
      token.check();
      if (finishing) throw RunCancelled();
    }
    final acquisition = _acquire(
      executable,
      args,
      cwd: cwd,
      stdin: stdin,
      persistent: persistent,
      cleanup: cleanup,
      retainOutput: retainOutput,
      environment: environment,
    );
    acquisitions.add(acquisition);
    unawaited(
      acquisition.then<void>(
        (_) {
          acquisitions.remove(acquisition);
        },
        onError: (Object _, StackTrace _) {
          acquisitions.remove(acquisition);
        },
      ),
    );
    return acquisition;
  }

  Future<OwnedProcess> _acquire(
    String executable,
    List<String> args, {
    String? cwd,
    String? stdin,
    required bool persistent,
    required bool cleanup,
    bool retainOutput = false,
    Map<String, String>? environment,
  }) async {
    File(p.join(logs.path, 'commands.jsonl')).writeAsStringSync(
      '${jsonEncode({'at': DateTime.now().toUtc().toIso8601String(), 'executable': executable, 'args': args, 'cwd': cwd})}\n',
      mode: FileMode.append,
      flush: true,
    );
    final OwnedProcess process;
    try {
      process = await super.start(
        executable,
        args,
        logDir: logs,
        workingDirectory: cwd,
        stdin: stdin,
        retainOutput: retainOutput,
        environment: environment,
      );
    } on OwnedProcessStartFailure catch (failure) {
      retainStartFailure(failure, executable, cwd, cleanup);
    }
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
                _OwnedChildFailure(code, executable, args),
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

  Never retainStartFailure(
    OwnedProcessStartFailure failure,
    String executable,
    String? cwd,
    bool cleanup,
  ) {
    final process = failure.process;
    active.add(process);
    processes.add({'pid': process.pid, 'path': executable, 'cwd': cwd});
    cleanupFailures.add('startup stop: ${failure.cleanupFailure}');
    if (failure.diagnosticFailure != null) {
      cleanupFailures.add('startup diagnostic: ${failure.diagnosticFailure}');
    }
    try {
      if (!cleanup) owned.add('failed startup process', () => stop(process));
      save();
    } catch (error) {
      cleanupFailures.add('startup ownership: $error');
    }
    Error.throwWithStackTrace(failure.primary, failure.primaryStack);
  }

  @override
  Future<OwnedProcess> start(
    String executable,
    List<String> arguments, {
    required Directory logDir,
    String? workingDirectory,
    String? stdin,
    bool retainOutput = false,
    Map<String, String>? environment,
  }) async => _Managed(
    await acquire(
      executable,
      arguments,
      cwd: workingDirectory,
      stdin: stdin,
      persistent: true,
      retainOutput: retainOutput,
      environment: environment,
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
      // These exact acquired emulators own the activity/reverse dependencies.
      // Their registered cleanup runs after those callbacks in owned.close().
      if (processes.any(
        (entry) => entry['pid'] == process.pid && entry.containsKey('avd'),
      )) {
        continue;
      }

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
    String? failurePhase,
    Map<String, String>? environment,
  }) async {
    final process = await acquire(
      executable,
      args,
      cwd: cwd,
      stdin: stdin,
      cleanup: cleanup,
      environment: environment,
    );
    Object? primary;
    try {
      final code = await wait(
        process.exitCode,
        timeout: timeout,
        cleanup: cleanup,
        nativeLifetime: true,
        failurePhase: failurePhase,
      );
      final result = ProcessResult(
        process.pid,
        code,
        File(p.join(logs.path, '${process.pid}.stdout.log')).readAsStringSync(),
        File(p.join(logs.path, '${process.pid}.stderr.log')).readAsStringSync(),
      );
      if (requireSuccess && code != 0) {
        throw _CommandFailure(code, executable);
      }
      return result;
    } catch (error) {
      recordFailure(error, failurePhase: failurePhase);
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

/// A fixed scenario op name (tools/ci/write_e2e_summary.py `RPC_OP`).
final _rpcOp = RegExp(r'^(?:account|message|mailbox)_local_[a-z_]{1,48}$');

/// The host's 1-minute load average times 100 (Linux), or null.
int? _hostLoad() {
  try {
    return loadX100(File('/proc/loadavg').readAsStringSync());
  } on FileSystemException {
    return null;
  }
}

/// Probes every owned emulator; one that cannot be read is left out. A run
/// without emulators (Windows) has no Android state.
Future<List<Map<String, Object?>>?> _probeAll(
  List<Future<Map<String, Object?>> Function()> probes,
) async {
  if (probes.isEmpty) return null;
  final states = <Map<String, Object?>>[];
  for (final probe in probes) {
    try {
      states.add(await probe());
    } catch (_) {
      /* The summary keeps the emulators that could be read. */
    }
  }
  return states;
}

/// `-g` grants the runtime permissions: the notification prompt an active
/// account opens (M6) would otherwise stay over the task after a force-stop,
/// and the next launch would only bring that task to the front.
List<String> _androidInstall(List<String> adb, String apk) => [
  ...adb,
  'install',
  '-r',
  '-g',
  apk,
];

Future<void> _captureAndroidLaunch(
  ProcessResult launch,
  Future<ProcessResult> Function() samplePid,
  Map<String, Object?> activity,
  void Function() save,
) async {
  final current = await samplePid();
  final pid = (current.stdout as String).trim();
  if (current.exitCode == 0 && RegExp(r'^\d+$').hasMatch(pid)) {
    activity['pid'] = pid;
    save();
  }
  if (launch.exitCode != 0) throw _CommandFailure(launch.exitCode, 'adb');
  // `am start` exits 0 when it only brought an existing task to the front or
  // handed the intent to a running activity; no new app process runs then.
  if ((launch.stdout as String).contains('Activity not started')) {
    throw StateError('Android launch did not start the activity');
  }
}
