import 'dart:async';

import 'package:chat_app/core_client.dart';
import 'package:chat_app/home/home_screen.dart';
import 'package:chat_app/src/rust/api/chat.dart';
import 'package:chat_app/theme/app_theme.dart';
import 'package:chat_app/theme/tokens.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

class FakeCoreClient implements CoreClient {
  FakeCoreClient({this.startError});

  final Object? startError;
  final controller = StreamController<ChatEvent>.broadcast();

  @override
  String get version => '9.9.9-test';

  @override
  Future<CoreInfo> start() async {
    final error = startError;
    if (error != null) throw error;
    return const CoreInfo(version: '9.9.9-test', instanceId: 'test');
  }

  @override
  Stream<ChatEvent> events() => controller.stream;
}

Future<FakeCoreClient> pumpHome(WidgetTester tester, {Object? startError}) async {
  final core = FakeCoreClient(startError: startError);
  await tester.pumpWidget(MaterialApp(theme: buildAppTheme(), home: HomeScreen(core: core)));
  await tester.pump();
  return core;
}

void main() {
  testWidgets('shows the empty state and the core version', (tester) async {
    await pumpHome(tester);
    expect(find.text('아직 대화가 없어요'), findsOneWidget);
    expect(find.text('코어 9.9.9-test'), findsOneWidget);
    expect(find.text('코어 시작 중'), findsOneWidget);
  });

  testWidgets('shows the connection state from the event stream', (tester) async {
    final core = await pumpHome(tester);
    core.controller.add(const ChatEvent.connectionState(directPeers: 2, mailboxOk: false));
    // The event is delivered in a microtask after pump() has checked for a scheduled frame,
    // so pump until settled to build the frame its setState schedules.
    await tester.pumpAndSettle();
    expect(find.text('직접 연결 2'), findsOneWidget);
  });

  testWidgets('shows why the core did not start', (tester) async {
    await pumpHome(tester, startError: const CoreError(code: ErrorCode.invalidConfig, detail: 'no folder'));
    await tester.pump();
    expect(find.text('코어를 시작하지 못했어요 · invalidConfig'), findsOneWidget);
  });

  test('connection labels', () {
    expect(connectionLabel(const ChatEvent_ConnectionState(directPeers: 0, mailboxOk: false)), '연결 없음');
    expect(connectionLabel(const ChatEvent_ConnectionState(directPeers: 0, mailboxOk: true)), '메일박스 연결됨');
    expect(
      connectionLabel(const ChatEvent_ConnectionState(directPeers: 0, mailboxOk: false, queueOk: true)),
      '서버 큐 사용 중',
    );
  });

  test('theme takes its colors from the tokens', () {
    final theme = buildAppTheme();
    expect(theme.scaffoldBackgroundColor, AppColors.background);
    expect(theme.colorScheme.primary, AppColors.accent);
  });
}
