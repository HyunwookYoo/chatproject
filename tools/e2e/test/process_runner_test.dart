import 'dart:io';
import 'dart:convert';

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
