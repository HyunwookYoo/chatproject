import 'dart:async';

import 'package:chat_app/home/home_screen.dart';
import 'package:chat_app/network/network_screen.dart';
import 'package:chat_app/src/rust/api/chat.dart' as rust;
import 'package:chat_app/theme/app_theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

import 'support/fake_network_client.dart';

const peer = rust.PeerInfo(
  peerId: 'remote-peer',
  phase: rust.PeerPhase.connected,
  path: rust.PeerPath.direct,
);
FakeNetworkClient onlineClient({List<rust.PeerInfo> peers = const [peer]}) =>
    FakeNetworkClient()
      ..info = rust.NetworkInfo(
        revision: BigInt.from(9),
        state: rust.NetworkState.online,
        endpointId: 'my-endpoint',
        ticket: 'my-ticket',
        peers: peers,
      );

Future<void> pumpNetwork(
  WidgetTester tester,
  FakeNetworkClient client, {
  double width = 1280,
}) async {
  tester.view.physicalSize = Size(width, 900);
  tester.view.devicePixelRatio = 1;
  addTearDown(tester.view.resetPhysicalSize);
  addTearDown(tester.view.resetDevicePixelRatio);
  addTearDown(() {
    unawaited(client.controller.close());
  });
  await tester.pumpWidget(
    MaterialApp(
      theme: buildAppTheme(),
      home: NetworkScreen(client: client),
    ),
  );
  await tester.pumpAndSettle();
}

void main() {
  for (final width in [360.0, 1280.0]) {
    testWidgets('probe is usable without overflow at $width', (tester) async {
      final client = onlineClient();
      await pumpNetwork(tester, client, width: width);
      expect(tester.takeException(), isNull);
      expect(find.text('네트워크 측정'), findsOneWidget);
      expect(find.textContaining('프로세스를 다시 시작'), findsOneWidget);
      expect(find.text('티켓에는 연결 주소가 들어 있어요. 연결할 상대에게만 공유해 주세요.'), findsOneWidget);
      await tester.ensureVisible(find.text('remote-peer'));
      await tester.tap(find.text('remote-peer'));
      await tester.pump();
      await tester.ensureVisible(find.byKey(const Key('probe-text')));
      await tester.enterText(
        find.byKey(const Key('probe-text')),
        'foreground text',
      );
      await tester.pump();
      await tester.ensureVisible(find.text('전송'));
      await tester.tap(find.text('전송'));
      await tester.pumpAndSettle();
      expect(client.sentPeer, 'remote-peer');
      expect(client.sentText, 'foreground text');
      expect(find.textContaining('쓰기 완료'), findsOneWidget);
      expect(find.text('상대 화면 표시나 저장을 확인한 상태는 아니에요'), findsOneWidget);
      expect(tester.takeException(), isNull);
    });
  }

  testWidgets('disconnected peer disables send and failed body is retained', (
    tester,
  ) async {
    final client = onlineClient(
      peers: [
        const rust.PeerInfo(
          peerId: 'offline-peer',
          phase: rust.PeerPhase.disconnected,
          path: rust.PeerPath.unknown,
        ),
      ],
    );
    await pumpNetwork(tester, client);
    await tester.tap(find.text('offline-peer'));
    await tester.enterText(find.byKey(const Key('probe-text')), 'draft');
    await tester.pump();
    final send = tester.widget<FilledButton>(
      find.widgetWithText(FilledButton, '전송'),
    );
    expect(send.onPressed, isNull);
    client.controller.add(
      rust.ChatEvent.networkChanged(
        info: rust.NetworkInfo(
          revision: BigInt.from(10),
          state: rust.NetworkState.online,
          endpointId: 'my-endpoint',
          peers: [
            const rust.PeerInfo(
              peerId: 'offline-peer',
              phase: rust.PeerPhase.connected,
              path: rust.PeerPath.relay,
            ),
          ],
        ),
      ),
    );
    await tester.pumpAndSettle();
    client.commandError = const rust.CoreError(
      code: rust.ErrorCode.peerNotConnected,
      detail: 'gone',
    );
    await tester.tap(find.text('전송'));
    await tester.pumpAndSettle();
    expect(
      tester
          .widget<TextField>(find.byKey(const Key('probe-text')))
          .controller!
          .text,
      'draft',
    );
    expect(client.sends, 1);
    expect(find.textContaining('peerNotConnected'), findsOneWidget);
  });

  testWidgets(
    'copies endpoint and ticket, connects target and disconnects selected peer',
    (tester) async {
      final copied = <String>[];
      tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        SystemChannels.platform,
        (call) async {
          if (call.method == 'Clipboard.setData') {
            copied.add((call.arguments as Map)['text'] as String);
          }
          return null;
        },
      );
      addTearDown(
        () => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(
          SystemChannels.platform,
          null,
        ),
      );
      final client = onlineClient();
      await pumpNetwork(tester, client);
      await tester.tap(find.byTooltip('ID 복사'));
      await tester.tap(find.byTooltip('티켓 복사'));
      expect(copied, ['my-endpoint', 'my-ticket']);
      await tester.enterText(
        find.byKey(const Key('peer-target')),
        'target-ticket',
      );
      await tester.pump();
      await tester.tap(find.widgetWithText(FilledButton, '연결'));
      await tester.pumpAndSettle();
      expect(client.target, 'target-ticket');
      await tester.tap(find.text('remote-peer'));
      await tester.pump();
      await tester.tap(find.text('연결 해제'));
      await tester.pumpAndSettle();
      expect(client.disconnectedPeer, 'remote-peer');
    },
  );

  testWidgets('busy command disables controls until error finally cleanup', (
    tester,
  ) async {
    final client = onlineClient();
    final delayed = Completer<void>();
    client.delayedCommand = delayed.future;
    await pumpNetwork(tester, client);
    await tester.enterText(find.byKey(const Key('peer-target')), 'target');
    await tester.pump();
    await tester.tap(find.widgetWithText(FilledButton, '연결'));
    await tester.pump();
    expect(
      tester
          .widget<FilledButton>(find.widgetWithText(FilledButton, '연결'))
          .onPressed,
      isNull,
    );
    expect(
      tester.widget<TextField>(find.byKey(const Key('peer-target'))).enabled,
      isFalse,
    );
    delayed.completeError(
      const rust.CoreError(code: rust.ErrorCode.busy, detail: 'full'),
    );
    await tester.pumpAndSettle();
    expect(
      tester
          .widget<FilledButton>(find.widgetWithText(FilledButton, '연결'))
          .onPressed,
      isNotNull,
    );
    expect(find.textContaining('busy'), findsOneWidget);
  });

  const messages = {
    rust.ErrorCode.invalidConfig: '시작 설정과 데이터 폴더를 확인해 주세요.',
    rust.ErrorCode.alreadyStartedDifferentConfig: '같은 데이터 폴더로 다시 시작해 주세요.',
    rust.ErrorCode.coreNotStarted: '앱을 다시 시작해 주세요.',
    rust.ErrorCode.networkNotStarted: '네트워크를 시작해 주세요.',
    rust.ErrorCode.invalidPeerAddress: '상대 ID나 티켓을 확인해 주세요.',
    rust.ErrorCode.selfConnection: '다른 기기의 ID나 티켓을 입력해 주세요.',
    rust.ErrorCode.peerNotConnected: '상대를 연결한 뒤 다시 보내 주세요.',
    rust.ErrorCode.messageTooLarge: '본문은 UTF-8 기준 1–4096바이트여야 해요.',
    rust.ErrorCode.busy: '진행 중인 작업이 끝난 뒤 다시 시도해 주세요.',
    rust.ErrorCode.networkTimeout: '네트워크를 확인하고 다시 연결해 주세요.',
    rust.ErrorCode.transportFailed: '네트워크와 접속 권한을 확인하고 다시 시도해 주세요.',
    rust.ErrorCode.notAvailable: '이 기기의 네트워크와 접속 권한을 확인해 주세요.',
  };
  for (final entry in messages.entries) {
    testWidgets('${entry.key.name} is visible with retry guidance', (
      tester,
    ) async {
      final client = onlineClient();
      await pumpNetwork(tester, client);
      client.controller.add(
        rust.ChatEvent.error(
          code: entry.key,
          detail: 'private body must not display',
        ),
      );
      await tester.pumpAndSettle();
      expect(find.textContaining(entry.value), findsOneWidget);
      expect(find.textContaining('private body'), findsNothing);
      expect(find.text('다시 시도'), findsOneWidget);
      await tester.tap(find.text('다시 시도'));
      await tester.pumpAndSettle();
      expect(find.textContaining(entry.value), findsNothing);
    });
  }

  testWidgets('stream failure and completion are visible', (tester) async {
    final client = onlineClient();
    await pumpNetwork(tester, client);
    client.controller.addError(StateError('stream failed'), StackTrace.current);
    await tester.pumpAndSettle();
    expect(find.textContaining('stream failed'), findsOneWidget);
    unawaited(client.controller.close());
    await tester.pumpAndSettle();
    expect(find.textContaining('종료'), findsOneWidget);
  });

  testWidgets(
    'idle Home keeps existing labels and opens optional network page',
    (tester) async {
      final client = onlineClient();
      addTearDown(() {
        unawaited(client.controller.close());
      });
      await tester.pumpWidget(
        MaterialApp(
          theme: buildAppTheme(),
          home: HomeScreen(core: client, network: client),
        ),
      );
      client.controller.add(
        const rust.ChatEvent.connectionState(directPeers: 0, mailboxOk: false),
      );
      await tester.pumpAndSettle();
      expect(find.text('대화'), findsOneWidget);
      expect(find.text('아직 대화가 없어요'), findsOneWidget);
      expect(find.text('연결 없음'), findsOneWidget);
      await tester.tap(find.byTooltip('네트워크 측정'));
      await tester.pumpAndSettle();
      expect(find.text('네트워크 측정'), findsOneWidget);
    },
  );
}
