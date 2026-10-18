import 'dart:async';

import 'package:chat_app/network/network_model.dart';
import 'package:chat_app/src/rust/api/chat.dart' as rust;
import 'package:flutter_test/flutter_test.dart';
import 'package:flutter/foundation.dart';

import 'support/fake_network_client.dart';
import 'account_model_test.dart' as account_fixture show view;

rust.NetworkInfo info(
  int revision, {
  String id = 'new',
  List<rust.PeerInfo> peers = const [],
}) => rust.NetworkInfo(
  revision: BigInt.from(revision),
  state: rust.NetworkState.online,
  endpointId: id,
  peers: peers,
);

void main() {
  TestWidgetsFlutterBinding.ensureInitialized();
  final diagnostics = <String?>[];
  late DebugPrintCallback previousPrint;
  setUp(() {
    diagnostics.clear();
    previousPrint = debugPrint;
    debugPrint = (message, {wrapWidth}) => diagnostics.add(message);
  });
  tearDown(() => debugPrint = previousPrint);
  test('account events preserve M2 connection state timings and diagnostics', () async {
    final client = FakeNetworkClient();
    final model = NetworkModel(client);
    final accountEvents = <rust.ChatEvent>[];
    final accountSubscription = client.events().listen(accountEvents.add);
    await model.open();
    model.applySnapshot(info(7, peers: [rust.PeerInfo(
      peerId: 'transport-peer',
      phase: rust.PeerPhase.connected,
      path: rust.PeerPath.direct,
      connectMs: BigInt.from(123),
      directMs: BigInt.from(456),
    )]));
    model.appendLine('existing M2 diagnostic');
    final before = model.snapshot;
    final beforeLines = List<String>.of(model.recentLines);
    final beforeDiagnostics = List<String?>.of(diagnostics);
    final changed = rust.ChatEvent.accountChanged(
      revision: BigInt.from(99),
      view: account_fixture.view(BigInt.from(99)),
    );
    const cached = rust.ChatEvent.accountCacheUsed(peerId: 'account-peer');
    const ready = rust.ChatEvent.accountAuthChanged(
      peerId: 'account-peer', installationId: 'installation', ready: true,
    );
    client.controller.add(cached);
    client.controller.add(ready);
    client.controller.add(changed);
    await Future<void>.delayed(Duration.zero);
    expect(accountEvents, [cached, ready, changed]);
    expect(model.snapshot, same(before));
    expect(model.snapshot!.state, rust.NetworkState.online);
    expect(model.snapshot!.peers.single.connectMs, BigInt.from(123));
    expect(model.snapshot!.peers.single.directMs, BigInt.from(456));
    expect(model.recentLines, beforeLines);
    expect(diagnostics, beforeDiagnostics);
    expect(model.error, isNull);
    await accountSubscription.cancel();
    await model.close();
    await client.controller.close();
  });
  test('account failure hides raw detail and releases busy state', () async {
    final client = FakeNetworkClient();
    final model = NetworkModel(client);
    await model.open();
    for (final code in [
      rust.ErrorCode.invalidRecovery,
      rust.ErrorCode.storageLocked,
      rust.ErrorCode.storageMissing,
      rust.ErrorCode.storageCorrupt,
      rust.ErrorCode.storageFull,
      rust.ErrorCode.accountExists,
      rust.ErrorCode.writerBusy,
      rust.ErrorCode.invalidInvite,
      rust.ErrorCode.inviteExpired,
      rust.ErrorCode.deviceLimit,
      rust.ErrorCode.pairMismatch,
      rust.ErrorCode.pairExpired,
      rust.ErrorCode.recordConflict,
      rust.ErrorCode.recordUnavailable,
      rust.ErrorCode.authFailed,
      rust.ErrorCode.accountRestricted,
    ]) {
      client.commandError = rust.CoreError(
        code: code,
        detail: 'secret storage diagnostics',
      );
      expect(await model.startNetwork(), isFalse);
      expect(model.busy, isFalse);
      expect(model.error, '계정 작업을 완료할 수 없어요. 다시 시도해 주세요.');
      expect(model.error, isNot(contains('secret storage diagnostics')));
      expect(model.error, isNot(contains(code.name)));
    }
    expect(diagnostics.join(), isNot(contains('secret storage diagnostics')));
    expect(model.recentLines, isEmpty);
    await model.close();
    await client.controller.close();
  });
  test('subscribes before delayed snapshot and rejects older state', () async {
    final client = FakeNetworkClient();
    final delayed = Completer<rust.NetworkInfo>();
    client.delayedInfo = delayed.future;
    final model = NetworkModel(client);
    final opening = model.open();
    expect(client.controller.hasListener, isTrue);
    client.controller.add(rust.ChatEvent.networkChanged(info: info(9)));
    await Future<void>.delayed(Duration.zero);
    delayed.complete(info(8, id: 'old'));
    await opening;
    expect(model.snapshot!.endpointId, 'new');
    expect(model.snapshot!.revision, BigInt.from(9));
    await model.close();
    await client.controller.close();
  });

  test(
    'equal revision peer update is accepted and full snapshot is authoritative',
    () async {
      final client = FakeNetworkClient();
      final model = NetworkModel(client);
      await model.open();
      model.applySnapshot(info(9));
      const peer = rust.PeerInfo(
        peerId: 'peer',
        phase: rust.PeerPhase.connected,
        path: rust.PeerPath.direct,
      );
      client.controller.add(
        rust.ChatEvent.peerChanged(revision: BigInt.from(9), peer: peer),
      );
      await Future<void>.delayed(Duration.zero);
      expect(model.snapshot!.peers.single.path, rust.PeerPath.direct);
      client.controller.add(
        rust.ChatEvent.peerChanged(revision: BigInt.from(8), peer: peer),
      );
      model.applySnapshot(info(10));
      await Future<void>.delayed(Duration.zero);
      expect(model.snapshot!.peers, isEmpty);
      model.applySnapshot(info(10, id: 'authoritative'));
      expect(model.snapshot!.endpointId, 'authoritative');
      await model.close();
      await client.controller.close();
    },
  );

  test('body history keeps newest 200 lines only', () {
    final model = NetworkModel(FakeNetworkClient());
    for (var i = 0; i < 205; i++) {
      model.appendLine('line $i');
    }
    expect(model.recentLines.length, 200);
    expect(model.recentLines.first, 'line 5');
    expect(model.recentLines.last, 'line 204');
    model.dispose();
  });

  test('failed send clears busy and never replays on recovery', () async {
    final client = FakeNetworkClient();
    final model = NetworkModel(client);
    await model.open();
    client.commandError = const rust.CoreError(
      code: rust.ErrorCode.peerNotConnected,
      detail: 'offline',
    );
    expect(await model.sendProbeText('peer', 'failed body'), isFalse);
    expect(model.busy, isFalse);
    expect(model.error, contains('peerNotConnected'));
    client.commandError = null;
    client.controller.add(rust.ChatEvent.networkChanged(info: info(4)));
    await Future<void>.delayed(Duration.zero);
    expect(client.sends, 1);
    expect(model.recentLines, isEmpty);
    await model.close();
    await client.controller.close();
  });

  test(
    'errors and stream end are visible and close cancels listener',
    () async {
      final client = FakeNetworkClient();
      final model = NetworkModel(client);
      await model.open();
      client.controller.add(
        const rust.ChatEvent.error(code: rust.ErrorCode.busy, detail: 'full'),
      );
      await Future<void>.delayed(Duration.zero);
      expect(model.error, contains('busy'));
      client.controller.addError(
        StateError('stream broke'),
        StackTrace.current,
      );
      await Future<void>.delayed(Duration.zero);
      expect(model.error, contains('stream broke'));
      expect(
        diagnostics.join(),
        contains('ChatProject network operation failed'),
      );
      await client.controller.close();
      expect(model.error, contains('종료'));
      await model.close();
      expect(client.controller.hasListener, isFalse);
    },
  );

  test('close cancels while opening and ignores delayed snapshot', () async {
    final client = FakeNetworkClient();
    final delayed = Completer<rust.NetworkInfo>();
    client.delayedInfo = delayed.future;
    final model = NetworkModel(client);
    final opening = model.open();
    await model.close();
    expect(client.controller.hasListener, isFalse);
    delayed.complete(info(7));
    await opening;
    expect(model.snapshot, isNull);
    await client.controller.close();
  });
}
