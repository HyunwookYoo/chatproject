import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:path/path.dart' as p;

abstract interface class OwnedProcess {
  int get pid;
  Future<int> get exitCode;
  Future<void> stop();
}

class _IoOwnedProcess implements OwnedProcess {
  _IoOwnedProcess(this._process, Directory logDir, {bool fileBacked = false}) {
    final out = fileBacked ? _process.stdout.drain<void>() : _process.stdout.pipe(File(p.join(logDir.path, '$pid.stdout.log')).openWrite());
    final err = fileBacked ? _process.stderr.drain<void>() : _process.stderr.pipe(File(p.join(logDir.path, '$pid.stderr.log')).openWrite());
    _processExit = _process.exitCode.then((code) {
      _exited = true;
      return code;
    });
    // Observe both streams now, even when the native child keeps running.
    exitCode = Future.wait<dynamic>([_processExit, out, err], eagerError: true)
        .then((results) => results.first as int);
    // Callers may wait for readiness before observing exitCode. Keep its error
    // available to them without allowing an early failure to escape the zone.
    unawaited(exitCode.then<void>((_) {}, onError: (Object error, StackTrace stack) {
      _lifecycleFailure = (error, stack);
    }));
  }

  final Process _process;
  bool _exited = false;
  late final Future<int> _processExit;
  (Object, StackTrace)? _lifecycleFailure;
  Map<String, dynamic>? _windowsIdentity;
  Future<void>? _stopping;
  @override
  int get pid => _process.pid;
  @override
  late final Future<int> exitCode;
  @override
  Future<void> stop() => _stopping ??= _stop();

  Future<void> _stop() async {
    try {
      await _terminate();
    } catch (_) {
      final failure = _lifecycleFailure;
      if (failure != null) Error.throwWithStackTrace(failure.$1, failure.$2);
      rethrow;
    }
    // A descendant may retain inherited pipes after the owned native process
    // exits. Bound draining as well so later resource cleanup still runs.
    await exitCode.timeout(const Duration(seconds: 5));
  }

  Future<void> _terminate() async {
    if (Platform.isWindows && !_exited) {
      final identity = _windowsIdentity;
      if (identity == null) throw StateError('Owned process identity unavailable; manual cleanup required');
      final encoded = base64Encode(utf8.encode(jsonEncode(identity)));
      final result = await Process.run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', '''
\$ErrorActionPreference = 'Stop'
\$child = \$null
try {
  \$expected = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$encoded')) | ConvertFrom-Json
  \$child = [Diagnostics.Process]::GetProcessById($pid)
  \$boundHandle = \$child.Handle
  if (\$child.HasExited -or \$child.MainModule.FileName -ne \$expected.path -or \$child.StartTime.ToUniversalTime().Ticks.ToString() -ne \$expected.created) { exit 4 }
  & "\$env:SystemRoot\\System32\\taskkill.exe" /PID $pid /T /F | Out-Null
  exit \$LASTEXITCODE
} catch { exit 4 } finally { if (\$null -ne \$child) { \$child.Dispose() } }
''']);
      if (result.exitCode != 0) throw StateError('Owned process tree identity or stop failed; manual cleanup required');
    } else if (!_exited) {
      _process.kill();
    }
    try {
      await _processExit.timeout(const Duration(seconds: 5));
    } on TimeoutException {
      if (!_exited && !Platform.isWindows) _process.kill(ProcessSignal.sigkill);
      await _processExit.timeout(const Duration(seconds: 5));
    }
  }

  Future<void> _captureWindowsIdentity() async {
    if (!Platform.isWindows || _exited) return;
    try {
      final result = await Process.run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', '''
\$ErrorActionPreference = 'Stop'
\$child = \$null
try {
  \$child = [Diagnostics.Process]::GetProcessById($pid)
  \$boundHandle = \$child.Handle
  if (\$child.HasExited) { exit 4 }
  @{ path = \$child.MainModule.FileName; created = \$child.StartTime.ToUniversalTime().Ticks.ToString() } | ConvertTo-Json -Compress
} catch { exit 4 } finally { if (\$null -ne \$child) { \$child.Dispose() } }
''']);
      if (result.exitCode == 0) _windowsIdentity = jsonDecode(result.stdout as String) as Map<String, dynamic>;
    } catch (_) {
      // Never substitute a bare PID kill if identity inspection is unavailable.
    }
  }
}

/// Runs external programs. Tests swap in a fake so they never touch real devices.
abstract interface class ProcessRunner {
  /// Retains the child handle and drains both output streams into [logDir].
  Future<OwnedProcess> start(String executable, List<String> arguments, {required Directory logDir, String? workingDirectory, String? stdin, bool retainOutput = false});
  /// Runs [executable] to completion and returns its output.
  Future<ProcessResult> run(String executable, List<String> arguments, {String? stdin, String? workingDirectory});

  /// Starts [executable] and returns at once; the program keeps running (emulators).
  Future<void> startDetached(String executable, List<String> arguments);

  /// Runs [executable] with its output shown in this terminal; returns the exit code.
  Future<int> runInherited(String executable, List<String> arguments, {String? workingDirectory});
}

class IoProcessRunner implements ProcessRunner {
  const IoProcessRunner();

  @override
  Future<OwnedProcess> start(String executable, List<String> arguments, {required Directory logDir, String? workingDirectory, String? stdin, bool retainOutput = false}) async {
    if (retainOutput && stdin != null) throw ArgumentError('Retained output does not support stdin');
    await logDir.create(recursive: true);
    final batch = Platform.isWindows && (executable.endsWith('.bat') || executable.endsWith('.cmd'));
    if (retainOutput && batch) throw ArgumentError('Retained output requires a native executable');
    final process = retainOutput
        ? await _startFileBacked(executable, arguments, logDir, workingDirectory)
        : await Process.start(executable, arguments, workingDirectory: workingDirectory, runInShell: batch);
    final owned = _IoOwnedProcess(process, logDir, fileBacked: retainOutput);
    await owned._captureWindowsIdentity();
    try {
      if (stdin != null) process.stdin.write(stdin);
      await process.stdin.close();
    } catch (error, stack) {
      try { await owned.stop(); } catch (_) { /* preserve input failure */ }
      Error.throwWithStackTrace(error, stack);
    }
    return owned;
  }

  Future<Process> _startFileBacked(String executable, List<String> arguments, Directory logDir, String? workingDirectory) {
    if (!Platform.isWindows) {
      return Process.start('/bin/sh', ['-c',
        r'logdir=$1; shift; exec "$@" >"$logdir/$$.stdout.log" 2>"$logdir/$$.stderr.log"',
        'e2e', logDir.absolute.path, executable, ...arguments], workingDirectory: workingDirectory);
    }
    // Base64 is a transport for data, not shell quoting. The native argument
    // string separately follows Windows' backslash/double-quote rules.
    final data = base64Encode(utf8.encode(jsonEncode({
      'executable': executable, 'arguments': arguments.map(_windowsArgument).join(' '),
      'logDir': logDir.absolute.path, 'workingDirectory': workingDirectory ?? Directory.current.path,
    })));
    return Process.start('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', '''
\$config = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('$data')) | ConvertFrom-Json
\$outFile = Join-Path \$config.logDir "\$PID.stdout.log"
\$errFile = Join-Path \$config.logDir "\$PID.stderr.log"
\$output = \$null; \$errors = \$null; \$native = \$null; \$code = 127
try {
  \$output = [IO.FileStream]::new(\$outFile, 'Create', 'Write', 'ReadWrite', 1, 'Asynchronous')
  \$errors = [IO.FileStream]::new(\$errFile, 'Create', 'Write', 'ReadWrite', 1, 'Asynchronous')
  \$info = New-Object Diagnostics.ProcessStartInfo
  \$info.FileName = \$config.executable
  \$info.Arguments = \$config.arguments
  \$info.WorkingDirectory = \$config.workingDirectory
  \$info.UseShellExecute = \$false
  \$info.CreateNoWindow = \$true
  \$info.RedirectStandardOutput = \$true
  \$info.RedirectStandardError = \$true
  \$native = [Diagnostics.Process]::Start(\$info)
  \$copyOut = \$native.StandardOutput.BaseStream.CopyToAsync(\$output)
  \$copyErr = \$native.StandardError.BaseStream.CopyToAsync(\$errors)
  \$native.WaitForExit()
  [Threading.Tasks.Task]::WaitAll([Threading.Tasks.Task[]]@(\$copyOut, \$copyErr))
  \$code = \$native.ExitCode
} catch {
  if (\$null -ne \$errors) {
    \$bytes = [Text.Encoding]::UTF8.GetBytes(\$_.ToString())
    \$errors.Write(\$bytes, 0, \$bytes.Length)
  }
} finally {
  if (\$null -ne \$output) { \$output.Dispose() }
  if (\$null -ne \$errors) { \$errors.Dispose() }
  if (\$null -ne \$native) { \$native.Dispose() }
}
exit \$code
'''], workingDirectory: workingDirectory);
  }

  static String _windowsArgument(String argument) {
    final escaped = argument.replaceAllMapped(RegExp(r'(\\*)"'), (match) => '${match[1]}${match[1]}\\"')
        .replaceAllMapped(RegExp(r'(\\+)$'), (match) => '${match[1]}${match[1]}');
    return '"$escaped"';
  }

  @override
  Future<ProcessResult> run(String executable, List<String> arguments,
      {String? stdin, String? workingDirectory}) async {
    // runInShell lets Windows find .bat launchers such as flutter.bat.
    final process = await Process.start(executable, arguments,
        workingDirectory: workingDirectory, runInShell: Platform.isWindows);
    if (stdin != null) process.stdin.write(stdin);
    await process.stdin.close();
    final out = process.stdout.transform(systemEncoding.decoder).join();
    final err = process.stderr.transform(systemEncoding.decoder).join();
    final code = await process.exitCode;
    return ProcessResult(process.pid, code, await out, await err);
  }

  @override
  Future<void> startDetached(String executable, List<String> arguments) async {
    await Process.start(executable, arguments, mode: ProcessStartMode.detached);
  }

  @override
  Future<int> runInherited(String executable, List<String> arguments, {String? workingDirectory}) async {
    final process = await Process.start(executable, arguments,
        workingDirectory: workingDirectory, runInShell: Platform.isWindows, mode: ProcessStartMode.inheritStdio);
    return process.exitCode;
  }
}
