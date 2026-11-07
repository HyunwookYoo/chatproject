import 'dart:async';
import 'dart:typed_data';

import 'package:chat_app/account/account_model.dart';
import 'package:chat_app/account/screens/contact_preview_screen.dart';
import 'package:chat_app/mailbox/notifier.dart';
import 'package:chat_app/main.dart';
import 'package:chat_app/message/screens/conversation_screen.dart';
import 'package:chat_app/src/rust/api/account.dart' as a;
import 'package:chat_app/src/rust/api/chat.dart' as r;
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

import 'support/fake_mailbox.dart';
import 'support/fake_message_client.dart' as m;
import 'support/fake_network_client.dart';

Uint8List _bytes(int value) => Uint8List.fromList(List.filled(32, value));

r.NotifyItem _item(int conversation, int newMessages, int unread) =>
    r.NotifyItem(
      conversation: _bytes(conversation),
      peer: _bytes(conversation + 100),
      newMessages: newMessages,
      unread: BigInt.from(unread),
    );

/// 0x01010101: the first 4 conversation bytes as a big-endian int32.
const _id1 = 0x01010101;

void main() {
  late StreamController<r.ChatEvent> bus;
  late FakeMailboxPlatform platform;
  late Notifier notifier;
  var lifecycle = AppLifecycleState.resumed;
  final lookups = <int>[];
  List<int>? found;

  setUp(() {
    bus = StreamController<r.ChatEvent>.broadcast(sync: true);
    platform = FakeMailboxPlatform();
    lifecycle = AppLifecycleState.resumed;
    lookups.clear();
    found = null;
    notifier = Notifier(
      events: bus.stream,
      platform: platform,
      aliasOf: (peer) => peer.first == 101 ? 'Alice' : null,
      findPeer: (id) async {
        lookups.add(id);
        return found;
      },
      lifecycle: () => lifecycle,
    );
  });
  tearDown(() async {
    await notifier.dispose();
    await bus.close();
  });

  void notify(List<r.NotifyItem> items) =>
      bus.add(r.ChatEvent.notify(items: items));

  test('new messages notify once with the alias and the unread count', () {
    notify([_item(1, 3, 3)]);
    expect(platform.notifications, [
      (id: _id1, title: isolatedName('Alice'), body: '새 메시지 3개'),
    ]);
    expect(platform.notifications.single.title.runes.first, 0x2068);
    expect(platform.cancelled, isEmpty);
  });

  test('a conversation without a contact alias is titled 새 메시지', () {
    notify([_item(2, 1, 1)]);
    expect(platform.notifications.single.title, '새 메시지');
    expect(platform.notifications.single.body, '새 메시지 1개');
  });

  test('unread 0 cancels the conversation notification', () {
    notify([_item(1, 3, 3)]);
    notify([_item(1, 0, 0)]);
    expect(platform.cancelled, [_id1]);
    expect(platform.notifications, hasLength(1));
  });

  test('a lower unread count updates a shown notification and posts no new '
      'one', () {
    notify([_item(1, 0, 2)]);
    expect(
      platform.notifications,
      isEmpty,
      reason: 'read elsewhere, none shown',
    );
    notify([_item(1, 3, 3)]);
    notify([_item(1, 0, 1)]);
    expect(platform.notifications.map((n) => n.body), ['새 메시지 3개', '새 메시지 1개']);
  });

  test(
    'the conversation on screen is not notified while the app is resumed',
    () {
      notifier.openPeer = _bytes(101);
      notify([_item(1, 2, 2)]);
      expect(platform.notifications, isEmpty);
      lifecycle = AppLifecycleState.hidden;
      notify([_item(1, 1, 3)]);
      expect(platform.notifications.single.body, '새 메시지 3개');
    },
  );

  test('two conversations in one event notify twice', () {
    notify([_item(1, 1, 1), _item(2, 4, 5)]);
    expect(platform.notifications.map((n) => (n.id, n.body)), [
      (_id1, '새 메시지 1개'),
      (0x02020202, '새 메시지 5개'),
    ]);
  });

  test('the id is the signed big-endian int32 of the conversation prefix', () {
    expect(notificationId([0xff, 0xff, 0xff, 0xfe, 9, 9]), -2);
    expect(notificationId([0x7f, 0xff, 0xff, 0xff]), 0x7fffffff);
    expect(notificationId([0, 0, 1, 0]), 256);
  });

  test('a tapped notification opens its conversation; unknown ids only look '
      'up and are ignored', () async {
    final opened = <List<int>>[];
    notifier.onOpen = opened.add;
    notify([_item(1, 1, 1)]);
    await notifier.open(_id1);
    expect(opened, [_bytes(101)]);
    expect(lookups, isEmpty);

    await notifier.open(77);
    expect(lookups, [77]);
    expect(opened, hasLength(1));

    found = _bytes(105);
    await notifier.open(0x05050505);
    expect(opened.last, _bytes(105));
  });

  testWidgets('the app notifies, opens a tapped conversation it looks up, '
      'stays quiet for it while open, and hands off before quitting', (
    tester,
  ) async {
    final core = FakeNetworkClient();
    final messages = m.FakeMessageClient()
      ..conversations = [m.conversation(1), m.conversation(5, revision: 2)];
    final accounts = AccountModel(
      ActiveAccountClient([
        a.ContactView(
          peerId: _bytes(2),
          alias: 'Alice',
          verified: false,
          newDevice: false,
          deviceUnverified: false,
        ),
      ]),
    );
    await accounts.open();
    final mailbox = FakeMailboxClient()..firstLinkShown = true;
    final channel = FakeMailboxPlatform();
    await tester.pumpWidget(
      ChatApp(
        core: core,
        network: core,
        account: accounts,
        messages: messages,
        mailbox: mailbox,
        mailboxPlatform: channel,
      ),
    );
    await tester.pumpAndSettle();
    tester.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
    r.NotifyItem item(int conversation, int unread) => r.NotifyItem(
      conversation: _bytes(conversation),
      peer: _bytes(conversation + 1),
      newMessages: 1,
      unread: BigInt.from(unread),
    );

    messages.controller.add(r.ChatEvent.notify(items: [item(1, 1)]));
    expect(channel.notifications.single.title, isolatedName('Alice'));

    channel.onOpenConversation!(0x0a0b0c0d);
    await tester.pumpAndSettle();
    expect(find.byType(ConversationScreen), findsNothing);

    channel.onOpenConversation!(0x05050505);
    await tester.pumpAndSettle();
    expect(
      tester.widget<ConversationScreen>(find.byType(ConversationScreen)).peer,
      _bytes(6),
    );
    messages.controller.add(r.ChatEvent.notify(items: [item(5, 2)]));
    expect(channel.notifications, hasLength(1));

    await channel.onExitRequested!();
    expect(mailbox.backgrounds, 1);
    expect(channel.calls.last, 'quit');

    await tester.pumpWidget(const SizedBox());
    accounts.dispose();
    await core.controller.close();
  });
}
