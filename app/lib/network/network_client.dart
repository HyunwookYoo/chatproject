import '../src/rust/api/chat.dart' as rust;

abstract interface class NetworkClient {
  Future<rust.NetworkInfo> startNetwork();
  Future<rust.NetworkInfo> networkInfo();
  Future<rust.PeerInfo> connectPeer(String target);
  Future<void> sendProbeText(String peerId, String text);
  Future<void> disconnectPeer(String peerId);
  Future<void> stopNetwork();
  Stream<rust.ChatEvent> events();
}
