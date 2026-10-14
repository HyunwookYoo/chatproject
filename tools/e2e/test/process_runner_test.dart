import 'dart:io';
import 'dart:convert';
import 'dart:async';
import 'dart:mirrors';

import 'package:chat_e2e/src/owned_resources.dart';
import 'package:chat_e2e/src/process_runner.dart';
import 'package:test/test.dart';

void main() {
  late Directory logs;
  setUp(() async {
    logs = await Directory.systemTemp.createTemp('owned_process_');
  });
  tearDown(() async {
    await logs.delete(recursive: true);
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
