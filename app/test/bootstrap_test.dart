import 'dart:io';
import 'dart:async';

import 'package:chat_app/bootstrap.dart';
import 'package:chat_app/main.dart';
import 'package:chat_app/startup_error_app.dart';
import 'package:chat_app/src/rust/api/chat.dart' as rust;
import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

import 'support/fake_network_client.dart';

void main() {
  late DebugPrintCallback previousPrint;
  setUp(() {
    previousPrint = debugPrint;
    debugPrint = (message, {wrapWidth}) {};
  });
  tearDown(() => debugPrint = previousPrint);
  testWidgets('network UI cannot start before hook readiness', (tester) async {
    debugPrint = previousPrint;
    final calls = <String>[];
    final client = OrderedClient(calls);
    final ready = Completer<void>();
    final boot = bootstrap(
      ['--control-url=ws://127.0.0.1:47000', '--name=fixture'],
      initializeRust: () async => calls.add('rust'),
      createClient: (_) => client,
      startTestHooks: (_, _) {
        calls.add('hooks');
        return ready.future;
      },
    );
    await tester.pumpWidget(
      FutureBuilder<Widget>(
        future: boot,
        builder: (_, snapshot) => snapshot.data ?? const SizedBox(),
      ),
    );
    await tester.pump();
    expect(calls, ['rust', 'core', 'hooks']);
    expect(find.byTooltip('네트워크 측정'), findsNothing);
    ready.complete();
    await tester.pumpAndSettle();
    expect(find.byTooltip('네트워크 측정'), findsOneWidget);
    await tester.tap(find.byTooltip('네트워크 측정'));
    await tester.pumpAndSettle();
    expect(calls, ['rust', 'core', 'hooks', 'core']);
    await tester.tap(find.text('네트워크 시작'));
    await tester.pumpAndSettle();
    expect(calls, ['rust', 'core', 'hooks', 'core', 'network']);
    await tester.pumpWidget(const SizedBox());
    await client.controller.close();
  });

  test('hook configuration is awaited before exposing UI', () async {
    final client = FakeNetworkClient();
    final ready = Completer<void>();
    final calls = <String>[];
    var exposed = false;
    final boot =
        bootstrap(
          ['--control-url', 'ws://127.0.0.1:47000', '--name=fixture'],
          initializeRust: () async => calls.add('rust'),
          createClient: (_) {
            calls.add('client');
            return client;
          },
          startTestHooks: (url, name) {
            expect(url, 'ws://127.0.0.1:47000');
            expect(name, 'fixture');
            calls.add('hooks');
            return ready.future;
          },
        ).then((widget) {
          exposed = true;
          return widget;
        });
    await Future<void>.delayed(Duration.zero);
    expect(calls, ['rust', 'client', 'hooks']);
    expect(exposed, isFalse);
    ready.complete();
    expect(await boot, isA<ChatApp>());
    await client.controller.close();
  });

  test('hook failure exposes startup error with fixed diagnostics', () async {
    final client = FakeNetworkClient();
    final logs = <String?>[];
    debugPrint = (message, {wrapWidth}) => logs.add(message);
    final app = await bootstrap(
      ['--control-url=ws://127.0.0.1:47000', '--name=fixture'],
      initializeRust: () async {},
      createClient: (_) => client,
      startTestHooks: (_, _) async =>
          throw StateError('private control detail'),
    );
    expect(app, isA<StartupErrorApp>());
    expect((app as StartupErrorApp).reason, 'Debug control startup failed');
    expect(logs.join(), isNot(contains('private control detail')));
    await client.controller.close();
  });

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

class OrderedClient extends FakeNetworkClient {
  OrderedClient(this.calls);
  final List<String> calls;
  @override
  Future<rust.CoreInfo> start() async {
    calls.add('core');
    return super.start();
  }

  @override
  Future<rust.NetworkInfo> startNetwork() async {
    calls.add('network');
    return super.startNetwork();
  }
}
