// Correspondence tests intentionally access generated codecs and the actual API.
// ignore_for_file: invalid_use_of_internal_member, invalid_use_of_protected_member

import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:isolate';
import 'package:chat_app/account/account_model.dart';
import 'package:chat_app/core_client.dart';
import 'package:chat_app/message/message_model.dart';
import 'package:chat_app/message/screens/conversation_list_screen.dart';
import 'package:chat_app/message/screens/conversation_screen.dart';
import 'package:chat_app/theme/app_theme.dart';
import 'package:chat_app/src/rust/api/account.dart' as account;
import 'package:chat_app/src/rust/api/chat.dart' as chat;
import 'package:chat_app/message/message_bridge.dart' as message;
import 'package:chat_app/src/rust/frb_generated.dart';
import 'package:flutter_rust_bridge/flutter_rust_bridge_for_generated.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:flutter/material.dart';

Uint8List bytes(int length, [int value = 255]) =>
    Uint8List.fromList(List.filled(length, value));
Matcher code(chat.ErrorCode code) =>
    isA<chat.CoreError>().having((error) => error.code, 'code', code);
void sameKey(chat.MessageKey a, chat.MessageKey b) {
  expect(a.conversation, orderedEquals(b.conversation));
  expect(a.sender, orderedEquals(b.sender));
  expect(a.installation, orderedEquals(b.installation));
  expect(a.id, orderedEquals(b.id));
}

// Captures the facade's actual handoff objects, before any native serializer can
// copy them. Native forwarding and codecs are tested separately against the DLL.
class _CaptureApi implements RustLibApi {
  final calls = <Symbol, Map<Symbol, dynamic>>{};
  final message.MessageView view;
  _CaptureApi(this.view);
  @override
  dynamic noSuchMethod(Invocation invocation) {
    calls[invocation.memberName] = invocation.namedArguments;
    return switch (invocation.memberName) {
      #crateApiMessageOpenDm => Future.value(
        message.ConversationView(
          id: bytes(32),
          peer: bytes(32),
          unread: BigInt.zero,
          changeRevision: BigInt.zero,
        ),
      ),
      #crateApiMessageListConversations => Future.value(
        message.ConversationPage(revision: BigInt.zero, items: []),
      ),
      #crateApiMessageLoadMessages => Future.value(
        message.MessagePage(revision: BigInt.zero, items: []),
      ),
      #crateApiMessageSendText ||
      #crateApiMessageCancelSend ||
      #crateApiMessageRetrySend => Future.value(view),
      #crateApiMessageMarkRead => Future<void>.value(),
      _ => super.noSuchMethod(invocation),
    };
  }
}

Future<Map<Symbol, Map<Symbol, dynamic>>> _captureOwnedInputs() async {
  final conversation = bytes(32, 1),
      sender = bytes(32, 2),
      installation = bytes(32, 3),
      id = bytes(16, 4);
  final key = chat.MessageKey(
    conversation: conversation,
    sender: sender,
    installation: installation,
    id: id,
  );
  final keys = [key];
  final api = _CaptureApi(
    message.MessageView(
      key: key,
      lamport: BigInt.one,
      sentAtMs: BigInt.one,
      body: 'x',
      outgoing: true,
      state: chat.DeliveryState.queued,
      cancelRequested: false,
    ),
  );
  RustLib.initMock(api: api);
  final pending = [
    message.openDm(peer: sender),
    message.sendText(peer: sender, id: id, body: 'x'),
    message.listConversations(
      before: message.ConversationCursor(
        changeRevision: BigInt.one,
        id: conversation,
      ),
      limit: 1,
    ),
    message.loadMessages(
      conversation: conversation,
      before: message.MessageCursor(
        lamport: BigInt.one,
        installation: installation,
        id: id,
      ),
      limit: 1,
    ),
    message.cancelSend(key: key),
    message.retrySend(key: key),
    message.markRead(conversation: conversation, keys: keys),
  ];
  for (final input in [conversation, sender, installation, id]) {
    input.fillRange(0, input.length, 0);
  }
  keys.clear();
  await Future.wait(pending);
  RustLib.dispose();
  return api.calls;
}

void main() {
  test('display DTO diagnostics do not render message bodies', () {
    const body = 'PUBLIC_TEST_BODY_DIAGNOSTIC_MARKER';
    final view = message.MessageView(
      key: chat.MessageKey(
        conversation: bytes(32),
        sender: bytes(32),
        installation: bytes(32),
        id: bytes(16),
      ),
      lamport: BigInt.one,
      sentAtMs: BigInt.one,
      body: body,
      outgoing: true,
      state: chat.DeliveryState.sentDirect,
      cancelRequested: false,
    );
    expect(view.toString().contains(body), isFalse);
    expect(
      message.MessagePage(
        revision: BigInt.one,
        items: [view],
      ).toString().contains(body),
      isFalse,
    );
  });

  test(
    'checked entry owns every mutable identifier before async handoff',
    () async {
      final calls = await Isolate.run(_captureOwnedInputs);
      expect(calls[#crateApiMessageOpenDm]![#peer], everyElement(2));
      expect(calls[#crateApiMessageSendText]![#peer], everyElement(2));
      expect(calls[#crateApiMessageSendText]![#id], everyElement(4));
      final conversations =
          calls[#crateApiMessageListConversations]![#before]
              as message.ConversationCursor;
      expect(conversations.id, everyElement(1));
      final load = calls[#crateApiMessageLoadMessages]!;
      expect(load[#conversation], everyElement(1));
      final cursor = load[#before] as message.MessageCursor;
      expect(cursor.installation, everyElement(3));
      expect(cursor.id, everyElement(4));
      final read = calls[#crateApiMessageMarkRead]!;
      expect(read[#conversation], everyElement(1));
      final readKeys = read[#keys] as List<chat.MessageKey>;
      expect(readKeys, hasLength(1));
      for (final owned in [
        calls[#crateApiMessageCancelSend]![#key] as chat.MessageKey,
        calls[#crateApiMessageRetrySend]![#key] as chat.MessageKey,
        readKeys.single,
      ]) {
        expect(owned.conversation, everyElement(1));
        expect(owned.sender, everyElement(2));
        expect(owned.installation, everyElement(3));
        expect(owned.id, everyElement(4));
      }
    },
  );

  // Required native gate sets an explicit current-source test-hooks DLL. Ordinary
  // app-only suites can run without native tooling; an invalid supplied path fails.
  final library = Platform.environment['CHAT_MESSAGE_BRIDGE_LIBRARY'];
  group('actual generated native bridge', skip: library == null, () {
    var initialized = false;
    late Directory directory;
    late account.AccountView owner;
    late RustLibApiImpl api;
    final peer = bytes(32);
    setUpAll(() async {
      final fixturePath = Platform.environment['CHAT_MESSAGE_BRIDGE_DATA_DIR'];
      if (fixturePath == null ||
          fixturePath.trim().isEmpty ||
          !Directory(fixturePath).isAbsolute) {
        throw StateError(
          'CHAT_MESSAGE_BRIDGE_DATA_DIR must be an explicit absolute runner-owned directory',
        );
      }
      directory = Directory(fixturePath);
      await RustLib.init(externalLibrary: ExternalLibrary.open(library!));
      initialized = true;
      api = RustLib.instance.api as RustLibApiImpl;
      await account.testStorageStart(dataDir: directory.path);
      await account.openAccount();
      final display = await account.beginCreate();
      await account.confirmCreate(
        flow: display.flow,
        answers: display.positions.map((p) => display.words[p - 1]).toList(),
      );
      owner = await account.snapshot();
    });
    tearDownAll(() async {
      if (initialized) RustLib.dispose();
      // Native owners hold the isolated database until process exit on Windows.
      // The runner removes its owned fixture after the test process exits.
    });

    test(
      'all identifier fields reject short and oversized without truncation',
      () async {
        for (final length in [0, 31, 33]) {
          await expectLater(
            message.openDm(peer: bytes(length)),
            throwsA(code(chat.ErrorCode.invalidMessage)),
          );
          await expectLater(
            message.loadMessages(conversation: bytes(length), limit: 50),
            throwsA(code(chat.ErrorCode.invalidMessage)),
          );
          await expectLater(
            message.markRead(conversation: bytes(length), keys: []),
            throwsA(code(chat.ErrorCode.invalidMessage)),
          );
        }
        for (final length in [0, 15, 17]) {
          await expectLater(
            message.sendText(peer: peer, id: bytes(length), body: 'x'),
            throwsA(code(chat.ErrorCode.invalidMessage)),
          );
        }
        for (final length in [31, 33]) {
          await expectLater(
            message.listConversations(
              before: message.ConversationCursor(
                changeRevision: BigInt.zero,
                id: bytes(length),
              ),
              limit: 50,
            ),
            throwsA(code(chat.ErrorCode.invalidMessage)),
          );
          await expectLater(
            message.loadMessages(
              conversation: peer,
              before: message.MessageCursor(
                lamport: BigInt.zero,
                installation: bytes(length),
                id: bytes(16),
              ),
              limit: 50,
            ),
            throwsA(code(chat.ErrorCode.invalidMessage)),
          );
        }
        for (final length in [15, 17]) {
          await expectLater(
            message.loadMessages(
              conversation: peer,
              before: message.MessageCursor(
                lamport: BigInt.zero,
                installation: peer,
                id: bytes(length),
              ),
              limit: 50,
            ),
            throwsA(code(chat.ErrorCode.invalidMessage)),
          );
        }
        final fields = [bytes(32), bytes(32), bytes(32), bytes(16)];
        for (var field = 0; field < 4; field++) {
          for (final delta in [-1, 1]) {
            final bad = fields.map(Uint8List.fromList).toList();
            bad[field] = bytes(fields[field].length + delta);
            final key = chat.MessageKey(
              conversation: bad[0],
              sender: bad[1],
              installation: bad[2],
              id: bad[3],
            );
            await expectLater(
              message.cancelSend(key: key),
              throwsA(code(chat.ErrorCode.invalidMessage)),
            );
            await expectLater(
              message.retrySend(key: key),
              throwsA(code(chat.ErrorCode.invalidMessage)),
            );
            await expectLater(
              message.markRead(conversation: fields[0], keys: [key]),
              throwsA(code(chat.ErrorCode.invalidMessage)),
            );
          }
        }
      },
    );

    test(
      'page and cursor bounds are checked before narrowing integers',
      () async {
        final maximum = BigInt.parse('9223372036854775807');
        for (final limit in [0, 51, 4294967295]) {
          await expectLater(
            message.listConversations(limit: limit),
            throwsA(code(chat.ErrorCode.invalidMessage)),
          );
        }
        for (final value in [
          maximum + BigInt.one,
          BigInt.parse('18446744073709551615'),
        ]) {
          await expectLater(
            message.listConversations(
              before: message.ConversationCursor(
                changeRevision: value,
                id: peer,
              ),
              limit: 50,
            ),
            throwsA(code(chat.ErrorCode.invalidMessage)),
          );
          await expectLater(
            message.loadMessages(
              conversation: peer,
              before: message.MessageCursor(
                lamport: value,
                installation: peer,
                id: bytes(16),
              ),
              limit: 50,
            ),
            throwsA(code(chat.ErrorCode.invalidMessage)),
          );
        }
        final page = await message.loadMessages(
          conversation: peer,
          before: message.MessageCursor(
            lamport: maximum,
            installation: peer,
            id: bytes(16),
          ),
          limit: 50,
        );
        expect(page.revision >= BigInt.zero, isTrue);
        await expectLater(
          message.markRead(
            conversation: peer,
            keys: List.filled(
              51,
              chat.MessageKey(
                conversation: peer,
                sender: peer,
                installation: peer,
                id: bytes(16),
              ),
            ),
          ),
          throwsA(code(chat.ErrorCode.invalidMessage)),
        );
      },
    );

    test(
      'Dart page integers cannot wrap to an accepted native limit',
      () async {
        for (final limit in [-1, -4294967295, 4294967297]) {
          await expectLater(
            message.listConversations(limit: limit),
            throwsA(code(chat.ErrorCode.invalidMessage)),
          );
          await expectLater(
            message.loadMessages(conversation: peer, limit: limit),
            throwsA(code(chat.ErrorCode.invalidMessage)),
          );
        }
      },
    );
    test(
      'Dart BigInt cursor cannot wrap to an accepted native integer',
      () async {
        for (final value in [
          -BigInt.one,
          -(BigInt.one << 64),
          BigInt.one << 64,
          (BigInt.one << 64) + BigInt.one,
          BigInt.one << 128,
        ]) {
          await expectLater(
            message.loadMessages(
              conversation: peer,
              before: message.MessageCursor(
                lamport: value,
                installation: peer,
                id: bytes(16),
              ),
              limit: 50,
            ),
            throwsA(code(chat.ErrorCode.invalidMessage)),
          );
          await expectLater(
            message.listConversations(
              before: message.ConversationCursor(
                changeRevision: value,
                id: peer,
              ),
              limit: 50,
            ),
            throwsA(code(chat.ErrorCode.invalidMessage)),
          );
        }
      },
    );
    test('Dart loose byte input cannot wrap to another identifier', () async {
      for (final value in [-1, 256, 511]) {
        await expectLater(
          message.openDm(peer: List.filled(32, value)),
          throwsA(code(chat.ErrorCode.invalidMessage)),
        );
        await expectLater(
          message.sendText(peer: peer, id: List.filled(16, value), body: 'x'),
          throwsA(code(chat.ErrorCode.invalidMessage)),
        );
        await expectLater(
          message.loadMessages(conversation: List.filled(32, value), limit: 50),
          throwsA(code(chat.ErrorCode.invalidMessage)),
        );
        await expectLater(
          message.markRead(conversation: List.filled(32, value), keys: []),
          throwsA(code(chat.ErrorCode.invalidMessage)),
        );
      }
    });

    test(
      'UTF8 byte boundary is exact and body survives durable roundtrip',
      () async {
        final body = '${List.filled(5461, '한').join()}a';
        expect(utf8.encode(body).length, 16384);
        final sent = await message.sendText(
          peer: peer,
          id: bytes(16),
          body: body,
        );
        expect(sent.body == body, isTrue);
        expect(sent.key.id, orderedEquals(bytes(16)));
        expect(sent.key.sender, orderedEquals(owner.publicPeerId!));
        expect(sent.key.installation, orderedEquals(owner.installation!));
        await expectLater(
          message.sendText(peer: peer, id: bytes(16, 1), body: '${body}a'),
          throwsA(code(chat.ErrorCode.messageTooLarge)),
        );
        await expectLater(
          message.sendText(peer: peer, id: bytes(16, 2), body: ''),
          throwsA(code(chat.ErrorCode.invalidMessage)),
        );
        await expectLater(
          message.sendText(peer: peer, id: bytes(16), body: 'different'),
          throwsA(code(chat.ErrorCode.messageConflict)),
        );
        final again = await message.sendText(
          peer: peer,
          id: bytes(16),
          body: body,
        );
        sameKey(sent.key, again.key);
        final page = await message.loadMessages(
          conversation: sent.key.conversation,
          limit: 50,
        );
        final loaded = page.items.singleWhere(
          (m) => m.key.id.every((b) => b == 255),
        );
        sameKey(sent.key, loaded.key);
        expect(loaded.body == body, isTrue);
        expect(loaded.state, isNot(chat.DeliveryState.delivered));
        expect(loaded.state, isNot(chat.DeliveryState.read));
        await chat.networkStop();
        final afterTransport = await message.loadMessages(
          conversation: sent.key.conversation,
          limit: 50,
        );
        expect(
          afterTransport.items.single.state,
          isNot(chat.DeliveryState.delivered),
        );
        expect(
          afterTransport.items.single.state,
          isNot(chat.DeliveryState.read),
        );
        final cancelled = await message.cancelSend(key: loaded.key);
        expect(cancelled.state, chat.DeliveryState.cancelled);
        await expectLater(
          message.retrySend(key: loaded.key),
          throwsA(code(chat.ErrorCode.accountRestricted)),
        );
      },
    );

    test(
      'owning account conversation, pagination and read stay authoritative',
      () async {
        final peer = bytes(32, 253);
        final opened = await message.openDm(peer: peer);
        expect(opened.peer, orderedEquals(peer));
        final previous = await message.sendText(
          peer: peer,
          id: bytes(16),
          body: 'older',
        );
        await message.cancelSend(key: previous.key);
        final sent = await message.sendText(
          peer: peer,
          id: bytes(16, 8),
          body: 'second',
        );
        sameKey(
          sent.key,
          (await message.loadMessages(
            conversation: opened.id,
            limit: 1,
          )).items.single.key,
        );
        final page = await message.loadMessages(
          conversation: opened.id,
          limit: 1,
        );
        expect(page.next, isNotNull);
        final older = await message.loadMessages(
          conversation: opened.id,
          before: page.next,
          limit: 50,
        );
        expect(older.items.any((m) => m.key.id.every((b) => b == 255)), isTrue);
        // Stop retained work before asserting deterministic recency pagination;
        // scheduler state changes legitimately advance conversation revisions.
        await message.cancelSend(key: sent.key);
        final second = await message.openDm(peer: bytes(32, 254));
        final conversations = await message.listConversations(limit: 1);
        expect(conversations.items.single.id, orderedEquals(second.id));
        expect(conversations.items.single.last, isNull);
        expect(conversations.next, isNotNull);
        final next = await message.listConversations(
          before: conversations.next,
          limit: 1,
        );
        expect(next.items.single.id, orderedEquals(opened.id));
        expect(next.items.single.last, isNotNull);
        await message.markRead(conversation: opened.id, keys: []);
        await expectLater(
          message.markRead(conversation: opened.id, keys: [sent.key]),
          throwsA(code(chat.ErrorCode.invalidMessage)),
        );
        await expectLater(
          message.openDm(peer: owner.publicPeerId!),
          throwsA(code(chat.ErrorCode.selfConnection)),
        );
        expect(chat.coreVersion(), isNotEmpty);
      },
    );

    test(
      'generated codecs keep all enums, reject unknowns and preserve cursors',
      () {
        SseDeserializer value(int value) =>
            SseDeserializer(ByteData(4)..setInt32(0, value, Endian.host));
        for (var i = 0; i < chat.ErrorCode.values.length; i++) {
          expect(api.sse_decode_error_code(value(i)), chat.ErrorCode.values[i]);
        }
        for (var i = 0; i < chat.DeliveryState.values.length; i++) {
          expect(
            api.sse_decode_delivery_state(value(i)),
            chat.DeliveryState.values[i],
          );
        }
        for (var i = 0; i < message.BlockReason.values.length; i++) {
          expect(
            api.sse_decode_block_reason(value(i)),
            message.BlockReason.values[i],
          );
        }
        expect(() => api.sse_decode_delivery_state(value(7)), throwsRangeError);
        expect(() => api.sse_decode_block_reason(value(3)), throwsRangeError);
        expect(
          () => api.sse_decode_error_code(value(chat.ErrorCode.values.length)),
          throwsRangeError,
        );
        expect(
          () => api.sse_decode_delivery_state(value(-1)),
          throwsRangeError,
        );
      },
    );

    testWidgets(
      'native bounded room and list controls reach retained history',
      (t) async {
        final core = FrbCoreClient(dataDir: directory.path);
        final accounts = AccountModel(core.account);
        final historyPeer = bytes(32, 200);
        final expectedConversations = <String>{};
        await t.runAsync(() async {
          for (var id = 1; id <= 225; id++) {
            final saved = await message.sendText(
              peer: historyPeer,
              id: bytes(16, id),
              body: 'Native history $id',
            );
            await message.cancelSend(key: saved.key);
          }
          for (var id = 0; id < 225; id++) {
            final opened = await message.openDm(peer: bytes(32, id));
            expectedConversations.add(opened.id.join(','));
          }
          await accounts.open();
        });
        final room = MessageModel(core.messages, account: accounts);
        final list = MessageModel(core.messages, account: accounts);
        Future<void> settleNative(MessageModel model) async {
          await t.pump();
          final elapsed = Stopwatch()..start();
          while (model.busy) {
            if (elapsed.elapsed > const Duration(seconds: 10)) {
              throw TimeoutException('native window did not settle');
            }
            await t.runAsync(
              () => Future<void>.delayed(const Duration(milliseconds: 10)),
            );
            await t.pump();
          }
          await t.pumpAndSettle();
          expect(model.errorCode, isNull);
        }

        Widget app(Widget child) =>
            MaterialApp(theme: buildAppTheme(), home: child);
        t.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
        try {
          await t.pumpWidget(
            app(ConversationScreen(model: room, peer: historyPeer)),
          );
          await settleNative(room);
          final viewport = find.descendant(
            of: find.byKey(const ValueKey('message-viewport')),
            matching: find.byType(Scrollable),
          );
          final seenMessages = <int>{};
          for (var page = 0; page < 5; page++) {
            expect(room.messages.length, lessThanOrEqualTo(150));
            seenMessages.addAll(room.messages.map((row) => row.key.id.first));
            if (page == 3) {
              await t.runAsync(() async {
                final saved = await message.sendText(
                  peer: historyPeer,
                  id: bytes(16, 226),
                  body: 'Native history 226',
                );
                await message.cancelSend(key: saved.key);
              });
              unawaited(room.reload());
              await settleNative(room);
              expect(room.messages.first.key.id.first, 175);
            }
            if (page < 4) {
              expect(room.hasOlder, true);
              await t.scrollUntilVisible(
                find.text('이전 메시지 더 보기'),
                1600,
                scrollable: viewport,
                maxScrolls: 100,
              );
              await t.tap(find.text('이전 메시지 더 보기'));
              await settleNative(room);
            }
          }
          expect(seenMessages, containsAll(List.generate(225, (i) => i + 1)));
          await t.scrollUntilVisible(
            find.text('Native history 1'),
            1600,
            scrollable: viewport,
            maxScrolls: 100,
          );
          expect(find.text('Native history 1').hitTestable(), findsOneWidget);
          await t.tap(find.text('최신 메시지로 이동'));
          await settleNative(room);
          expect(room.messages.length, 50);
          expect(find.text('Native history 226').hitTestable(), findsOneWidget);
          List<int>? openedPeer;
          await t.pumpWidget(
            app(
              Scaffold(
                body: ConversationListScreen(
                  model: list,
                  onOpen: (peer) => openedPeer = peer,
                ),
              ),
            ),
          );
          await settleNative(list);
          final seenConversations = <String>{};
          for (var page = 0; page < 6; page++) {
            expect(list.conversations.length, lessThanOrEqualTo(150));
            seenConversations.addAll(
              list.conversations.map((row) => row.id.join(',')),
            );
            if (!list.hasOlder) break;
            await t.scrollUntilVisible(
              find.text('이전 대화 더 보기'),
              2000,
              maxScrolls: 100,
            );
            await t.tap(find.text('이전 대화 더 보기'));
            await settleNative(list);
          }
          expect(list.hasOlder, false);
          expect(seenConversations, containsAll(expectedConversations));
          final oldest = list.conversations.last;
          final tile = find.byKey(ValueKey(oldest.id.join(',')));
          await t.scrollUntilVisible(tile, 2000, maxScrolls: 100);
          await t.tap(tile);
          expect(openedPeer, oldest.peer);
          await t.runAsync(() async {
            await message.openDm(peer: bytes(32, 226));
          });
          unawaited(list.reload());
          await settleNative(list);
          await t.tap(find.text('최신 대화로 이동'));
          await settleNative(list);
          expect(list.conversations.length, 50);
          expect(list.conversations.first.peer, bytes(32, 226));
          final latest = find.byKey(
            ValueKey(list.conversations.first.id.join(',')),
          );
          expect(latest.hitTestable(), findsOneWidget);
          await t.tap(latest);
          expect(openedPeer, bytes(32, 226));
          expect(t.takeException(), isNull);
        } finally {
          await t.pumpWidget(const SizedBox());
          room.dispose();
          list.dispose();
          accounts.dispose();
        }
      },
      timeout: const Timeout(Duration(minutes: 2)),
    );


  });
}
