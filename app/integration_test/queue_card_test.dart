// M8 Task 11 on a real Windows or Android device (CI jobs `windows` and `android` run every
// integration file): the home card while this device uses a server queue, and "서버 사용 안 함"
// in the settings taking it away. The core is a fake that answers the mode with the
// `ConnectionState` the account actor publishes (`using_queue`); the real core path is the
// m8 network scenario (Task 12).
import 'dart:async';
import 'dart:io';

import 'package:chat_app/core_client.dart';
import 'package:chat_app/home/home_screen.dart';
import 'package:chat_app/mailbox/mailbox_client.dart';
import 'package:chat_app/mailbox/mailbox_model.dart';
import 'package:chat_app/mailbox/mailbox_platform.dart';
import 'package:chat_app/mailbox/mailbox_settings_screen.dart';
import 'package:chat_app/src/rust/api/chat.dart';
import 'package:chat_app/theme/app_theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

const _card = '메일박스 없음 · 서버가 대신 받아 둬요';

class _Core implements CoreClient {
  final stream = StreamController<ChatEvent>.broadcast();
  void using(bool using) => stream.add(
    ChatEvent.connectionState(directPeers: 0, mailboxOk: false, usingQueue: using, hasMailbox: false),
  );
  @override
  String get version => 'integration';
  @override
  Future<CoreInfo> start() async =>
      const CoreInfo(version: 'integration', instanceId: 'integration');
  @override
  Stream<ChatEvent> events() => stream.stream;
}

class _Client implements MailboxClient {
  _Client(this.core);
  final _Core core;
  QueueMode mode = QueueMode.auto;
  @override
  Future<MailboxStateView> mailboxState() async => const MailboxStateView(
    enabled: false,
    firstLinkShown: true,
    uptime7DPermille: 0,
  );
  @override
  Future<QueueStateView> queueState() async => QueueStateView(
    mode: mode,
    verdict: Verdict.noMailbox,
    usingQueue: mode != QueueMode.never,
    uptime7DPermille: 0,
  );
  @override
  Future<void> queueSetMode(QueueMode mode) async {
    this.mode = mode;
    core.using(mode != QueueMode.never);
  }

  @override
  Future<void> startNetwork() async {}
  @override
  Future<void> mailboxForeground() async {}
  @override
  Future<int> mailboxBackground() async => 0;
  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

/// No tray, service or registry change on the CI machine.
class _Platform implements MailboxPlatform {
  @override
  Future<bool> setMailbox(bool on) async => true;
  @override
  Future<bool> autostart() async => false;
  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('the queue card shows, and "서버 사용 안 함" takes it away', (tester) async {
    final core = _Core();
    final model = MailboxModel(_Client(core), _Platform());
    addTearDown(model.dispose);
    await model.accountActive();
    await tester.pumpWidget(
      MaterialApp(theme: buildAppTheme(), home: HomeScreen(core: core)),
    );
    await tester.pump();
    core.using(true);
    await tester.pumpAndSettle();
    expect(find.text(_card), findsOneWidget);

    unawaited(
      tester
          .state<NavigatorState>(find.byType(Navigator))
          .push(
            MaterialPageRoute<void>(
              builder: (_) => MailboxSettingsScreen(model: model),
            ),
          ),
    );
    await tester.pumpAndSettle();
    await tester.ensureVisible(find.text('서버 사용 안 함'));
    await tester.tap(find.text('서버 사용 안 함'));
    await tester.pumpAndSettle();
    expect(model.queue?.mode, QueueMode.never);
    tester.state<NavigatorState>(find.byType(Navigator)).pop();
    await tester.pumpAndSettle();
    expect(find.text(_card), findsNothing);
    await tester.pumpWidget(const SizedBox());
    await core.stream.close();
  }, skip: !(Platform.isWindows || Platform.isAndroid));
}
