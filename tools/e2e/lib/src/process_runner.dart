import 'dart:io';

/// Runs external programs. Tests swap in a fake so they never touch real devices.
abstract interface class ProcessRunner {
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
