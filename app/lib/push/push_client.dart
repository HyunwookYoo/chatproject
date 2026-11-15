import 'dart:typed_data';

import '../src/rust/api/push.dart' as rust;

/// The core's push calls (M7 spec 5, 8), behind an interface for widget tests.
abstract interface class PushClient {
  Future<void> configure({
    String? proxyUrl,
    rust.PushPlatform? platform,
    String? snapshotDir,
  });
  Future<void> setToken(rust.PushPlatform platform, String token);
  Future<void> setEnabled(bool enabled);
  Future<rust.PushStatusView> status();
  Future<Uint8List?> conversationOf(List<int> tag);
  Future<int> ingestInbox(String dir);
}

class FrbPushClient implements PushClient {
  const FrbPushClient();
  @override
  Future<void> configure({
    String? proxyUrl,
    rust.PushPlatform? platform,
    String? snapshotDir,
  }) => rust.pushConfigure(
    proxyUrl: proxyUrl,
    platform: platform,
    snapshotDir: snapshotDir,
  );
  @override
  Future<void> setToken(rust.PushPlatform platform, String token) =>
      rust.pushSetToken(platform: platform, token: token);
  @override
  Future<void> setEnabled(bool enabled) =>
      rust.pushSetEnabled(enabled: enabled);
  @override
  Future<rust.PushStatusView> status() => rust.pushStatus();
  @override
  Future<Uint8List?> conversationOf(List<int> tag) =>
      rust.pushConversationOf(tag: tag);
  @override
  Future<int> ingestInbox(String dir) => rust.pushIngestInbox(dir: dir);
}
