import 'dart:async';
import 'dart:io';
import 'dart:ui' show ViewFocusEvent, ViewFocusState, ViewFocusDirection;
import 'dart:ui' as ui;

import 'package:chat_app/message/message_bridge.dart';
import 'package:chat_app/message/message_model.dart';
import 'package:chat_app/message/screens/conversation_list_screen.dart';
import 'package:chat_app/message/screens/conversation_screen.dart';
import 'package:chat_app/src/rust/api/chat.dart' as rust;
import 'package:flutter/material.dart';
import 'package:flutter/rendering.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'support/conversation_fixture.dart';
import 'support/fake_message_client.dart';

void main() {
  late ConversationFixture f;
  setUp(() async {
    f = ConversationFixture();
    f.client.rows = [];
  });
  tearDown(() async => f.close());
  Future<void> show(WidgetTester t, {double scale = 1}) => pumpConversation(
    t,
    ConversationScreen(model: f.model, peer: bytes(2)),
    scale: scale,
  );

  const visualDir = String.fromEnvironment('MESSAGE_VISUAL_DIR');
  const visualFont = String.fromEnvironment('MESSAGE_VISUAL_FONT');
  testWidgets(
    'public fixture rendered visual artifacts',
    (t) async {
      await f.open();
      // Optional owned artifacts only. A local Korean font makes glyph/layout
      // critique useful in the widget renderer; this is not native font evidence.
      await t.runAsync(() async {
        final loader = FontLoader('Roboto')
          ..addFont(File(visualFont).readAsBytes().then(ByteData.sublistView));
        await loader.load();
      });
      t.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
      addTearDown(t.view.resetPhysicalSize);
      addTearDown(t.view.resetDevicePixelRatio);
      for (final scenario in [
        (1.0, const Size(390, 844), 'room-mobile'),
        (2.0, const Size(390, 844), 'room-large-text'),
        (1.0, const Size(1000, 760), 'room-desktop'),
      ]) {
        f.client.rows = [
          row(1, body: '공개 화면 검증 메시지입니다.', outgoing: false),
          row(2, body: '전송과 전달은 다른 상태예요.', state: DeliveryState.sentDirect),
          row(
            3,
            body: '연락처 등록 후 다시 시도할 수 있어요.',
            state: DeliveryState.blocked,
            block: BlockReason.contactRequired,
          ),
        ];
        t.view.physicalSize = scenario.$2;
        t.view.devicePixelRatio = 1;
        final capture = GlobalKey();
        await t.pumpWidget(
          RepaintBoundary(
            key: capture,
            child: testApp(
              ConversationScreen(model: f.model, peer: bytes(2)),
              scale: scenario.$1,
            ),
          ),
        );
        await t.pumpAndSettle();
        expect(t.takeException(), isNull);
        await t.runAsync(() async {
          final image =
              await (capture.currentContext!.findRenderObject()!
                      as RenderRepaintBoundary)
                  .toImage();
          final png = await image.toByteData(format: ui.ImageByteFormat.png);
          await File(
            '$visualDir/${scenario.$3}.png',
          ).writeAsBytes(png!.buffer.asUint8List());
          image.dispose();
        });
        await t.pumpWidget(const SizedBox());
      }
      final listModel = MessageModel(f.client, account: f.accounts);
      f.client.conversations = [
        ConversationView(
          id: bytes(1),
          peer: bytes(2),
          last: row(1, body: '공개 대화 목록 검증', state: DeliveryState.sentDirect),
          unread: BigInt.from(3),
          changeRevision: BigInt.one,
        ),
        ConversationView(
          id: bytes(2),
          peer: bytes(3),
          last: row(
            2,
            body: '상대 저장 확인이 도착했어요.',
            state: DeliveryState.delivered,
          ),
          unread: BigInt.zero,
          changeRevision: BigInt.two,
        ),
      ];
      t.view.physicalSize = const Size(390, 844);
      final capture = GlobalKey();
      await t.pumpWidget(
        RepaintBoundary(
          key: capture,
          child: testApp(
            Scaffold(
              appBar: AppBar(title: const Text('대화')),
              body: ConversationListScreen(model: listModel, onOpen: (_) {}),
            ),
          ),
        ),
      );
      await t.pumpAndSettle();
      expect(t.takeException(), isNull);
      await t.runAsync(() async {
        final image =
            await (capture.currentContext!.findRenderObject()!
                    as RenderRepaintBoundary)
                .toImage();
        final png = await image.toByteData(format: ui.ImageByteFormat.png);
        await File(
          '$visualDir/list-mobile.png',
        ).writeAsBytes(png!.buffer.asUint8List());
        image.dispose();
      });
      await t.pumpWidget(const SizedBox());
      listModel.dispose();
    },
    skip: visualDir.isEmpty || visualFont.isEmpty,
  );

  testWidgets(
    'visibility is rechecked between read batches when a large viewport loses foreground',
    (t) async {
      await f.open();
      t.view.physicalSize = const Size(1000, 30000);
      t.view.devicePixelRatio = 1;
      addTearDown(t.view.resetPhysicalSize);
      addTearDown(t.view.resetDevicePixelRatio);
      final held = Completer<void>();
      f.client.readWait = held.future;
      f.client.rows = List.generate(150, (i) => row(i + 1, outgoing: false));
      await show(t);
      expect(f.client.reads.length, 1);
      unawaited(f.model.loadOlder());
      await t.pumpAndSettle();
      unawaited(f.model.loadOlder());
      await t.pumpAndSettle();
      f.client.onRead = () =>
          t.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
      f.client.readWait = null;
      held.complete();
      await t.pumpAndSettle();
      expect(f.client.reads.length, 2);
      expect(f.client.reads.every((batch) => batch.length <= 50), true);
    },
  );

  testWidgets(
    'read failure remains retryable when concurrent refresh clears model error',
    (t) async {
      await f.open();
      final reading = Completer<void>();
      f.client.readWait = reading.future;
      f.client.rows = [row(1, outgoing: false)];
      await show(t);
      final page = Completer<MessagePage>();
      f.client.query = (_) => page.future;
      unawaited(f.model.reload());
      await t.pump();
      void completeRefresh() {
        if (f.model.errorCode != null && !page.isCompleted) {
          page.complete(
            MessagePage(revision: BigInt.one, items: f.client.rows),
          );
        }
      }

      f.model.addListener(completeRefresh);
      f.client.failRead = true;
      reading.complete();
      await t.pumpAndSettle();
      f.model.removeListener(completeRefresh);
      expect(f.model.errorCode, isNull);
      expect(find.text('읽음 반영 다시 시도'), findsOneWidget);
    },
  );

  testWidgets(
    'unrelated send error cannot turn a successful viewport read into read failure',
    (t) async {
      await f.open();
      f.client.rows = List.generate(
        80,
        (i) => row(
          i + 1,
          outgoing: false,
          body: 'Public viewport message\nSecond line\nThird line',
        ),
      );
      await show(t);
      final calls = f.client.reads.length;
      f.client.sending = (_, _) async => throw const rust.CoreError(
        code: rust.ErrorCode.messageTooLarge,
        detail: 'private',
      );
      await t.enterText(find.byType(TextField), 'Public rejected draft');
      await t.tap(find.text('보내기'));
      await t.pumpAndSettle();
      expect(f.model.errorCode, rust.ErrorCode.messageTooLarge);
      await t.drag(
        find.byKey(const ValueKey('message-viewport')),
        const Offset(0, 450),
      );
      await t.pumpAndSettle();
      expect(f.client.reads.length, greaterThan(calls));
      expect(find.text('읽음 반영 다시 시도'), findsNothing);
    },
  );

  testWidgets(
    'cancellation plus sentDirect does not imply a received receipt',
    (t) async {
      await f.open();
      f.client.rows = [
        row(1, state: DeliveryState.sentDirect, cancelled: true),
      ];
      await show(t);
      expect(find.text('전송됨'), findsOneWidget);
      expect(find.textContaining('수신 확인이 반영'), findsNothing);
      expect(find.textContaining('도착했을 수 있어요'), findsOneWidget);
    },
  );

  testWidgets(
    'view focus loss before frame excludes newly rendered incoming keys',
    (t) async {
      await f.open();
      f.client.rows = [row(1, outgoing: false)];
      await show(t);
      void focus(ViewFocusState state) => t.binding.handleViewFocusChanged(
        ViewFocusEvent(
          viewId: t.view.viewId,
          state: state,
          direction: ViewFocusDirection.undefined,
        ),
      );
      focus(ViewFocusState.unfocused);
      f.client.rows = [row(2, outgoing: false)];
      f.client.changed(2);
      await t.pumpAndSettle();
      expect(
        f.client.reads.expand((b) => b).any((k) => k.id.first == 2),
        false,
      );
      focus(ViewFocusState.focused);
      await t.pumpAndSettle();
      expect(f.client.reads.expand((b) => b).any((k) => k.id.first == 2), true);
    },
  );

  testWidgets(
    'late send and page after lock cannot restore private display or draft',
    (t) async {
      await f.open();
      f.client.rows = [row(1)];
      final send = Completer<MessageView>();
      f.client.sending = (_, _) => send.future;
      await show(t);
      await t.enterText(find.byType(TextField), 'Public pending draft');
      await t.tap(find.text('보내기'));
      await t.pump();
      final page = Completer<MessagePage>();
      f.client.query = (_) => page.future;
      unawaited(f.model.reload());
      await t.pump();
      f.lock();
      await t.pump();
      send.complete(row(2));
      page.complete(MessagePage(revision: BigInt.from(5), items: [row(2)]));
      await t.pumpAndSettle();
      expect(find.text('Public pending draft'), findsNothing);
      expect(find.text('Public test message'), findsNothing);
      expect(find.byType(TextField), findsNothing);
    },
  );

  testWidgets('changing room while send waits preserves the new room draft', (
    t,
  ) async {
    await f.open();
    final send = Completer<MessageView>();
    f.client.sending = (_, _) => send.future;
    await show(t);
    await t.enterText(find.byType(TextField), 'Public first draft');
    await t.tap(find.text('보내기'));
    await t.pump();
    f.client.rows = [];
    f.client.conversations.add(conversation(2));
    await pumpConversation(
      t,
      ConversationScreen(model: f.model, peer: bytes(3)),
    );
    await t.enterText(find.byType(TextField), 'Public second draft');
    send.complete(row(1));
    await t.pumpAndSettle();
    expect(
      t.widget<TextField>(find.byType(TextField)).controller!.text,
      'Public second draft',
    );
  });

  testWidgets(
    'saving is not queued and duplicate actions retain input until durable commit',
    (t) async {
      await f.open();
      final held = Completer<MessageView>();
      f.client.sending = (_, _) => held.future;
      await show(t);
      await t.enterText(find.byType(TextField), 'Public draft');
      await t.tap(find.text('보내기'));
      await t.pump();
      expect(find.text('저장 중'), findsOneWidget);
      expect(find.text('보내기 대기'), findsNothing);
      expect(
        t.widget<TextField>(find.byType(TextField)).controller!.text,
        'Public draft',
      );
      await t.tap(find.text('보내기'));
      await t.pump();
      expect(f.client.tokens.length, 1);
      f.client.rows = [row(1)];
      held.complete(row(1));
      await t.pumpAndSettle();
      expect(
        t.widget<TextField>(find.byType(TextField)).controller!.text,
        isEmpty,
      );
      expect(find.text('보내기 대기'), findsOneWidget);
    },
  );

  testWidgets(
    'quota error retains draft and action token without exposing detail',
    (t) async {
      await f.open();
      f.client.sending = (_, _) async => throw const rust.CoreError(
        code: rust.ErrorCode.storageFull,
        detail: 'private detail',
      );
      await show(t);
      await t.enterText(find.byType(TextField), 'Public draft');
      await t.tap(find.text('보내기'));
      await t.pumpAndSettle();
      expect(
        t.widget<TextField>(find.byType(TextField)).controller!.text,
        'Public draft',
      );
      expect(find.textContaining('기존 메시지는 지우지 않아요'), findsOneWidget);
      expect(find.textContaining('private detail'), findsNothing);
      await t.tap(find.text('보내기'));
      await t.pumpAndSettle();
      expect(f.client.tokens[0], f.client.tokens[1]);
    },
  );

  testWidgets(
    'edits during pending send survive success and lock drops private draft',
    (t) async {
      await f.open();
      final held = Completer<MessageView>();
      f.client.sending = (_, _) => held.future;
      await show(t);
      await t.enterText(find.byType(TextField), 'Public draft');
      await t.tap(find.text('보내기'));
      await t.pump();
      await t.enterText(find.byType(TextField), 'Public edited draft');
      held.complete(row(1));
      await t.pumpAndSettle();
      expect(
        t.widget<TextField>(find.byType(TextField)).controller!.text,
        'Public edited draft',
      );
      f.lock();
      await t.pumpAndSettle();
      expect(find.text('Public edited draft'), findsNothing);
      expect(find.byType(TextField), findsNothing);
    },
  );

  testWidgets(
    'durable delivery states and late read after cancellation stay distinct',
    (t) async {
      await f.open();
      for (final state in DeliveryState.values) {
        f.client.rows = [
          row(1, state: state, cancelled: state == DeliveryState.read),
        ];
        await show(t);

        unawaited(f.model.reload());

        await t.pumpAndSettle();
        final label = [
          '보내기 대기',
          '전송됨',
          '전달됨',
          '읽음',
          '전송 보류',
          '전송 실패',
          '전송 취소',
        ][state.index];
        expect(find.text(label), findsWidgets);
        if (state == DeliveryState.sentDirect) {
          expect(find.text('전달됨'), findsNothing);
        }
        if (state == DeliveryState.read) {
          expect(find.textContaining('재전송 중단됨'), findsOneWidget);
        }
      }
    },
  );

  testWidgets(
    'blocked mutual contact reason supports explicit retry and cancellation warns',
    (t) async {
      await f.open();
      f.client.rows = [
        row(
          1,
          state: DeliveryState.blocked,
          block: BlockReason.contactRequired,
        ),
      ];
      await show(t);
      expect(find.textContaining('상호 연락처'), findsOneWidget);
      await t.tap(find.text('다시 시도'));
      await t.pumpAndSettle();
      expect(f.client.retries.length, 1);
      await t.tap(find.text('전송 취소'));
      await t.pumpAndSettle();
      expect(find.text('이미 전송 중인 메시지는 상대에게 도착했을 수 있어요.'), findsOneWidget);
      await t.tap(find.text('전송 중단'));
      await t.pumpAndSettle();
      expect(f.client.cancellations.length, 1);
    },
  );

  testWidgets(
    'cancelled resend is a new action and never overwrites another draft',
    (t) async {
      await f.open();
      f.client.rows = [row(1, state: DeliveryState.cancelled, cancelled: true)];
      await show(t);
      await t.enterText(find.byType(TextField), 'Other public draft');
      await t.tap(find.text('새 메시지로 보내기'));
      await t.pumpAndSettle();
      expect(f.client.tokens.length, 1);
      expect(
        t.widget<TextField>(find.byType(TextField)).controller!.text,
        'Other public draft',
      );
    },
  );

  testWidgets(
    'read marks only intersecting incoming rows and cannot create a refresh loop',
    (t) async {
      await f.open();
      f.client.emitReadEvent = true;
      f.client.rows = List.generate(
        80,
        (i) => row(
          i + 1,
          outgoing: i == 79,
          body: 'Public message ${i + 1}\nSecond line\nThird line',
        ),
      );
      await show(t);

      final ids = f.client.reads
          .expand((b) => b)
          .map((k) => k.id.first)
          .toSet();
      expect(ids, isNotEmpty);
      expect(ids.length, lessThan(20));
      expect(ids, isNot(contains(80)));
      expect(ids, isNot(contains(1)));
      final calls = f.client.reads.length;
      for (var i = 0; i < 5; i++) {
        unawaited(f.model.reload());

        await t.pumpAndSettle();
      }
      expect(f.client.reads.length, calls);
      await t.drag(
        find.byKey(const ValueKey('message-viewport')),
        const Offset(0, 450),
      );
      await t.pumpAndSettle();
      expect(
        f.client.reads.expand((b) => b).map((k) => k.id.first).toSet().length,
        greaterThan(ids.length),
      );
    },
  );

  testWidgets(
    'covered route and inactive lifecycle do not mark new rows read',
    (t) async {
      await f.open();
      f.client.rows = [row(1, outgoing: false)];
      await show(t);
      final context = t.element(find.byType(ConversationScreen));
      Navigator.of(context).push(
        MaterialPageRoute<void>(
          builder: (_) => const Scaffold(body: Text('Public cover')),
        ),
      );
      await t.pumpAndSettle();
      f.client.rows = [row(2, outgoing: false)];
      f.client.changed(2);
      await t.pumpAndSettle();
      expect(
        f.client.reads.expand((b) => b).any((k) => k.id.first == 2),
        false,
      );
      Navigator.of(context).pop();
      await t.pumpAndSettle();
      expect(f.client.reads.expand((b) => b).any((k) => k.id.first == 2), true);
      t.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
      f.client.rows = [row(3, outgoing: false)];
      f.client.changed(3);
      await t.pumpAndSettle();
      expect(
        f.client.reads.expand((b) => b).any((k) => k.id.first == 3),
        false,
      );
      t.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
      await t.pumpAndSettle();
      expect(f.client.reads.expand((b) => b).any((k) => k.id.first == 3), true);
    },
  );

  testWidgets('failed visible read stops repeating and explicit retry rearms', (
    t,
  ) async {
    await f.open();
    f.client.failRead = true;
    f.client.rows = [row(1, outgoing: false)];
    await show(t);
    expect(f.client.reads.length, 1);

    unawaited(f.model.reload());

    await t.pumpAndSettle();
    expect(f.client.reads.length, 1);
    f.client.failRead = false;
    await t.tap(find.text('읽음 반영 다시 시도'));
    await t.pumpAndSettle();
    expect(f.client.reads.length, 2);
  });

  testWidgets('Enter stays multiline while Ctrl and Cmd Enter submit once', (
    t,
  ) async {
    await f.open();
    await show(t);
    for (final modifier in [
      LogicalKeyboardKey.controlLeft,
      LogicalKeyboardKey.metaLeft,
    ]) {
      await t.enterText(find.byType(TextField), 'Public\nmultiline');
      await t.sendKeyEvent(LogicalKeyboardKey.enter);
      await t.pump();
      expect(f.model.saving, false);
      await t.sendKeyDownEvent(modifier);
      await t.sendKeyEvent(LogicalKeyboardKey.enter);
      await t.sendKeyUpEvent(modifier);
      await t.pumpAndSettle();
    }
    expect(f.client.tokens.length, 2);
    expect(
      t.widget<TextField>(find.byType(TextField)).keyboardType,
      TextInputType.multiline,
    );
  });

  testWidgets(
    '200 percent long plain text scrolls with accessible actions and safe huge time',
    (t) async {
      await f.open();
      t.view.physicalSize = const Size(720, 1280);
      t.view.devicePixelRatio = 2;
      addTearDown(t.view.resetPhysicalSize);
      addTearDown(t.view.resetDevicePixelRatio);
      final semantics = t.ensureSemantics();

      f.client.rows = [
        row(
          1,
          body:
              '<script>Public test</script>\n${List.filled(80, 'Public long body').join(' ')}',
          time: BigInt.parse('9223372036854775807'),
        ),
      ];
      await show(t, scale: 2);
      expect(t.takeException(), isNull);
      expect(
        find.textContaining('<script>Public test</script>'),
        findsOneWidget,
      );
      expect(find.text('시각 확인 불가'), findsOneWidget);
      expect(find.bySemanticsLabel('메시지 입력'), findsOneWidget);
      await t.drag(
        find.byKey(const ValueKey('message-viewport')),
        const Offset(0, 300),
      );
      await t.pumpAndSettle();
      expect(t.takeException(), isNull);
      semantics.dispose();
    },
  );
}
