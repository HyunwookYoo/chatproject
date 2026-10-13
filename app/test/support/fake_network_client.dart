import 'dart:async';

import 'package:chat_app/core_client.dart';
import 'package:chat_app/network/network_client.dart';
import 'package:chat_app/src/rust/api/chat.dart' as rust;

class FakeNetworkClient implements CoreClient, NetworkClient {
  final controller = StreamController<rust.ChatEvent>.broadcast();
  rust.NetworkInfo info = rust.NetworkInfo(
    revision: BigInt.zero,
    state: rust.NetworkState.stopped,
    peers: [],
  );
  Future<rust.NetworkInfo>? delayedInfo;
  Future<void>? delayedCommand;
  Object? commandError;
  Object? startError;
  int sends = 0;
  String? target;
  String? sentPeer;
  String? sentText;
  String? disconnectedPeer;

  @override
  String get version => 'test';
  @override
  Future<rust.CoreInfo> start() async {
    if (startError case final error?) throw error;
    return const rust.CoreInfo(version: 'test', instanceId: 'test');
  }

  @override
  Stream<rust.ChatEvent> events() => controller.stream;
  @override
  Future<rust.NetworkInfo> networkInfo() => delayedInfo ?? Future.value(info);
  Future<void> _command() async {
    await delayedCommand;
    if (commandError case final error?) throw error;
  }

  @override
  Future<rust.NetworkInfo> startNetwork() async {
    await _command();
    return info;
  }

  @override
  Future<rust.PeerInfo> connectPeer(String value) async {
    target = value;
    await _command();
    return rust.PeerInfo(
      peerId: value,
      phase: rust.PeerPhase.connected,
      path: rust.PeerPath.relay,
    );
  }

  @override
  Future<void> sendProbeText(String peerId, String text) async {
    sends++;
    sentPeer = peerId;
    sentText = text;
    await _command();
  }

  @override
  Future<void> disconnectPeer(String peerId) async {
    disconnectedPeer = peerId;
    await _command();
  }

  @override
  Future<void> stopNetwork() => _command();
}
