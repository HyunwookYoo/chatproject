import 'dart:io';

import 'package:path_provider/path_provider.dart';

import 'src/rust/api/chat.dart' as rust;
import 'network/network_client.dart';

/// The UI reaches the Rust core only through this interface, so widget tests can use
/// a fake core.
abstract interface class CoreClient {
  String get version;
  Future<rust.CoreInfo> start();
  Stream<rust.ChatEvent> events();
}

class FrbCoreClient implements CoreClient, NetworkClient {
  FrbCoreClient({this.dataDir});

  final String? dataDir;
  Stream<rust.ChatEvent>? _events;

  @override
  String get version => rust.coreVersion();

  @override
  Future<rust.CoreInfo> start() async {
    if (dataDir != null) return rust.coreStart(dataDir: dataDir!);
    final support = await getApplicationSupportDirectory();
    return rust.coreStart(dataDir: '${support.path}${Platform.pathSeparator}core');
  }

  @override
  // Keep one Rust subscription across page listeners, without replaying messages.
  Stream<rust.ChatEvent> events() => _events ??= rust.events().asBroadcastStream();

  @override
  Future<rust.NetworkInfo> startNetwork() => rust.networkStart();
  @override
  Future<rust.NetworkInfo> networkInfo() => rust.networkInfo();
  @override
  Future<rust.PeerInfo> connectPeer(String target) =>
      rust.connectPeer(target: target);
  @override
  Future<void> sendProbeText(String peerId, String text) =>
      rust.sendProbeText(peerId: peerId, text: text);
  @override
  Future<void> disconnectPeer(String peerId) =>
      rust.disconnectPeer(peerId: peerId);
  @override
  Future<void> stopNetwork() => rust.networkStop();
}
