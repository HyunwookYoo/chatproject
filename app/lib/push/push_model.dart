import 'dart:async';
import 'dart:convert';

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';

import '../mailbox/notifier.dart' show notificationId;
import '../src/rust/api/chat.dart' as rust;
import '../src/rust/api/push.dart' show PushPlatform;
import 'push_client.dart';
import 'push_platform.dart';

/// This device's push setting (M7 spec 4, 8) and the app's side of the
/// platform hand-over: configuration, OS permission, token, the iOS inbox and
/// tapped notifications.
class PushModel extends ChangeNotifier {
  PushModel(this.client, this.host, {this.proxyUrl}) {
    if (!supported) return;
    host.listen(onToken: _tokenChanged, onOpened: _opened);
    unawaited(_queue(_readPermission));
  }

  final PushClient client;
  final PushHost host;

  /// Debug `--push-url`; null uses the build's address.
  final String? proxyUrl;

  /// Opens the conversation (its id) of a tapped iOS notification.
  ValueChanged<List<int>>? onOpen;

  NotificationPermission? permission;
  bool loaded = false;
  bool enabled = false;
  bool busy = false;
  String? error;

  bool _active = false;
  bool _closed = false;
  Future<void>? _starting;
  String? _groupDir;

  /// Every step runs after the one before, so a resume never interleaves with
  /// a setting change.
  Future<void> _tail = Future.value();

  static bool get android => defaultTargetPlatform == TargetPlatform.android;
  static bool get ios => defaultTargetPlatform == TargetPlatform.iOS;

  /// Receives pushes and has a notification permission. Windows only sends.
  static bool get supported => android || ios;

  static PushPlatform? get _platform => android
      ? PushPlatform.fcm
      : ios
      ? PushPlatform.apns
      : null;

  /// The notification question is due: the platform has not asked yet.
  bool get askDue =>
      supported && permission == NotificationPermission.notDetermined;

  /// The account is Active: at every app start, once.
  Future<void> start() => _starting ??= _queue(() async {
    _active = true;
    if (ios) _groupDir = await host.appGroupDir();
    await _core(
      () => client.configure(
        proxyUrl: proxyUrl,
        platform: _platform,
        snapshotDir: _groupDir,
      ),
    );
    if (!supported) return;
    await _sync();
    if (!ios) return;
    await _ingest();
    if (await host.takeOpenedTag() case final String tag) await _openTag(tag);
  });

  void lifecycle(AppLifecycleState state) {
    if (state != AppLifecycleState.resumed || !supported) return;
    unawaited(
      _queue(() async {
        if (!_active) return _readPermission();
        // The permission can change in the OS settings meanwhile (spec 8).
        await _sync();
        if (ios) await _ingest();
      }),
    );
  }

  /// The permission screen's "알림 켜기": push follows the answer.
  Future<void> allow() => _busy(() async {
    final granted = await host.requestPermission();
    permission = await host.permission();
    if (!_active) return; // start() applies it.
    await _core(() => client.setEnabled(granted));
    if (granted) await _handToken();
    await _status();
  });

  /// The permission screen's "나중에": not asked again, push stays off.
  Future<void> later() => _busy(() async {
    await host.markAsked();
    permission = await host.permission();
    if (!_active) return;
    await _core(() => client.setEnabled(false));
    await _status();
  });

  /// The settings switch. On without OS permission asks again, or opens the
  /// OS settings when the OS no longer asks.
  Future<void> setEnabled(bool on) => _busy(() async {
    if (on) {
      permission = await host.permission();
      if (permission != NotificationPermission.granted) {
        final granted = await host.requestPermission();
        permission = await host.permission();
        if (!granted) return host.openSettings();
      }
    }
    try {
      await client.setEnabled(on);
    } catch (failure) {
      _failed(failure);
      return;
    }
    enabled = on;
    if (on) await _handToken();
  });

  /// Android: the native notifier leaves the conversation on screen out
  /// (spec 5.5); MainActivity applies it only while resumed.
  void conversationShown(List<int>? conversation) {
    if (!android) return;
    unawaited(
      host.setOpenConversation(
        conversation == null ? null : notificationId(conversation),
      ),
    );
  }

  Future<void> _queue(Future<void> Function() step) => _tail = _tail
      .then((_) => _closed ? null : step())
      .catchError((Object failure) => _log(failure));

  Future<void> _busy(Future<void> Function() operation) async {
    if (_closed || busy) return;
    busy = true;
    error = null;
    _changed();
    try {
      await _queue(operation);
    } finally {
      busy = false;
      _changed();
    }
  }

  Future<void> _readPermission() async {
    permission = await host.permission();
    _changed();
  }

  /// Without OS permission push is off (spec 8); with it the core gets the
  /// current token (it ignores one it has, and one while push is off).
  Future<void> _sync() async {
    permission = await host.permission();
    if (permission == NotificationPermission.granted) {
      await _handToken();
    } else {
      await _core(() => client.setEnabled(false));
    }
    await _status();
  }

  Future<void> _status() async {
    try {
      enabled = (await client.status()).enabled;
      loaded = true;
    } catch (failure) {
      _log(failure);
    }
    _changed();
  }

  Future<void> _handToken() async {
    final platform = _platform;
    final token = await host.token();
    if (platform == null || token == null || token.isEmpty) return;
    await _core(() => client.setToken(platform, token));
  }

  void _tokenChanged(String token) {
    // Before start, start() asks for the token itself.
    if (!_active || !ios || token.isEmpty) return;
    unawaited(
      _queue(() => _core(() => client.setToken(PushPlatform.apns, token))),
    );
  }

  Future<void> _ingest() async {
    final dir = _groupDir;
    if (dir == null) return;
    await _core(() => client.ingestInbox('$dir/push'));
  }

  /// A tapped iOS notification. Not taken before start: iOS keeps it for
  /// takeOpenedTag.
  Future<bool> _opened(String tag) async {
    if (!_active) return false;
    await _openTag(tag);
    return true;
  }

  /// [tag] comes from the notification and is untrusted: anything but 16
  /// base64url bytes, or a tag the core does not know, opens nothing.
  Future<void> _openTag(String tag) async {
    final List<int> bytes;
    try {
      bytes = base64Url.decode(base64Url.normalize(tag));
    } on FormatException {
      return;
    }
    if (bytes.length != 16) return;
    try {
      final conversation = await client.conversationOf(bytes);
      if (conversation != null && !_closed) onOpen?.call(conversation);
    } catch (failure) {
      _log(failure);
    }
  }

  Future<void> _core(Future<void> Function() call) async {
    try {
      await call();
    } catch (failure) {
      _log(failure);
    }
  }

  void _failed(Object failure) {
    _log(failure);
    error = '알림 설정을 바꾸지 못했어요. 잠시 뒤 다시 시도해 주세요.';
  }

  void _log(Object failure) => debugPrint(
    'ChatProject push operation failed: '
    '${failure is rust.CoreError ? failure.code.name : failure.runtimeType}',
  );

  void _changed() {
    if (!_closed) notifyListeners();
  }

  @override
  void dispose() {
    _closed = true;
    super.dispose();
  }
}
