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
    expect(find.byTooltip('알림 측정'), findsNothing, reason: 'probe removed');
  });

  testWidgets('shows the connection state from the event stream', (tester) async {
    final core = await pumpHome(tester);
    core.controller.add(const ChatEvent.connectionState(directPeers: 2, mailboxOk: false, usingQueue: false, hasMailbox: false));
    // The event is delivered in a microtask after pump() has checked for a scheduled frame,
    // so pump until settled to build the frame its setState schedules.
    await tester.pumpAndSettle();
    expect(find.text('직접 연결 2'), findsOneWidget);
  });

  // M8 spec 4: the card shows while this device uses a server queue, and
  // goes with it (a mailbox qualified, or "서버 사용 안 함").
  testWidgets('the queue card follows using_queue', (tester) async {
    const card = '메일박스 없음 · 서버가 대신 받아 둬요';
    final core = await pumpHome(tester);
    expect(find.text(card), findsNothing);
    core.controller.add(const ChatEvent.connectionState(directPeers: 1, mailboxOk: false, usingQueue: true, hasMailbox: false));
    await tester.pumpAndSettle();
    expect(find.text(card), findsOneWidget);
    expect(find.text('직접 연결 1'), findsOneWidget);
    core.controller.add(const ChatEvent.connectionState(directPeers: 1, mailboxOk: false, usingQueue: false, hasMailbox: false));
    await tester.pumpAndSettle();
    expect(find.text(card), findsNothing);
  });

  // M8 spec 4 "메일박스가 있으면 지금처럼 카드가 없다": with "항상 서버 사용" and a
  // qualified mailbox the device uses a queue, but the card would say there is no mailbox.
  testWidgets('no queue card beside a qualified mailbox ("항상 서버 사용")', (tester) async {
    const card = '메일박스 없음 · 서버가 대신 받아 둬요';
    final core = await pumpHome(tester);
    core.controller.add(const ChatEvent.connectionState(directPeers: 1, mailboxOk: true, usingQueue: true, hasMailbox: true));
    await tester.pumpAndSettle();
    expect(find.text('직접 연결 1'), findsOneWidget);
    expect(find.text(card), findsNothing);
    // The mailbox stops qualifying: the card shows.
    core.controller.add(const ChatEvent.connectionState(directPeers: 1, mailboxOk: true, usingQueue: true, hasMailbox: false));
    await tester.pumpAndSettle();
    expect(find.text(card), findsOneWidget);
  });

  testWidgets('shows why the core did not start', (tester) async {
    await pumpHome(tester, startError: const CoreError(code: ErrorCode.invalidConfig, detail: 'no folder'));
    await tester.pump();
    expect(find.text('코어를 시작하지 못했어요 · invalidConfig'), findsOneWidget);
  });

  testWidgets('stream failure and completion have their own visible status', (tester) async {
    final core = await pumpHome(tester);
    core.controller.addError(StateError('stream failed'), StackTrace.current);
    await tester.pumpAndSettle();
    expect(find.textContaining('이벤트를 받지 못했어요'), findsOneWidget);
    expect(find.textContaining('코어를 시작하지 못했어요'), findsNothing);
    unawaited(core.controller.close());
    await tester.pumpAndSettle();
    expect(find.textContaining('이벤트 스트림이 종료됐어요'), findsOneWidget);
  });

  test('connection labels', () {
    expect(connectionLabel(const ChatEvent_ConnectionState(directPeers: 0, mailboxOk: false, usingQueue: false, hasMailbox: false)), '연결 없음');
    expect(connectionLabel(const ChatEvent_ConnectionState(directPeers: 0, mailboxOk: true, usingQueue: false, hasMailbox: false)), '메일박스 연결됨');
  });

  test('theme takes its colors from the tokens', () {
    final theme = buildAppTheme();
    expect(theme.scaffoldBackgroundColor, AppColors.background);
    expect(theme.colorScheme.primary, AppColors.accent);
  });
}
