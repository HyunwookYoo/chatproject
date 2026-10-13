import 'dart:io';

import 'package:chat_app/bootstrap.dart';
import 'package:chat_app/main.dart';
import 'package:chat_app/startup_error_app.dart';
import 'package:chat_app/src/rust/api/chat.dart' as rust;
import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';

import 'support/fake_network_client.dart';

void main() {
  late DebugPrintCallback previousPrint;
  setUp(() {
    previousPrint = debugPrint;
    debugPrint = (message, {wrapWidth}) {};
  });
  tearDown(() => debugPrint = previousPrint);
  test(
    'Rust initialization failure renders startup error and reports stack',
    () async {
      final logs = <String?>[];
      final previous = debugPrint;
      debugPrint = (message, {wrapWidth}) => logs.add(message);
      addTearDown(() => debugPrint = previous);
      final app = await bootstrap(
        [],
        initializeRust: () async {
          throw StateError('init failed');
        },
        createClient: (_) => throw StateError('must not construct'),
      );
      expect(app, isA<StartupErrorApp>());
      expect((app as StartupErrorApp).reason, contains('init failed'));
      expect(logs.join(), contains('ChatProject startup failed'));
      expect(logs.join(), contains('bootstrap_test.dart'));
    },
  );

  test('core start failure becomes StartupErrorApp', () async {
    final client = FakeNetworkClient()..startError = StateError('start failed');
    final app = await bootstrap(
      [],
      initializeRust: () async {},
      createClient: (_) => client,
    );
    expect(app, isA<StartupErrorApp>());
    expect((app as StartupErrorApp).reason, contains('start failed'));
    await client.controller.close();
  });

  test('structured startup failure keeps its code visible', () async {
    final client = FakeNetworkClient()
      ..startError = const rust.CoreError(
        code: rust.ErrorCode.alreadyStartedDifferentConfig,
        detail: 'private path',
      );
    final app = await bootstrap(
      [],
      initializeRust: () async {},
      createClient: (_) => client,
    );
    expect(
      (app as StartupErrorApp).reason,
      contains('alreadyStartedDifferentConfig'),
    );
    await client.controller.close();
  });

  test('Release rejects debug control flags before Rust can initialize', () {
    for (final flag in [
      '--debug-hooks',
      '--control-url',
      '--control-url=http://127.0.0.1',
      '--name',
      '--name=probe',
    ]) {
      expect(
        () => startupDataDir([flag], releaseMode: true),
        throwsFormatException,
      );
    }
    expect(
      startupDataDir([
        '--control-url=http://127.0.0.1',
        '--name=probe',
      ], releaseMode: false),
      isNull,
    );
    expect(
      () => startupDataDir(['--data-dir'], releaseMode: false),
      throwsFormatException,
    );
  });

  test(
    'absolute override reaches client and relative override is rejected',
    () async {
      final client = FakeNetworkClient();
      String? received;
      final path = Platform.isWindows ? 'C:/probe/data' : '/probe/data';
      final app = await bootstrap(
        ['--data-dir', path],
        initializeRust: () async {},
        createClient: (dataDir) {
          received = dataDir;
          return client;
        },
      );
      expect(app, isA<ChatApp>());
      expect(received, path);
      final invalid = await bootstrap(
        ['--data-dir=relative'],
        initializeRust: () async {},
        createClient: (_) => client,
      );
      expect(invalid, isA<StartupErrorApp>());
      await client.controller.close();
    },
  );
}
