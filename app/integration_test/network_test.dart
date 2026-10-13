import 'dart:async';
import 'dart:io';

import 'package:chat_app/src/rust/api/chat.dart';
import 'package:chat_app/src/rust/frb_generated.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  test('stopped commands recover after an event subscription is canceled', () async {
    await RustLib.init();
    final dataDir = await Directory.systemTemp.createTemp('chat_network_it_');
    addTearDown(() => dataDir.delete(recursive: true));
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
}
