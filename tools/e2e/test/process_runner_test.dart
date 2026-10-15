import 'dart:io';
import 'dart:convert';
import 'dart:async';
import 'dart:mirrors';

import '../bin/run.dart' as harness;

import 'package:chat_e2e/src/owned_resources.dart';
import 'package:chat_e2e/src/process_runner.dart';
import 'package:test/test.dart';

// The only simulated I/O is the selected stdin failure. Child lifetime,
// stdout/stderr, PID and socket protocol remain those of the real Process.
class InputFaultProcess implements Process {
  InputFaultProcess(this.real, this.stdin);
  final Process real;
  @override
  final IOSink stdin;
  @override
  int get pid => real.pid;
  @override
  Future<int> get exitCode => real.exitCode;
  @override
  Stream<List<int>> get stdout => real.stdout;
  @override
  Stream<List<int>> get stderr => real.stderr;
  @override
  bool kill([ProcessSignal signal = ProcessSignal.sigterm]) => real.kill(signal);
}

class FaultInput implements IOSink {
  FaultInput(this.real, this.primary, this.stack, this.writeFailure);
  final IOSink real;
  final Object primary;
  final StackTrace stack;
  final bool writeFailure;
  @override
  void write(Object? value) {
    if (writeFailure) Error.throwWithStackTrace(primary, stack);
    real.write(value);
  }
  @override
  Future<void> close() => Future<void>.error(primary, stack);
  @override
  dynamic noSuchMethod(Invocation invocation) => throw UnsupportedError('Unexpected fixture input operation');
}

void main() {
  late Directory logs;
  setUp(() async {
    logs = await Directory.systemTemp.createTemp('owned_process_');
  });
  tearDown(() async {
    await logs.delete(recursive: true);
  });

  for (final writeFailure in [false, true]) {
    for (final diagnosticCollision in [false, true]) {
      test('failed stdin ${writeFailure ? 'write' : 'close'} and reclaim retain child${diagnosticCollision ? ' despite diagnostic collision' : ''}', () async {
        final portFile = File('${logs.path}/live-port');
        final script = File('${logs.path}/live.dart')..writeAsStringSync('''
import 'dart:convert';
import 'dart:io';
Future<void> main(List<String> args) async {
  final server = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
  stdout.writeln('real child alive');
  File(args[0]).writeAsStringSync('\${server.port}');
  server.listen((socket) { socket.listen((bytes) {
    if (utf8.decode(bytes) == 'exit') exit(0);
    socket.add(utf8.encode('alive'));
  }); });
}
''');
        final real = await Process.start(Platform.resolvedExecutable, [script.path, portFile.path]);
        final primary = StateError('controlled input failure');
        final primaryStack = StackTrace.current;
        final delegated = InputFaultProcess(real, FaultInput(real.stdin, primary, primaryStack, writeFailure));
        final library = reflectClass(IoProcessRunner).owner as LibraryMirror;
        final ownedType = library.declarations[MirrorSystem.getSymbol('_IoOwnedProcess', library)] as ClassMirror;
        // Deliberately use the existing constructor without identity capture.
        // Windows therefore fails closed naturally, without private field edits.
        final owned = ownedType.newInstance(const Symbol(''), [delegated, logs]).reflectee as OwnedProcess;
        final runLibrary = (reflect(harness.main) as ClosureMirror).function.owner as LibraryMirror;
        final runType = runLibrary.declarations[MirrorSystem.getSymbol('_Run', runLibrary)] as ClassMirror;
        final runner = runType.newInstance(const Symbol(''), [logs]).reflectee as dynamic;
        final remaining = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
        final remainingPort = remaining.port;
        runner.owned.add('remaining socket', () async { await remaining.close(); });
        int? port;
        final diagnostic = File('${logs.path}/${real.pid}.start-failure.json');
        try {
          final deadline = DateTime.now().add(const Duration(seconds: 5));
          while (!portFile.existsSync()) {
            if (DateTime.now().isAfter(deadline)) fail('real child readiness deadline');
            await Future<void>.delayed(const Duration(milliseconds: 10));
          }
          port = int.parse(portFile.readAsStringSync());
          if (diagnosticCollision) Directory(diagnostic.path).createSync();
          Object? failure;
          try {
            await (library.invoke(MirrorSystem.getSymbol('_finishManagedStart', library),
                [delegated, owned, 'no\n', logs, Platform.resolvedExecutable]).reflectee as Future<OwnedProcess>);
          } catch (error) { failure = error; }
          if (!diagnosticCollision) {
            expect(diagnostic.existsSync(), isTrue, reason: 'failed live-child reclamation must leave ownership diagnostics');
            final record = jsonDecode(diagnostic.readAsStringSync()) as Map;
            expect(record['pid'], real.pid);
            expect(record['path'], Platform.resolvedExecutable);
            expect(record['primary'], primary.toString());
            expect(record['cleanup'], contains('identity unavailable'));
          }
          final handoff = failure as dynamic;
          expect(handoff.primary, same(primary));
          expect(handoff.primaryStack.toString(), primaryStack.toString());
          expect(handoff.process, same(owned));
          expect(handoff.cleanupFailure, isA<StateError>());
          if (diagnosticCollision) {
            expect(handoff.diagnosticFailure, isA<FileSystemException>());
            expect(handoff.toString(), contains('diagnostic write failed'));
          } else { expect(handoff.diagnosticFailure, isNull); }
          Object? caughtPrimary;
          StackTrace? caughtStack;
          try { runner.retainStartFailure(handoff, Platform.resolvedExecutable, null, false); }
          catch (error, stack) { caughtPrimary = error; caughtStack = stack; }
          expect(caughtPrimary, same(primary));
          expect(caughtStack.toString(), primaryStack.toString());
          await runner.stopMonitors();
          await runner.unwind();
          final failures = await runner.owned.close();
          expect(failures, hasLength(1));
          expect(failures.single, contains('identity unavailable'));
          expect(runner.cleanupFailures, contains(contains('startup stop:')));
          if (diagnosticCollision) expect(runner.cleanupFailures, contains(contains('startup diagnostic:')));
          final record = jsonDecode(File('${logs.path}/ownership.json').readAsStringSync()) as Map;
          expect((record['processes'] as List).single['pid'], real.pid);
          final ping = await Socket.connect(InternetAddress.loopbackIPv4, port);
          ping.write('ping');
          expect(utf8.decode(await ping.first.timeout(const Duration(seconds: 2))), 'alive');
          ping.destroy();
          await expectLater(Socket.connect(InternetAddress.loopbackIPv4, remainingPort), throwsA(isA<SocketException>()));
          final stopping = owned.stop();
          expect(owned.stop(), same(stopping));
          await expectLater(stopping, throwsA(same(handoff.cleanupFailure)));
          await expectLater(runner.command(Platform.resolvedExecutable, ['--version']), throwsA(isA<RunCancelled>()));
        } finally {
          if (port == null && portFile.existsSync()) port = int.parse(portFile.readAsStringSync());
          if (port != null) {
            final exitSocket = await Socket.connect(InternetAddress.loopbackIPv4, port);
            exitSocket.write('exit');
            await exitSocket.flush();
            exitSocket.destroy();
          }
          await real.exitCode.timeout(const Duration(seconds: 5));
          await owned.exitCode.timeout(const Duration(seconds: 5));
          try { await real.stdin.close(); } catch (_) { /* fixture already exited */ }
          await runner.owned.close();
          await remaining.close();
        }
      }, skip: Platform.isWindows ? false : 'Controlled missing Windows identity is a Windows-only fault boundary.');
    }
  }

  test('real startup input preserves ordinary broken-pipe failure after child closes stdin', () async {
    final ready = File('${logs.path}/closed-stdin.json');
    final source = File('${logs.path}/closed_stdin.dart')..writeAsStringSync('''
import 'dart:async';
import 'dart:convert';
import 'dart:ffi';
import 'dart:io';
Future<void> main(List<String> args) async {
  final server = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
  final record = <String, Object?>{'port': server.port, 'pid': pid, 'stage': 'before-close'};
  File(args[0]).writeAsStringSync(jsonEncode(record));
  String? originalPipe;
  if (Platform.isLinux) {
    try { originalPipe = Link('/proc/self/fd/0').targetSync(); } on FileSystemException { /* diagnostic only */ }
  }
  bool closed;
  if (Platform.isWindows) {
    final kernel = DynamicLibrary.open('kernel32.dll');
    final getHandle = kernel.lookupFunction<IntPtr Function(Int32), int Function(int)>('GetStdHandle');
    final closeHandle = kernel.lookupFunction<Int32 Function(IntPtr), int Function(int)>('CloseHandle');
    closed = closeHandle(getHandle(-10)) != 0;
  } else {
    final close = DynamicLibrary.process().lookupFunction<Int32 Function(Int32), int Function(int)>('close');
    closed = close(0) == 0;
  }
  record['stage'] = 'after-close';
  record['closed'] = closed;
  if (originalPipe != null && originalPipe.startsWith('pipe:[')) {
    try {
      var count = 0;
      for (final entry in Directory('/proc/self/fd').listSync(followLinks: false)) {
        try { if (Link(entry.path).targetSync() == originalPipe) count++; } on FileSystemException { /* fd closed during enumeration */ }
      }
      record['same_pipe_fds'] = count;
    } on FileSystemException { /* diagnostic only */ }
  }
  File(args[0]).writeAsStringSync(jsonEncode(record));
  if (!closed) throw StateError('fixture stdin close failed');
  server.listen((socket) { socket.listen((_) => exit(0)); });
  Timer(const Duration(seconds: 15), () => exit(3));
}
''');
    final real = await Process.start(Platform.resolvedExecutable, [source.path, ready.path]);
    OwnedProcess? owned;
    Future<OwnedProcess>? pending;
    Object? failure;
    final elapsed = Stopwatch();
    try {
      // Use the real startup-input helper after the reader closes. Writing before
      // that handshake can fill the pipe and hit Dart 3.12.2's dropped Linux
      // EPOLLERR event instead of the ordinary synchronous broken-pipe path.
      // The separate managed-stdin test covers the public start entry point.
      final library = reflectClass(IoProcessRunner).owner as LibraryMirror;
      final type = library.declarations[MirrorSystem.getSymbol('_IoOwnedProcess', library)] as ClassMirror;
      final instance = type.newInstance(const Symbol(''), [real, logs]);
      owned = instance.reflectee as OwnedProcess;
      await instance.invoke(MirrorSystem.getSymbol('_captureWindowsIdentity', library), []).reflectee;
      final deadline = DateTime.now().add(const Duration(seconds: 10));
      while (true) {
        try {
          if (ready.existsSync()) {
            final record = jsonDecode(ready.readAsStringSync()) as Map;
            if (record['stage'] == 'after-close') {
              expect(record['closed'], isTrue, reason: 'fixture must close its real stdin before input');
              break;
            }
          }
        } on FileSystemException { /* readiness record is still being written */
        } on FormatException { /* readiness record is still being written */ }
        if (DateTime.now().isAfter(deadline)) fail('real child stdin readiness deadline');
        await Future<void>.delayed(const Duration(milliseconds: 10));
      }
      elapsed.start();
      pending = library.invoke(MirrorSystem.getSymbol('_finishManagedStart', library),
          [real, owned, 'x' * (1024 * 1024), logs, Platform.resolvedExecutable]).reflectee as Future<OwnedProcess>;
      try {
        await pending.timeout(const Duration(seconds: 10));
      } catch (error) { failure = error; }
      Map diagnostic = <String, Object?>{};
      try {
        if (ready.existsSync()) diagnostic = jsonDecode(ready.readAsStringSync()) as Map;
      } on FileSystemException {
        diagnostic['stage'] = 'unreadable';
      } on FormatException {
        diagnostic['stage'] = 'unreadable';
      }
      expect(failure, isA<SocketException>(), reason: jsonEncode({
        'stage': diagnostic['stage'] ?? 'not-ready', 'closed': diagnostic['closed'],
        'same_pipe_fds': diagnostic['same_pipe_fds'], 'elapsed_ms': elapsed.elapsedMilliseconds,
        'error_type': failure.runtimeType.toString(),
      }));
      expect(ready.existsSync(), isTrue);
      final record = jsonDecode(ready.readAsStringSync()) as Map;
      await expectLater(Socket.connect(InternetAddress.loopbackIPv4, record['port'] as int), throwsA(isA<SocketException>()));
      if (Platform.isWindows) {
        final lookup = await Process.run('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
          'if (Get-Process -Id ${record['pid']} -ErrorAction SilentlyContinue) { exit 0 }; exit 3']);
        expect(lookup.exitCode, 3, reason: 'the exact fixture native child must already have exited');
      } else if (Platform.isLinux) {
        expect(Directory('/proc/${record['pid']}').existsSync(), isFalse);
      }
      expect(logs.listSync().where((f) => f.path.endsWith('.start-failure.json')), isEmpty);
    } finally {
      try {
        if (ready.existsSync()) {
          final record = jsonDecode(ready.readAsStringSync()) as Map;
          try {
            final release = await Socket.connect(InternetAddress.loopbackIPv4, record['port'] as int)
                .timeout(const Duration(seconds: 2));
            release.write('exit');
            await release.flush();
            release.destroy();
          } on SocketException { /* already reclaimed */ }
        }
      } catch (error) {
        print('fixture release type: ${error.runtimeType}');
      }
      // timeout() does not cancel the admitted start. Observe its completion
      // after releasing the exact fixture, and reclaim any late success.
      try {
        if (pending != null) {
          final child = await pending.timeout(const Duration(seconds: 5));
          await child.stop();
        }
      } catch (error) {
        if (error is OwnedProcessStartFailure) {
          try { await error.process.stop(); } catch (cleanup) { print('fixture cleanup type: ${cleanup.runtimeType}'); }
        } else if (error is! SocketException) {
          print('fixture pending-start drain type: ${error.runtimeType}');
        }
      }
      // Retain cleanup ownership even if setup/readiness failed before input.
      try {
        if (owned != null) {
          await owned.stop();
        } else {
          real.kill();
          await real.exitCode.timeout(const Duration(seconds: 5));
        }
      } catch (error) { print('fixture owned cleanup type: ${error.runtimeType}'); }
    }
  });

  test('managed stdin reaches a real child before EOF and logs are flushed', () async {
    final source = File('${logs.path}/stdin_child.dart')..writeAsStringSync('''
import 'dart:convert';
import 'dart:io';
Future<void> main() async {
  final input = await stdin.transform(utf8.decoder).join();
  stdout.write(input == 'no\\n' ? 'received-no-before-eof' : 'wrong-input');
}
''');
    final child = await const IoProcessRunner().start(Platform.resolvedExecutable,
        [source.path], logDir: logs, stdin: 'no\n');
    try {
      expect(await child.exitCode.timeout(const Duration(seconds: 10)), 0);
      expect(File('${logs.path}/${child.pid}.stdout.log').readAsStringSync(), 'received-no-before-eof');
    } finally { await child.stop(); }
  });

  test('retained mode rejects stdin before launching a child', () async {
    await expectLater(const IoProcessRunner().start(Platform.resolvedExecutable,
        ['--version'], logDir: logs, stdin: 'no\n', retainOutput: true), throwsArgumentError);
    expect(logs.listSync(), isEmpty);
  });

  for (final failedStream in ['stdout', 'stderr']) {
    for (final exitBeforeConsumer in [false, true]) {
      test(
        '$failedStream sink failure stays observed with ${exitBeforeConsumer ? 'exited' : 'live'} child and late consumer',
        () async {
          final portFile = File('${logs.path}/failure-port');
          final source = File('${logs.path}/failure_child.dart')
            ..writeAsStringSync('''
import 'dart:io';
Future<void> main(List<String> args) async {
  final server = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
  File(args[0]).writeAsStringSync('\${server.port}');
  server.listen((socket) { socket.listen((_) => exit(0)); });
}
''');
          final unhandled = <Object>[];
          Object? observedError;
          Object? stopError;
          Object? scenarioError;
          bool aliveBeforeConsumer = false;
          bool exitedAfterStop = false;
          late int port;
          final complete = Completer<void>();
          runZonedGuarded(() async {
            Process? process;
            try {
              process = await Process.start(Platform.resolvedExecutable, [source.path, portFile.path]);
              await process.stdin.close();
              var exited = false;
              final actualExit = process.exitCode.then((code) {
                exited = true;
                return code;
              });
              Directory('${logs.path}/${process.pid}.$failedStream.log').createSync();
              final library = reflectClass(IoProcessRunner).owner as LibraryMirror;
              final type = library.declarations[MirrorSystem.getSymbol('_IoOwnedProcess', library)] as ClassMirror;
              final instance = type.newInstance(const Symbol(''), [process, logs]);
              final owned = instance.reflectee as OwnedProcess;
              // Mirrors reproduce normal start setup only; public lifecycle/stop
              // and a real failing filesystem sink drive the assertions.
              await instance.invoke(MirrorSystem.getSymbol('_captureWindowsIdentity', library), []).reflectee;
              final deadline = DateTime.now().add(const Duration(seconds: 10));
              while (!portFile.existsSync()) {
                if (DateTime.now().isAfter(deadline)) throw StateError('Failure fixture did not start');
                await Future<void>.delayed(const Duration(milliseconds: 10));
              }
              port = int.parse(portFile.readAsStringSync());
              await Future<void>.delayed(const Duration(milliseconds: 100));
              if (exitBeforeConsumer) {
                final socket = await Socket.connect(InternetAddress.loopbackIPv4, port);
                socket.write('exit');
                await socket.flush();
                await socket.close();
                await actualExit;
                await Future<void>.delayed(const Duration(milliseconds: 100));
              }
              aliveBeforeConsumer = !exited;
              try {
                await owned.exitCode.timeout(const Duration(seconds: 1));
              } catch (error) {
                observedError = error;
              }
              try {
                await owned.stop();
              } catch (error) {
                stopError = error;
              }
              await actualExit.timeout(const Duration(seconds: 10));
              exitedAfterStop = exited;
            } catch (error) {
              scenarioError = error;
            } finally {
              // The fixture owns this exact live Process; always reclaim it even
              // while testing a broken implementation.
              process?.kill();
              await process?.exitCode;
              complete.complete();
            }
          }, (error, _) => unhandled.add(error));
          await complete.future.timeout(const Duration(seconds: 20));
          expect(scenarioError, isNull);
          expect(
            unhandled,
            isEmpty,
            reason: 'Neither output nor lifecycle failure may escape before a consumer attaches',
          );
          expect(aliveBeforeConsumer, !exitBeforeConsumer);
          expect(observedError, isA<FileSystemException>());
          expect(stopError, same(observedError), reason: 'stop must preserve the original logging failure');
          expect(exitedAfterStop, isTrue);
          await expectLater(Socket.connect(InternetAddress.loopbackIPv4, port), throwsA(isA<SocketException>()));
        },
      );
    }
  }

  test(
    'stop bounds inherited output drains and continues remaining cleanup',
    () async {
      final release = File('${logs.path}/release');
      final ready = File('${logs.path}/ready');
      final source = File('${logs.path}/inherited_output.dart')
        ..writeAsStringSync('''
import 'dart:async';
import 'dart:io';
Future<void> main(List<String> args) async {
  if (args[0] == 'parent') {
    await Process.start(Platform.resolvedExecutable,
      [Platform.script.toFilePath(), 'grandchild', args[1], args[2]],
      mode: ProcessStartMode.inheritStdio);
    exit(0);
  }
  stdout.writeln('inherited stdout');
  stderr.writeln('inherited stderr');
  File(args[2]).writeAsStringSync('ready');
  final deadline = DateTime.now().add(const Duration(seconds: 20));
  while (!File(args[1]).existsSync() && DateTime.now().isBefore(deadline)) {
    await Future<void>.delayed(const Duration(milliseconds: 10));
  }
}
''');
      final process = await Process.start(Platform.resolvedExecutable, [
        source.path,
        'parent',
        release.path,
        ready.path,
      ]);
      final library = reflectClass(IoProcessRunner).owner as LibraryMirror;
      final type =
          library.declarations[MirrorSystem.getSymbol(
                '_IoOwnedProcess',
                library,
              )]
              as ClassMirror;
      // Constructor setup only: real grandchild inheritance holds both output
      // pipes open after the native parent exits. No stale descendant is signaled.
      final owned =
          type.newInstance(const Symbol(''), [process, logs]).reflectee
              as OwnedProcess;
      final resources = OwnedResources();
      var remainingCleaned = false;
      var lifecycleComplete = false;
      final lifecycle = owned.exitCode.then((code) {
        lifecycleComplete = true;
        return code;
      });
      Future<void>? stopping;
      Future<List<String>>? closing;
      try {
        await process.stdin.close();
        expect(await process.exitCode.timeout(const Duration(seconds: 5)), 0);
        final deadline = DateTime.now().add(const Duration(seconds: 5));
        while (!ready.existsSync()) {
          if (DateTime.now().isAfter(deadline)) {
            fail('Inherited output fixture did not start');
          }
          await Future<void>.delayed(const Duration(milliseconds: 10));
        }
        expect(lifecycleComplete, isFalse);
        resources.add('remaining', () async {
          remainingCleaned = true;
        });
        resources.add('process', () => stopping = owned.stop());
        closing = resources.close();
        // A test deadline throws out of this await; it cannot satisfy the
        // assertions for a returned production stop failure below.
        final failures = await closing.timeout(const Duration(seconds: 8));
        expect(remainingCleaned, isTrue);
        expect(failures, hasLength(1));
        expect(failures.single, startsWith('process: TimeoutException'));
        expect(lifecycleComplete, isFalse);
        expect(owned.stop(), same(stopping));
        await expectLater(stopping, throwsA(isA<TimeoutException>()));
      } finally {
        // The test owns the release protocol, not a stale PID. Wait for actual
        // inherited pipe EOF before deleting fixture files or finishing the test.
        release.writeAsStringSync('release');
        expect(await lifecycle.timeout(const Duration(seconds: 5)), 0);
        await closing;
        await process.exitCode;
      }
      expect(remainingCleaned, isTrue);
      expect(
        File('${logs.path}/${owned.pid}.stdout.log').readAsStringSync(),
        contains('inherited stdout'),
      );
      expect(
        File('${logs.path}/${owned.pid}.stderr.log').readAsStringSync(),
        contains('inherited stderr'),
      );
      expect(owned.stop(), same(stopping));
      await expectLater(owned.stop(), throwsA(isA<TimeoutException>()));
    },
  );

  test('managed child captures output and exit before completion', () async {
    final child = await const IoProcessRunner().start(Platform.resolvedExecutable, ['--version'], logDir: logs);
    expect(child.pid, greaterThan(0));
    expect(await child.exitCode, 0);
    final files = logs.listSync().whereType<File>().toList();
    expect(
      files.map((file) => file.path.split(Platform.pathSeparator).last),
      containsAll(['${child.pid}.stdout.log', '${child.pid}.stderr.log']),
    );
    expect(files.map((file) => file.readAsStringSync()).join(), contains('Dart SDK version'));
    await child.stop();
    await child.stop();
  });

  test('unknown managed executable fails visibly', () async {
    await expectLater(
      const IoProcessRunner().start('nonexistent-chat-e2e-executable', [], logDir: logs),
      throwsA(isA<ProcessException>()),
    );
  });

  test('stop terminates only the retained live child', () async {
    final script = File('${logs.path}/child.dart')
      ..writeAsStringSync("import 'dart:async'; void main() { Timer.periodic(const Duration(seconds: 1), (_) {}); }");
    final child = await const IoProcessRunner().start(Platform.resolvedExecutable, [script.path], logDir: logs);
    await child.stop().timeout(const Duration(seconds: 10));
    expect(await child.exitCode, isNot(0));
  });

  test('file-backed native launch preserves executable spaces and literal native arguments', () async {
    final source = File('${logs.path}/native fixture.dart')
      ..writeAsStringSync('''
import 'dart:convert';
import 'dart:io';
void main(List<String> args) { stdout.writeln(jsonEncode(args)); stderr.write('native stderr'); }
''');
    final executable = File('${logs.path}/native fixture${Platform.isWindows ? '.exe' : ''}');
    final compiled = await Process.run(Platform.resolvedExecutable, [
      'compile',
      'exe',
      source.path,
      '-o',
      executable.path,
    ]);
    expect(compiled.exitCode, 0, reason: '${compiled.stderr}');
    final arguments = ['space value', 'say"hello', r'C:\trailing\', r'one\"two', '', 'x&y', '%PATH%', 'unicode 🌱'];
    final child = await const IoProcessRunner().start(executable.path, arguments, logDir: logs, retainOutput: true);
    expect(await child.exitCode, 0);
    expect(jsonDecode(File('${logs.path}/${child.pid}.stdout.log').readAsStringSync()), arguments);
    expect(File('${logs.path}/${child.pid}.stderr.log').readAsStringSync(), 'native stderr');
    await child.stop();
  });

  test('file-backed missing executable reports nonzero with diagnostic output', () async {
    final child = await const IoProcessRunner().start(
      'nonexistent-chat-e2e-executable',
      [],
      logDir: logs,
      retainOutput: true,
    );
    expect(await child.exitCode, isNot(0));
    expect(File('${logs.path}/${child.pid}.stderr.log').readAsStringSync(), isNotEmpty);
  });

  test('file-backed current-run stop terminates wrapper and native child', () async {
    final portFile = File('${logs.path}/native-port');
    final source = File('${logs.path}/native child.dart')
      ..writeAsStringSync('''
import 'dart:io';
Future<void> main(List<String> args) async {
  final server = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
  File(args[0]).writeAsStringSync('\${server.port}');
  server.listen((socket) => socket.destroy());
}
''');
    final child = await const IoProcessRunner().start(
      Platform.resolvedExecutable,
      [source.path, portFile.path],
      logDir: logs,
      retainOutput: true,
    );
    final deadline = DateTime.now().add(const Duration(seconds: 10));
    while (!portFile.existsSync()) {
      if (DateTime.now().isAfter(deadline)) fail('File-backed child did not start');
      await Future<void>.delayed(const Duration(milliseconds: 20));
    }
    final port = int.parse(portFile.readAsStringSync());
    await child.stop();
    expect(await child.exitCode, isNot(0));
    await expectLater(Socket.connect(InternetAddress.loopbackIPv4, port), throwsA(isA<SocketException>()));
  });

  test(
    'Windows owned batch stop terminates its child tree and preserves an unrelated holder',
    () async {
      final holder = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
      final holderSubscription = holder.listen((socket) => socket.destroy());
      addTearDown(() async {
        await holderSubscription.cancel();
        await holder.close();
      });
      final portFile = File('${logs.path}/port');
      final script = File('${logs.path}/tree_child.dart')
        ..writeAsStringSync('''
import 'dart:async';
import 'dart:io';
Future<void> main(List<String> args) async {
  final server = await ServerSocket.bind(InternetAddress.loopbackIPv4, 0);
  server.listen((socket) => socket.destroy());
  File(args[0]).writeAsStringSync('\${server.port}');
  Timer(const Duration(seconds: 7), () => exit(0));
}
''');
      final batch = File('${logs.path}/tree.bat')
        ..writeAsStringSync('@echo off\r\n"${Platform.resolvedExecutable}" "${script.path}" "${portFile.path}"\r\n');
      final child = await const IoProcessRunner().start(batch.path, [], logDir: logs);
      final deadline = DateTime.now().add(const Duration(seconds: 5));
      while (!portFile.existsSync()) {
        if (DateTime.now().isAfter(deadline)) fail('Owned child did not start');
        await Future<void>.delayed(const Duration(milliseconds: 20));
      }
      final port = int.parse(portFile.readAsStringSync());
      final watch = Stopwatch()..start();
      await child.stop();
      expect(
        watch.elapsed,
        lessThan(const Duration(seconds: 4)),
        reason: 'stop must terminate descendants, not wait for their self-expiry',
      );
      await expectLater(Socket.connect(InternetAddress.loopbackIPv4, port), throwsA(isA<SocketException>()));
      final unrelated = await Socket.connect(InternetAddress.loopbackIPv4, holder.port);
      unrelated.destroy();
    },
    skip: !Platform.isWindows ? 'Windows batch process tree behavior' : false,
  );
}
