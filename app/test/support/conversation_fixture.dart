import 'package:chat_app/account/account_model.dart';
import 'package:chat_app/message/message_bridge.dart';
import 'package:chat_app/message/message_model.dart';
import 'package:chat_app/src/rust/api/account.dart' as account;
import 'package:chat_app/src/rust/api/chat.dart' as rust;
import 'package:chat_app/theme/app_theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import '../account_model_test.dart' as fixture;
import 'fake_message_client.dart';

class ReadClient extends FakeMessageClient {
  bool failRead = false;
  bool emitReadEvent = false;
  Future<void>? readWait;
  VoidCallback? onRead;
  @override
  Future<void> markRead({
    required List<int> conversation,
    required List<MessageKey> keys,
  }) async {
    reads.add(keys);
    onRead?.call();
    await readWait;
    if (failRead) {
      throw const rust.CoreError(
        code: rust.ErrorCode.storageFull,
        detail: 'private',
      );
    }
    if (emitReadEvent) changed(revision.toInt() + 1);
  }
}

class ConversationFixture {
  final accountClient = fixture.FakeAccountClient();
  final client = ReadClient();
  late final AccountModel accounts;
  late final MessageModel model;
  Future<void> open() async {
    accounts = AccountModel(accountClient);
    await accounts.open();
    accounts.applySnapshot(
      fixture.view(BigInt.two, phase: account.AccountPhase.localReady),
    );
    model = MessageModel(client, account: accounts);
  }

  Future<void> close() async {
    model.dispose();
    accounts.dispose();
    await client.controller.close();
    await accountClient.controller.close();
  }

  void lock() => accounts.applySnapshot(
    fixture.view(BigInt.from(3), phase: account.AccountPhase.locked),
  );
}

MessageView row(
  int id, {
  String body = 'Public test message',
  DeliveryState state = DeliveryState.queued,
  bool outgoing = true,
  bool cancelled = false,
  BlockReason? block,
  BigInt? time,
}) => MessageView(
  key: message(id).key,
  lamport: BigInt.from(id),
  sentAtMs: time ?? BigInt.from(1700000000000),
  body: body,
  outgoing: outgoing,
  state: state,
  cancelRequested: cancelled,
  block: block,
);

Widget testApp(Widget child, {double scale = 1}) => MaterialApp(
  theme: buildAppTheme(),
  builder: (context, child) => MediaQuery(
    data: MediaQuery.of(context).copyWith(textScaler: TextScaler.linear(scale)),
    child: child!,
  ),
  home: child,
);

Future<void> pumpConversation(
  WidgetTester tester,
  Widget screen, {
  double scale = 1,
}) async {
  tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
  await tester.pumpWidget(testApp(screen, scale: scale));
  await tester.pumpAndSettle();
}
