import 'dart:typed_data';

import 'package:chat_app/push/push_client.dart';
import 'package:chat_app/push/push_platform.dart';
import 'package:chat_app/src/rust/api/chat.dart' as r;
import 'package:chat_app/src/rust/api/push.dart' as p;

/// The core's push calls, recorded in call order.
class FakePushClient implements PushClient {
  bool enabled = true;
  r.ErrorCode? failure;
  final calls = <String>[];
  final tags = <List<int>>[];

  /// What `conversationOf` answers.
  Uint8List? conversation;

  void _check() {
    if (failure != null) {
      throw r.CoreError(code: failure!, detail: 'fixture failure');
    }
  }

  @override
  Future<void> configure({
    String? proxyUrl,
    p.PushPlatform? platform,
    String? snapshotDir,
  }) async {
    calls.add('configure:$proxyUrl:${platform?.name}:$snapshotDir');
    _check();
  }

  @override
  Future<void> setToken(p.PushPlatform platform, String token) async {
    calls.add('setToken:${platform.name}:$token');
    _check();
  }

  @override
  Future<void> setEnabled(bool enabled) async {
    calls.add('setEnabled:$enabled');
    _check();
    this.enabled = enabled;
  }

  @override
  Future<p.PushStatusView> status() async {
    _check();
    return p.PushStatusView(enabled: enabled, handles: 0);
  }

  @override
  Future<Uint8List?> conversationOf(List<int> tag) async {
    tags.add(List.of(tag));
    _check();
    return conversation;
  }

  @override
  Future<int> ingestInbox(String dir) async {
    calls.add('ingest:$dir');
    _check();
    return 0;
  }
}

/// The platform's notification permission, token and taps, recorded.
class FakePushHost implements PushHost {
  NotificationPermission state = NotificationPermission.notDetermined;

  /// What the OS prompt answers.
  bool grant = true;
  String? currentToken = 'TOKEN';
  String? groupDir = '/group';
  String? openedTag;
  final calls = <String>[];
  void Function(String)? onToken;
  Future<bool> Function(String)? onOpened;

  @override
  Future<NotificationPermission> permission() async => state;

  @override
  Future<bool> requestPermission() async {
    calls.add('requestPermission');
    if (state == NotificationPermission.notDetermined) {
      state = grant
          ? NotificationPermission.granted
          : NotificationPermission.denied;
    }
    return state == NotificationPermission.granted;
  }

  @override
  Future<void> markAsked() async {
    calls.add('markAsked');
    if (state == NotificationPermission.notDetermined) {
      state = NotificationPermission.denied;
    }
  }

  @override
  Future<void> openSettings() async => calls.add('openSettings');

  @override
  Future<String?> token() async => currentToken;

  @override
  Future<String?> appGroupDir() async => groupDir;

  @override
  Future<String?> takeOpenedTag() async {
    final tag = openedTag;
    openedTag = null;
    return tag;
  }

  @override
  Future<void> setOpenConversation(int? id) async =>
      calls.add('setOpenConversation:$id');

  @override
  void listen({
    required void Function(String token) onToken,
    required Future<bool> Function(String tag) onOpened,
  }) {
    this.onToken = onToken;
    this.onOpened = onOpened;
  }
}
