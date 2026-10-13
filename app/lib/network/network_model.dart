import 'dart:async';

import 'package:flutter/foundation.dart';

import '../src/rust/api/chat.dart' as rust;
import 'network_client.dart';

class NetworkModel extends ChangeNotifier {
  NetworkModel(this.client);

  final NetworkClient client;
  StreamSubscription<rust.ChatEvent>? _subscription;
  int _generation = 0;
  bool _closed = false;
  rust.NetworkInfo? snapshot;
  final List<String> recentLines = [];
  String? error;
  bool busy = false;

  Future<void> open() async {
    final generation = ++_generation;
    _closed = false;
    final previous = _subscription;
    if (previous != null) await previous.cancel();
    if (_closed || generation != _generation) return;
    _subscription = client.events().listen(
      _event,
      onError: (Object failure, StackTrace stack) => _failure(failure, stack),
      onDone: () => _failure(StateError('이벤트 스트림이 종료됐어요'), StackTrace.current),
    );
    try {
      final next = await client.networkInfo();
      if (!_closed && generation == _generation) applySnapshot(next);
    } catch (failure, stack) {
      if (!_closed && generation == _generation) _failure(failure, stack);
    }
  }

  void applySnapshot(rust.NetworkInfo next) {
    if (_closed || (snapshot != null && next.revision < snapshot!.revision)) {
      return;
    }
    snapshot = next;
    notifyListeners();
  }

  void _event(rust.ChatEvent event) {
    if (_closed) return;
    switch (event) {
      case rust.ChatEvent_NetworkChanged(:final info):
        applySnapshot(info);
      case rust.ChatEvent_PeerChanged(:final revision, :final peer):
        final current = snapshot;
        if (current == null || revision < current.revision) return;
        final peers = [...current.peers];
        final index = peers.indexWhere((item) => item.peerId == peer.peerId);
        if (index < 0) {
          peers.add(peer);
        } else {
          peers[index] = peer;
        }
        applySnapshot(
          rust.NetworkInfo(
            revision: revision,
            state: current.state,
            endpointId: current.endpointId,
            ticket: current.ticket,
            peers: peers,
          ),
        );
      case rust.ChatEvent_ProbeTextReceived(:final peerId, :final text):
        appendLine('수신 · $peerId\n$text');
      case rust.ChatEvent_Error(:final code, :final detail):
        _failure(
          rust.CoreError(code: code, detail: detail),
          StackTrace.current,
        );
      case rust.ChatEvent_ConnectionState():
        break;
    }
  }

  void appendLine(String line) {
    if (_closed) return;
    recentLines.add(line);
    if (recentLines.length > 200) recentLines.removeAt(0);
    notifyListeners();
  }

  void _failure(Object failure, StackTrace stack) {
    if (_closed) return;
    error = failure is rust.CoreError
        ? '${failure.code.name} · ${networkErrorMessage(failure.code)}'
        : '$failure';
    // The fixed diagnostic context avoids logging targets, tickets or probe bodies.
    debugPrint(
      'ChatProject network operation failed: ${failure is rust.CoreError ? failure.code.name : failure}',
    );
    debugPrintStack(
      label: 'ChatProject network operation failed',
      stackTrace: stack,
    );
    notifyListeners();
  }

  Future<bool> _run(Future<void> Function() operation) async {
    if (busy || _closed) return false;
    busy = true;
    error = null;
    notifyListeners();
    try {
      await operation();
      return true;
    } catch (failure, stack) {
      _failure(failure, stack);
      return false;
    } finally {
      busy = false;
      if (!_closed) notifyListeners();
    }
  }

  Future<bool> startNetwork() =>
      _run(() async => applySnapshot(await client.startNetwork()));
  Future<bool> connectPeer(String target) => _run(() async {
    await client.connectPeer(target);
    applySnapshot(await client.networkInfo());
  });
  Future<bool> sendProbeText(String peerId, String text) => _run(() async {
    await client.sendProbeText(peerId, text);
    appendLine('쓰기 완료 · $peerId\n$text');
  });
  Future<bool> disconnectPeer(String peerId) => _run(() async {
    await client.disconnectPeer(peerId);
    applySnapshot(await client.networkInfo());
  });
  Future<bool> stopNetwork() => _run(() async {
    await client.stopNetwork();
    applySnapshot(await client.networkInfo());
  });
  Future<bool> retry() => _run(() async {
    await open();
    if (!_closed && snapshot?.state != rust.NetworkState.online) {
      applySnapshot(await client.startNetwork());
    }
  });

  Future<void> close() async {
    _closed = true;
    ++_generation;
    final subscription = _subscription;
    _subscription = null;
    await subscription?.cancel();
  }

  @override
  void dispose() {
    unawaited(close());
    super.dispose();
  }
}

String networkErrorMessage(rust.ErrorCode code) => switch (code) {
  rust.ErrorCode.invalidConfig => '시작 설정과 데이터 폴더를 확인해 주세요.',
  rust.ErrorCode.alreadyStartedDifferentConfig => '같은 데이터 폴더로 다시 시작해 주세요.',
  rust.ErrorCode.coreNotStarted => '앱을 다시 시작해 주세요.',
  rust.ErrorCode.networkNotStarted => '네트워크를 시작해 주세요.',
  rust.ErrorCode.invalidPeerAddress => '상대 ID나 티켓을 확인해 주세요.',
  rust.ErrorCode.selfConnection => '다른 기기의 ID나 티켓을 입력해 주세요.',
  rust.ErrorCode.peerNotConnected => '상대를 연결한 뒤 다시 보내 주세요.',
  rust.ErrorCode.messageTooLarge => '본문은 UTF-8 기준 1–4096바이트여야 해요.',
  rust.ErrorCode.busy => '진행 중인 작업이 끝난 뒤 다시 시도해 주세요.',
  rust.ErrorCode.networkTimeout => '네트워크를 확인하고 다시 연결해 주세요.',
  rust.ErrorCode.transportFailed => '네트워크와 접속 권한을 확인하고 다시 시도해 주세요.',
  rust.ErrorCode.notAvailable => '이 기기의 네트워크와 접속 권한을 확인해 주세요.',
};
