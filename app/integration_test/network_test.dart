import 'dart:async';
import 'dart:io';

import 'package:chat_app/core_client.dart';
import 'package:chat_app/src/rust/api/chat.dart';
import 'package:chat_app/src/rust/frb_generated.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  late Directory dataDir;
  setUpAll(() async {
    await RustLib.init();
    dataDir = await Directory.systemTemp.createTemp('chat_network_it_');
  });
  tearDownAll(() => dataDir.delete(recursive: true));

  test('stopped commands recover after an event subscription is canceled', () async {
    final core = await coreStart(dataDir: dataDir.path);
    final firstState = Completer<ChatEvent_ConnectionState>();
    final subscription = events().listen((event) {
      if (event is ChatEvent_ConnectionState && !firstState.isCompleted) {
        firstState.complete(event);
      }
    });
    expect((await firstState.future.timeout(const Duration(seconds: 5))).directPeers, 0);
    await subscription.cancel();

    for (var i = 0; i < 2; i++) {
      final info = await networkInfo();
      expect(info.state, NetworkState.stopped);
      expect(info.endpointId, isNull);
      expect(info.ticket, isNull);
      expect(info.peers, isEmpty);
      await networkStop();
    }

    final event = await events().first.timeout(const Duration(seconds: 5));
    expect(event, isA<ChatEvent_ConnectionState>());
    final state = event as ChatEvent_ConnectionState;
    expect(state.directPeers, 0);
    expect(state.mailboxOk, isFalse);
    expect(state.queueOk, isNull);
    expect((await coreStart(dataDir: dataDir.path)).instanceId, core.instanceId);
  });

  test('Frb client shares one stream and repeated start publishes no extra state', () async {
    final client = FrbCoreClient(dataDir: dataDir.path);
    final stream = client.events();
    expect(identical(stream, client.events()), isTrue);
    final first = <ChatEvent>[];
    final second = <ChatEvent>[];
    final replay = Completer<void>();
    final a = stream.listen((event) {
      first.add(event);
      if (event is ChatEvent_ConnectionState && !replay.isCompleted) replay.complete();
    });
    final b = client.events().listen(second.add);
    await replay.future.timeout(const Duration(seconds: 5));
    expect(first.single, isA<ChatEvent_ConnectionState>());
    expect(second.single, isA<ChatEvent_ConnectionState>());
    await client.start();
    await client.start();
    // Observe the bridge long enough to detect an accidental duplicate startup event.
    await Future<void>.delayed(const Duration(milliseconds: 100));
    expect(first.length, 1);
    expect(second.length, 1);
    await a.cancel();
    await b.cancel();
    final lateEvents = <ChatEvent>[];
    final lateSubscription = client.events().listen(lateEvents.add);
    await Future<void>.delayed(const Duration(milliseconds: 100));
    expect(lateEvents, isEmpty);
    await lateSubscription.cancel();
    expect((await client.networkInfo()).state, NetworkState.stopped);
  });
}
