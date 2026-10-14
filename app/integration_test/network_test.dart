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
  late FrbCoreClient client;
  setUpAll(() async {
    await RustLib.init();
    dataDir = await Directory.systemTemp.createTemp('chat_network_it_');
    client = FrbCoreClient(dataDir: dataDir.path);
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

  test('real node messages are fresh after UI listeners are removed and resubscribed', () async {
    final executable = File('${Directory.current.parent.path}/target/debug/chat_node${Platform.isWindows ? '.exe' : ''}');
    expect(executable.existsSync(), isTrue,
        reason: 'Build chat_node with cargo build -p chat_node --features harness first');
    final server = await HttpServer.bind(InternetAddress.loopbackIPv4, 47000);
    final participants = <String, Completer<_Participant>>{
      'flutter-messages': Completer<_Participant>(),
      'node-messages': Completer<_Participant>(),
    };
    final serverSubscription = server.listen((request) async {
      final socket = await WebSocketTransformer.upgrade(request);
      final participant = _Participant(socket, (name, participant) {
        participants[name]!.complete(participant);
      });
      participant.configure();
    });
    final nodeDir = await Directory.systemTemp.createTemp('chat_message_node_');
    Process? relay;
    Process? node;
    _Participant? appControl;
    _Participant? nodeControl;
    StreamSubscription<ChatEvent>? uiSubscription;
    try {
      relay = await Process.start(executable.path, ['relay', '--bind', '127.0.0.1:3340']);
      final relayOutput = relay.stdout.transform(utf8.decoder).transform(const LineSplitter());
      final ready = await relayOutput.first.timeout(const Duration(seconds: 10));
      expect(jsonDecode(ready), {'type': 'ready', 'role': 'relay'});
      relay.stderr.drain<void>();
      await testHooksStart(controlUrl: 'ws://127.0.0.1:47000', name: 'flutter-messages');
      appControl = await participants['flutter-messages']!.future.timeout(const Duration(seconds: 10));
      node = await Process.start(executable.path, [
        'puppet', '--control-url', 'ws://127.0.0.1:47000',
        '--name', 'node-messages', '--data-dir', nodeDir.path,
      ]);
      node.stdout.drain<void>();
      node.stderr.drain<void>();
      nodeControl = await participants['node-messages']!.future.timeout(const Duration(seconds: 10));
      await client.startNetwork();
      await nodeControl.rpc('network_start', {});
      final deadline = DateTime.now().add(const Duration(seconds: 15));
      while ((await client.networkInfo()).state != NetworkState.online ||
          (await nodeControl.rpc('network_info', {}))['state'] != 'Online') {
        if (DateTime.now().isAfter(deadline)) fail('Local message endpoints did not become ready');
        await Future<void>.delayed(const Duration(milliseconds: 10));
      }
      final appInfo = await client.networkInfo();
      await nodeControl.rpc('connect_peer', {'target': appInfo.ticket});
      final firstBodies = <String>[];
      final firstReceived = Completer<void>();
      uiSubscription = client.events().listen((event) {
        if (event is ChatEvent_ProbeTextReceived) {
          firstBodies.add(event.text);
          if (!firstReceived.isCompleted) firstReceived.complete();
        }
      });
      await nodeControl.rpc('send_probe_text', {'peer_id': appInfo.endpointId, 'text': 'first real body 안녕'});
      await firstReceived.future.timeout(const Duration(seconds: 10));
      expect(firstBodies, ['first real body 안녕']);
      // This is the same cached UI broadcast, with an ordinary listener cancel.
      // Its permanent native subscription stays alive across page lifecycles.
      await uiSubscription.cancel().timeout(const Duration(seconds: 5));
      final laterBodies = <String>[];
      final freshReceived = Completer<void>();
      uiSubscription = client.events().listen((event) {
        if (event is ChatEvent_ProbeTextReceived) {
          laterBodies.add(event.text);
          if (!freshReceived.isCompleted) freshReceived.complete();
        }
      });
      // Also exercise a new raw native subscription with the real fresh body.
      // Cancel at its yielded message to respect FRB2.11.1's async* lifecycle.
      final rawFresh = events().where((event) => event is ChatEvent_ProbeTextReceived).first;
      await nodeControl.rpc('send_probe_text', {'peer_id': appInfo.endpointId, 'text': 'fresh real body 새 글'});
      await freshReceived.future.timeout(const Duration(seconds: 10));
      expect(laterBodies, ['fresh real body 새 글']);
      expect((await rawFresh.timeout(const Duration(seconds: 10)) as ChatEvent_ProbeTextReceived).text,
          'fresh real body 새 글');
      expect(firstBodies, ['first real body 안녕']);
      await nodeControl.rpc('network_stop', {});
    } finally {
      for (final process in [node, relay]) {
        process?.kill();
      }
      // Attempt all owned cleanup even if cancellation or a close fails.
      try {
        await Future.wait<void>([
          if (uiSubscription != null) uiSubscription.cancel().timeout(const Duration(seconds: 5)),
          networkStop().timeout(const Duration(seconds: 10)),
          if (appControl != null) appControl.close(),
          if (nodeControl != null) nodeControl.close(),
          for (final process in [node, relay])
            if (process != null) process.exitCode.timeout(const Duration(seconds: 10)).then((_) {}),
          server.close(force: true).then((_) {}),
          serverSubscription.cancel(),
        ]);
      } finally {
        await nodeDir.delete(recursive: true);
      }
    }
  });
}

class _Participant {
  _Participant(this.socket, void Function(String, _Participant) hello) {
    subscription = socket.listen((data) {
      final record = jsonDecode(data as String) as Map<String, dynamic>;
      if (record['type'] == 'hello') hello(record['name'] as String, this);
      if (record['type'] == 'result') pending.remove(record['id'])!.complete(record);
    }, onDone: () {
      for (final request in pending.values) {
        request.completeError(StateError('participant disconnected'));
      }
      pending.clear();
    });
  }
  final WebSocket socket;
  late final StreamSubscription<dynamic> subscription;
  final pending = <int, Completer<Map<String, dynamic>>>{};
  var nextId = 0;
  void configure() => socket.add(jsonEncode({'type': 'config', 'relay': 'http://127.0.0.1:3340'}));
  Future<dynamic> rpc(String op, Map<String, dynamic> args) async {
    final id = ++nextId;
    final result = Completer<Map<String, dynamic>>();
    pending[id] = result;
    socket.add(jsonEncode({'type': 'cmd', 'id': id, 'op': op, 'args': args}));
    final record = await result.future.timeout(const Duration(seconds: 15));
    expect(record['ok'], isTrue, reason: 'real node RPC $op must succeed');
    return record['value'];
  }
  Future<void> close() async {
    await socket.close().timeout(const Duration(seconds: 5));
    await subscription.cancel();
  }
}
