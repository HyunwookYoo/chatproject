// Invoke from the repository root after `cd tools/e2e && dart pub get`:
// dart --packages=tools/e2e/.dart_tool/package_config.json tools/ci/release_smoke.dart ...
import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:args/args.dart';
import 'package:chat_e2e/src/android_sdk.dart';
import 'package:chat_e2e/src/owned_resources.dart';
import 'package:chat_e2e/src/process_runner.dart';
import 'package:path/path.dart' as p;

const _package = 'dev.chatproject.chat_app';
const _activity = '$_package/.MainActivity';

Future<void> main(List<String> arguments) async {
  final parser = ArgParser()
    ..addOption('platform', mandatory: true, allowed: ['windows', 'android'])
    ..addOption('artifact', mandatory: true)
    ..addOption('serial')
    ..addOption('contract-nonce');
  var code = 0;
  final cancel = RunCancellation();
  StreamSubscription<ProcessSignal>? signal;
  try {
    final options = parser.parse(arguments);
    if (options.rest.isNotEmpty) throw FormatException(parser.usage);
    if (options['platform'] == 'windows' && !Platform.isWindows) {
      throw const FormatException('Windows smoke requires a Windows host');
    }
    try {
      signal = ProcessSignal.sigint.watch().listen((_) => cancel.cancel());
    } on UnsupportedError {
      stderr.writeln(
        'Handled interrupt unavailable; ownership is retained on hard termination.',
      );
    }
    final logs = Directory(
      p.join(
        'build',
        'e2e',
        '${DateTime.now().microsecondsSinceEpoch}',
        'release',
      ),
    );
    await runSmoke(
      platform: options['platform'] as String,
      artifact: p.absolute(options['artifact'] as String),
      serial: options['serial'] as String?,
      contractNonce: options['contract-nonce'] as String?,
      logs: logs,
      cancellation: cancel,
    );
    stdout.writeln(
      options['contract-nonce'] == null
          ? 'Release Home/native smoke PASS; network DNS and traffic remain physical-device validation'
          : 'Release runtime contract PASS; binary marker scan is a separate check',
    );
  } catch (error) {
    code = error is RunCancelled ? 130 : 1;
    stderr.writeln('Release smoke FAIL: $error');
  } finally {
    await signal?.cancel();
    try {
      await stdout.flush();
      await stderr.flush();
    } catch (_) {
      if (code == 0) code = 1;
    }
    // All admitted work and cleanup have finished, including failed native stops.
    exit(code);
  }
}

Future<void> runSmoke({
  required String platform,
  required String artifact,
  required Directory logs,
  String? serial,
  String? contractNonce,
  ProcessRunner runner = const IoProcessRunner(),
  RunCancellation? cancellation,
  Duration readyTimeout = const Duration(seconds: 45),
  Duration settleTime = const Duration(seconds: 3),
  Duration pollInterval = const Duration(milliseconds: 300),
}) async {
  final run = _Smoke(runner, logs, cancellation ?? RunCancellation());
  await logs.create(recursive: true);
  Object? primary;
  try {
    if (contractNonce != null &&
        !RegExp(r'^[a-zA-Z0-9_-]+$').hasMatch(contractNonce)) {
      throw const FormatException('Invalid contract nonce');
    }
    late final Future<({bool current, String text, String output})> Function()
    inspect;
    Object? exited;
    if (platform == 'windows') {
      final executable = p.join(artifact, 'chat_app.exe');
      if (!File(executable).existsSync())
        throw StateError('Missing full Windows Release artifact');
      final child = await run.start(executable, [
        '--data-dir',
        p.absolute(p.join(logs.path, 'core')),
      ], cwd: artifact);
      unawaited(
        child.exitCode.then<void>(
          (code) => exited = 'Owned app exited ($code)',
          onError: (Object error, StackTrace _) => exited = error,
        ),
      );
      inspect = () async {
        final ui = await run.command('powershell.exe', [
          '-NoProfile',
          '-NonInteractive',
          '-Command',
          _windowsUi(child.pid),
        ]);
        final data = jsonDecode(ui.stdout as String) as Map<String, dynamic>;
        return (
          current: data['pid'] == child.pid && data['window'] != 0,
          text: (data['texts'] as List).join('\n'),
          output: run.output(child.pid),
        );
      };
    } else if (platform == 'android') {
      if (serial == null || !RegExp(r'^emulator-\d+$').hasMatch(serial)) {
        throw const FormatException(
          'Android automated Release smoke requires an explicit emulator serial',
        );
      }
      if (!File(artifact).existsSync()) throw StateError('Missing Release APK');
      final adb = AndroidSdk.locate().adb;
      Future<ProcessResult> command(
        List<String> args, {
        bool cleanup = false,
        bool success = true,
      }) => run.command(
        adb,
        ['-s', serial, ...args],
        cleanup: cleanup,
        success: success,
      );
      Future<String> appPid({bool cleanup = false}) async =>
          ((await command(
                    ['shell', 'pidof', _package],
                    cleanup: cleanup,
                    success: false,
                  )).stdout
                  as String)
              .trim();
      if ((await appPid()).isNotEmpty)
        throw StateError(
          'Existing app process preserved; use an idle test emulator',
        );
      await command(['install', '-r', artifact]);
      final packageInfo =
          (await command(['shell', 'dumpsys', 'package', _package])).stdout
              as String;
      if (packageInfo.contains('DEBUGGABLE') ||
          !packageInfo.contains('android.permission.INTERNET') ||
          !packageInfo.contains('android.permission.ACCESS_NETWORK_STATE')) {
        throw StateError(
          'Release manifest permission/debuggable contract failed',
        );
      }
      final since =
          ((await command(['shell', 'date', '+%s.%N'])).stdout as String)
              .trim();
      if (!RegExp(r'^\d+\.\d+$').hasMatch(since))
        throw StateError('Device log window unavailable');
      run.cancel.check();
      String? ownedPid;
      run.activities.add({'package': _package, 'serial': serial, 'pid': null});
      run.owned.add('Android activity', () async {
        final current = await appPid(cleanup: true);
        if (current.isNotEmpty && current != ownedPid) {
          throw StateError(
            'Android activity identity changed; preserved for manual cleanup',
          );
        }
        if (current.isNotEmpty)
          await command(['shell', 'am', 'force-stop', _package], cleanup: true);
        run.activities.clear();
        run.save();
      });
      run.save();
      // Launch and PID registration are one admitted acquisition. Reuse the
      // bounded, self-reclaiming cleanup commands to drain it despite Ctrl+C;
      // checking cancellation in between would strand the launched app.
      await command(['shell', 'am', 'start', '-W', '-n', _activity], cleanup: true);
      ownedPid = await appPid(cleanup: true);
      if (!RegExp(r'^\d+$').hasMatch(ownedPid))
        throw StateError('Owned Android app failed to start');
      run.activities.single['pid'] = ownedPid;
      run.save();
      run.cancel.check();
      inspect = () async {
        if (await appPid() != ownedPid)
          throw StateError('Owned Android app exited or changed');
        final activity =
            (await command([
                  'shell',
                  'dumpsys',
                  'activity',
                  'activities',
                ])).stdout
                as String;
        final current = RegExp(
          r'(?:mResumedActivity|topResumedActivity)[:=][^\n]*dev\.chatproject\.chat_app/\.MainActivity',
        ).hasMatch(activity);
        var text = '';
        if (current && contractNonce == null) {
          // uiautomator prints only the current window; retain no global screenshot.
          text =
              (await command([
                    'exec-out',
                    'uiautomator',
                    'dump',
                    '/dev/tty',
                  ])).stdout
                  as String;
          if (!text.contains('package="$_package"')) text = '';
        }
        final output =
            (await command([
                  'logcat',
                  '-d',
                  '--pid=$ownedPid',
                  '-T',
                  since,
                  '-v',
                  'brief',
                ])).stdout
                as String;
        File(
          p.join(logs.path, 'owned-app.log'),
        ).writeAsStringSync(output, flush: true);
        return (current: current, text: text, output: output);
      };
    } else {
      throw const FormatException('Unsupported platform');
    }
    final timer = Stopwatch()..start();
    Duration? readyAt;
    while (true) {
      run.cancel.check();
      if (exited != null) throw StateError('$exited');
      final state = await inspect();
      if (exited != null) throw StateError('$exited');
      if (RegExp(
        r'Unhandled Exception|FATAL EXCEPTION|Fatal signal|ChatProject startup failed|Home event stream (?:failed|ended)|UnsatisfiedLinkError',
        caseSensitive: false,
      ).hasMatch(state.output)) {
        throw StateError('Owned app reported a runtime exception');
      }
      final ready =
          state.current &&
          (contractNonce == null
              ? state.text.contains('대화') &&
                    state.text.contains('아직 대화가 없어요') &&
                    state.text.contains('연결 없음') &&
                    RegExp(r'코어 \d+\.\d+\.\d+').hasMatch(state.text)
              : _contractProof(state.output, contractNonce));
      if (ready) {
        readyAt ??= timer.elapsed;
        if (timer.elapsed - readyAt >= settleTime) break;
      } else {
        readyAt = null;
      }
      if (timer.elapsed > readyTimeout + settleTime)
        throw StateError('Release readiness deadline exceeded');
      await Future<void>.delayed(pollInterval);
    }
  } catch (error) {
    primary = error;
    rethrow;
  } finally {
    // No background acquisition/body is raced with cancellation. Each awaited
    // start registers its exact handle before a cancellation check can unwind.
    final failures = await run.owned.close();
    if (failures.isNotEmpty) {
      try {
        File(
          p.join(logs.path, 'cleanup.json'),
        ).writeAsStringSync(jsonEncode(failures), flush: true);
      } catch (_) {
        if (primary == null) rethrow;
      }
      if (primary == null)
        throw StateError('Release cleanup failed; see ownership record');
    }
  }
}

bool _contractProof(String output, String nonce) {
  final records = <Map<String, dynamic>>[];
  for (final line in const LineSplitter().convert(output)) {
    final start = line.indexOf('{"type":"release_contract"');
    if (start < 0) continue;
    try {
      records.add(jsonDecode(line.substring(start)) as Map<String, dynamic>);
    } on FormatException {
      throw StateError('Malformed Release contract proof');
    }
  }
  if (records.isEmpty) return false;
  if (records.length != 1) throw StateError('Duplicate Release contract proof');
  final record = records.single;
  if (record.length != 7 ||
      record['version'] != 1 ||
      record['mode'] != 'release' ||
      record['nonce'] != nonce ||
      record['hooks'] != 'notAvailable' ||
      record['network'] != 'stopped' ||
      !record.containsKey('endpoint') ||
      record['endpoint'] != null) {
    throw StateError('Invalid Release contract proof');
  }
  return true;
}

class _Smoke {
  _Smoke(this.runner, this.logs, this.cancel);
  final ProcessRunner runner;
  final Directory logs;
  final RunCancellation cancel;
  final owned = OwnedResources();
  final processes = <int>[];
  final activities = <Map<String, Object?>>[];

  void save() => File(p.join(logs.path, 'ownership.json')).writeAsStringSync(
    jsonEncode({
      'ownerPid': pid,
      'processes': processes,
      'activities': activities,
    }),
    flush: true,
  );

  String output(int pid) => ['stdout', 'stderr']
      .map((kind) {
        final file = File(p.join(logs.path, '$pid.$kind.log'));
        return file.existsSync() ? file.readAsStringSync() : '';
      })
      .join('\n');

  Future<OwnedProcess> start(
    String exe,
    List<String> args, {
    String? cwd,
    bool cleanup = false,
  }) async {
    if (!cleanup) cancel.check();
    late final OwnedProcess child;
    try {
      child = await runner.start(
        exe,
        args,
        logDir: logs,
        workingDirectory: cwd,
      );
    } on OwnedProcessStartFailure catch (failure) {
      child = failure.process;
      processes.add(child.pid);
      if (!cleanup) owned.add('failed startup child', () => stop(child));
      if (cleanup) {
        try {
          await stop(child);
        } catch (_) {
          /* Retain ownership and primary. */
        }
      }
      try {
        save();
      } catch (_) {
        /* Preserve the acquisition failure. */
      }
      Error.throwWithStackTrace(failure.primary, failure.primaryStack);
    }
    processes.add(child.pid);
    if (!cleanup) owned.add('owned child', () => stop(child));
    try {
      save();
    } catch (error, stack) {
      if (cleanup) {
        try {
          await stop(child);
        } catch (_) {
          /* Preserve the registration failure. */
        }
      }
      Error.throwWithStackTrace(error, stack);
    }
    // Cleanup commands reclaim themselves. Ordinary commands check cancellation
    // after returning the handle so their finally block always owns reclamation.
    return child;
  }

  Future<void> stop(OwnedProcess child) async {
    if (!processes.contains(child.pid)) return;
    await child.stop();
    processes.remove(child.pid);
    save();
  }

  Future<ProcessResult> command(
    String exe,
    List<String> args, {
    bool cleanup = false,
    bool success = true,
  }) async {
    final child = await start(exe, args, cleanup: cleanup);
    Object? primary;
    try {
      if (!cleanup) cancel.check();
      final code = await child.exitCode.timeout(const Duration(seconds: 60));
      final result = ProcessResult(
        child.pid,
        code,
        File(p.join(logs.path, '${child.pid}.stdout.log')).readAsStringSync(),
        File(p.join(logs.path, '${child.pid}.stderr.log')).readAsStringSync(),
      );
      if (success && code != 0)
        throw StateError('Owned command failed ($code)');
      return result;
    } catch (error) {
      primary = error;
      rethrow;
    } finally {
      try {
        await stop(child);
      } catch (error) {
        if (primary == null) rethrow;
        try {
          File(
            p.join(logs.path, 'command-cleanup.log'),
          ).writeAsStringSync('$error', mode: FileMode.append, flush: true);
        } catch (_) {
          /* Preserve the command's primary failure. */
        }
      }
    }
  }
}

String _windowsUi(int ownedPid) =>
    '''
\$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [Text.UTF8Encoding]::new(\$false)
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName Accessibility
Add-Type -ReferencedAssemblies ([Accessibility.IAccessible].Assembly.Location) -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class SmokeAccessibility {
  [DllImport("user32.dll", CharSet = CharSet.Unicode)]
  public static extern IntPtr FindWindowEx(IntPtr parent, IntPtr after, string name, string title);
  [DllImport("oleacc.dll")]
  public static extern int AccessibleObjectFromWindow(IntPtr window, uint objectId, ref Guid iid, [MarshalAs(UnmanagedType.Interface)] out Accessibility.IAccessible accessible);
  [DllImport("oleacc.dll")]
  public static extern int AccessibleChildren(Accessibility.IAccessible container, int start, int count, [Out, MarshalAs(UnmanagedType.LPArray, SizeParamIndex = 2)] object[] children, out int obtained);
  public static string[] Names(IntPtr window) {
    var iid = new Guid("618736E0-3C3D-11CF-810C-00AA00389B71");
    Accessibility.IAccessible root;
    int result = AccessibleObjectFromWindow(window, 0xfffffffc, ref iid, out root);
    if (result != 0 || root == null) return new string[0];
    System.Threading.Thread.Sleep(200);
    var names = new List<string>();
    Read(root, names, 0);
    return names.ToArray();
  }
  private static void Read(Accessibility.IAccessible node, List<string> names, int depth) {
    if (depth > 20) throw new InvalidOperationException("Accessibility tree too deep");
    names.Add(node.get_accName(0));
    int count = node.accChildCount;
    var children = new object[count];
    int obtained;
    AccessibleChildren(node, 0, count, children, out obtained);
    for (int i = 0; i < obtained; i++) {
      var child = children[i] as Accessibility.IAccessible;
      if (child != null) Read(child, names, depth + 1);
      else if (children[i] is int) names.Add(node.get_accName(children[i]));
    }
  }
}
'@
\$child = [Diagnostics.Process]::GetProcessById($ownedPid)
try {
  \$handle = \$child.MainWindowHandle
  \$texts = @()
  if (\$handle -ne 0) {
    # Flutter enables semantics on an MSAA client request (WM_GETOBJECT).
    # Ask only the owned window's Flutter child, without changing system settings.
    \$view = [SmokeAccessibility]::FindWindowEx(\$handle, [IntPtr]::Zero, \$null, \$null)
    if (\$view -eq [IntPtr]::Zero) { \$view = \$handle }
    if (\$view -ne [IntPtr]::Zero) {
      \$viewElement = [Windows.Automation.AutomationElement]::FromHandle(\$view)
      if (\$viewElement.Current.ProcessId -ne $ownedPid) { throw 'Unexpected Flutter child window' }
      \$texts += [SmokeAccessibility]::Names(\$view)
    }
    \$window = [Windows.Automation.AutomationElement]::FromHandle(\$handle)
    if (\$window.Current.ProcessId -ne $ownedPid) { throw 'Foreign app window' }
    \$nodes = \$window.FindAll([Windows.Automation.TreeScope]::Descendants, [Windows.Automation.Condition]::TrueCondition)
    foreach (\$node in \$nodes) { \$texts += \$node.Current.Name }
  }
  @{ pid = $ownedPid; window = \$handle.ToInt64(); texts = \$texts } | ConvertTo-Json -Compress
} finally { \$child.Dispose() }
''';
