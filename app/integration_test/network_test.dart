import 'dart:async';
import 'dart:io';
import 'dart:convert';

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

  test('Debug hook waits for Local config and raw late stream receives state', () async {
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 47000);
    final hello = Completer<Map<String, dynamic>>();
    final connected = Completer<WebSocket>();
    final serverSubscription = server.listen((request) async {
      final socket = await WebSocketTransformer.upgrade(request);
      connected.complete(socket);
      socket.listen((data) {
        final record = jsonDecode(data as String) as Map<String, dynamic>;
        if (record['type'] == 'hello' && !hello.isCompleted) {
          hello.complete(record);
        }
      });
    });
    WebSocket? socket;
    try {
      var ready = false;
      final startup = testHooksStart(controlUrl: 'ws://127.0.0.1:47000', name: 'flutter-test')
          .then((_) { ready = true; });
      final record = await hello.future.timeout(const Duration(seconds: 5));
      expect(record['name'], 'flutter-test');
      expect((await networkInfo()).endpointId, isNull);
      expect((await networkInfo()).state, NetworkState.stopped);
      expect(ready, isFalse);
      socket = await connected.future;
      socket.add(jsonEncode({'type': 'config', 'relay': null}));
      await startup.timeout(const Duration(seconds: 5));
      await networkStart();
      final deadline = DateTime.now().add(const Duration(seconds: 10));
      while ((await networkInfo()).state != NetworkState.online) {
        if (DateTime.now().isAfter(deadline)) {
          fail('Local endpoint did not become ready');
        }
        await Future<void>.delayed(const Duration(milliseconds: 10));
      }
      final identity = (await networkInfo()).endpointId;
      expect(identity, isNotNull);
      expect((await networkStart()).endpointId, identity);
      // Cancel at the yielded snapshot; FRB's async* stream can otherwise wait
      // for another native event before finishing idle subscription cancellation.
      final state = await events().first.timeout(const Duration(seconds: 5));
      expect(state, isA<ChatEvent_ConnectionState>());
      await socket.close().timeout(const Duration(seconds: 5));
      expect((await networkInfo()).state, NetworkState.online);
    } finally {
      await networkStop();
      await socket?.close().timeout(const Duration(seconds: 5));
      await server.close(force: true);
      await serverSubscription.cancel();
    }
  });
}
