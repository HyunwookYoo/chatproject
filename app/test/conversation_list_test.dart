import 'dart:async';
import 'package:chat_app/message/message_bridge.dart';
import 'package:chat_app/message/message_model.dart';
import 'package:chat_app/account/account_model.dart';
import 'package:chat_app/main.dart';
import 'package:chat_app/message/screens/conversation_screen.dart';
import 'package:chat_app/src/rust/api/account.dart' as account;
import 'package:chat_app/message/screens/conversation_list_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'support/conversation_fixture.dart';
import 'support/fake_message_client.dart';
import 'support/fake_network_client.dart';
import 'account_onboarding_test.dart' show ScreenClient;

void main() {
  late ConversationFixture f;
  setUp(() async {
    f = ConversationFixture();
  });
  tearDown(() async => f.close());
  testWidgets('replacing a borrowed list projection reloads the new owner', (
    t,
  ) async {
    await f.open();
    await pumpConversation(
      t,
      Scaffold(
        body: ConversationListScreen(model: f.model, onOpen: (_) {}),
      ),
    );
    final replacement = MessageModel(f.client, account: f.accounts);
    await pumpConversation(
      t,
      Scaffold(
        body: ConversationListScreen(model: replacement, onOpen: (_) {}),
      ),
    );
    expect(find.text('fixture'), findsOneWidget);
    await t.pumpWidget(const SizedBox());
    replacement.dispose();
  });
  testWidgets(
    'contact opens offline DM and room disposal leaves shared account and list paging usable',
    (t) async {
      await f.open();
      final core = FakeNetworkClient();
      final client = ScreenClient()
        ..phase = account.AccountPhase.localReady
        ..saved = [
          account.ContactView(
            peerId: bytes(2),
            alias: 'Public contact',
            verified: false,
            newDevice: false,
            deviceUnverified: false,
          ),
        ];
      final accounts = AccountModel(client);
      await accounts.open();
      f.client.conversations = List.generate(
        60,
        (i) => conversation(i + 1, revision: i + 1),
      );
      await t.pumpWidget(
        ChatApp(
          core: core,
          network: core,
          account: accounts,
          messages: f.client,
        ),
      );
      await t.pumpAndSettle();
      await t.tap(find.text('\u2068Public contact\u2069'));
      await t.pumpAndSettle();
      await t.tap(find.text('대화 시작'));
      await t.pumpAndSettle();
      final room = t.widget<ConversationScreen>(
        find.byType(ConversationScreen),
      );
      expect(identical(room.model.account, accounts), true);
      expect(identical(room.model.client, f.client), true);
      expect(room.model.ready, true);
      await t.pageBack();
      await t.pumpAndSettle();
      expect(room.model.ready, false);
      expect(accounts.snapshot!.phase, account.AccountPhase.localReady);
      await t.tap(find.byTooltip('취소').first);
      await t.pumpAndSettle();
      expect(find.text('내 기기'), findsOneWidget);
      await t.tap(find.text('대화 및 네트워크 진단'));
      await t.pumpAndSettle();
      expect(find.byTooltip('네트워크 측정'), findsOneWidget);
      await t.tap(find.byType(ListTile).first);
      await t.pumpAndSettle();
      await t.pageBack();
      await t.pumpAndSettle();
      await t.scrollUntilVisible(find.text('이전 대화 더 보기'), 1000);
      await t.tap(find.text('이전 대화 더 보기'));
      await t.pumpAndSettle();
      expect(find.text('이전 대화 더 보기'), findsNothing);
      await t.pumpWidget(const SizedBox());
      accounts.dispose();
      await client.controller.close();
      await core.controller.close();
    },
  );
  testWidgets(
    'list exposes durable last state exact unread and opens selected peer',
    (t) async {
      await f.open();
      List<int>? opened;
      f.client.conversations = [
        ConversationView(
          id: bytes(1),
          peer: bytes(2),
          last: row(1, state: DeliveryState.sentDirect),
          unread: BigInt.parse('9007199254740993'),
          changeRevision: BigInt.one,
        ),
      ];
      await pumpConversation(
        t,
        Scaffold(
          body: ConversationListScreen(
            model: f.model,
            onOpen: (peer) => opened = peer,
          ),
        ),
        scale: 2,
      );
      expect(find.text('전송됨'), findsOneWidget);
      expect(find.text('전달됨'), findsNothing);
      expect(find.textContaining('9007199254740993'), findsOneWidget);
      await t.tap(find.text('Public test message'));
      await t.pump();
      expect(opened, bytes(2));
      expect(t.takeException(), isNull);
    },
  );
  testWidgets(
    'empty list directs contacts and bounded cursor paging loads older conversations',
    (t) async {
      await f.open();
      f.client.conversations = [];
      await pumpConversation(
        t,
        Scaffold(
          body: ConversationListScreen(model: f.model, onOpen: (_) {}),
        ),
      );
      expect(find.text('아직 대화가 없어요'), findsOneWidget);
      f.client.conversations = List.generate(
        60,
        (i) => conversation(i + 1, revision: i + 1),
      );
      unawaited(f.model.reload());
      await t.pumpAndSettle();
      expect(f.model.conversations.length, 50);
      await t.scrollUntilVisible(find.text('이전 대화 더 보기'), 1000);
      await t.tap(find.text('이전 대화 더 보기'));
      await t.pumpAndSettle();
      expect(f.model.conversations.length, 60);
      f.lock();
      await t.pumpAndSettle();
      expect(find.text('fixture'), findsNothing);
    },
  );
}
