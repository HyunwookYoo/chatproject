import 'dart:io';
import 'dart:mirrors';

import 'package:test/test.dart';

import '../bin/run.dart' as cli;

void main() {
  final library =
      (reflect(cli.main) as ClosureMirror).function.owner as LibraryMirror;
  ClassMirror type(String name) =>
      library.declarations[MirrorSystem.getSymbol(name, library)]
          as ClassMirror;
  late Directory logs;
  late InstanceMirror run;
  void phase(String name) {
    final values =
        type('_Phase').getField(const Symbol('values')).reflectee as List;
    run.setField(
      const Symbol('phase'),
      values.singleWhere((v) => v.value == name),
    );
  }

  Map projection() =>
      run.getField(const Symbol('safeFailure')).reflectee as Map;
  setUp(() async {
    logs = await Directory.systemTemp.createTemp('diagnostic_fixture_');
    run = type('_Run').newInstance(const Symbol(''), [logs]);
  });
  tearDown(() async => logs.delete(recursive: true));

  test(
    'first typed command failure survives cleanup without private values',
    () {
      phase('build_android');
      final error = type(
        '_CommandFailure',
      ).newInstance(const Symbol(''), [17, 'PRIVATE_TOKEN']).reflectee;
      expect(error, isA<StateError>());
      expect(
        (error as StateError).message,
        'Command failed (17): PRIVATE_TOKEN',
      );
      run.invoke(const Symbol('recordFailure'), [error]);
      phase('cleanup');
      run.invoke(const Symbol('recordFailure'), [
        const FileSystemException('PRIVATE_TOKEN'),
      ]);
      expect(projection(), {
        'phase': 'build_android',
        'failure_kind': 'command_exit',
        'command_exit_code': 17,
      });
      expect(projection().toString(), isNot(contains('PRIVATE_TOKEN')));
    },
  );

  test('cleanup-only and unknown failures use fixed categories', () {
    phase('cleanup');
    run.invoke(const Symbol('recordFailure'), [null]);
    expect(projection(), {
      'phase': 'cleanup',
      'failure_kind': 'cleanup',
      'command_exit_code': null,
    });
    run = type('_Run').newInstance(const Symbol(''), [logs]);
    phase('android_participant');
    run.invoke(const Symbol('fail'), ['PRIVATE_TOKEN', StackTrace.current]);
    expect(projection(), {
      'phase': 'android_participant',
      'failure_kind': 'other',
      'command_exit_code': null,
    });
  });
}
