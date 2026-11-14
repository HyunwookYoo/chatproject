import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';

import '../src/rust/api/chat.dart' as rust;
import 'mailbox_client.dart';
import 'mailbox_platform.dart';

/// This device's mailbox setting (M6 spec 4, 8) and the app's foreground and
/// background hooks into the core.
class MailboxModel extends ChangeNotifier {
  MailboxModel(this.client, this.platform);
  final MailboxClient client;
  final MailboxPlatform platform;

  bool loaded = false;
  bool enabled = false;

  /// Windows: starts in the tray at login.
  bool autostart = false;
  bool firstLinkShown = true;
  bool busy = false;
  String? error;

  bool _active = false;
  bool _background = false;
  bool _closed = false;

  /// Android was told to be a mailbox while it could not start one.
  bool _platformPending = false;
  Future<void>? _loading;

  static bool get windows => defaultTargetPlatform == TargetPlatform.windows;
  static bool get android => defaultTargetPlatform == TargetPlatform.android;

  /// iOS cannot be a mailbox (spec 8.3).
  static bool get supported => windows || android;

  /// Windows shows the first-link screen once (spec 8.1).
  bool get firstLinkDue => windows && loaded && !firstLinkShown;

  /// The account is Active: load the setting once and tell the platform.
  Future<void> accountActive() {
    _active = true;
    return _loading ??= _load();
  }

  Future<void> _load() async {
    final MailboxStateView state;
    try {
      state = await client.mailboxState();
    } catch (failure) {
      _loading = null;
      _failed(failure);
      return;
    }
    if (_closed) return;
    enabled = state.enabled;
    firstLinkShown = state.firstLinkShown;
    if (windows) autostart = await platform.autostart();
    loaded = true;
    _changed();
    if (!supported) return;
    // Every start, so a restarted Android app restores its service and a
    // Windows --tray start learns the setting.
    _platformPending = !await platform.setMailbox(enabled) && enabled;
    // Android prompts only on the first run with an active account; it
    // remembers that it asked.
    if (android) await platform.requestNotificationPermission();
  }

  void lifecycle(AppLifecycleState state) {
    switch (state) {
      case AppLifecycleState.resumed:
        _background = false;
        if (_platformPending) {
          _platformPending = false;
          unawaited(_restorePlatform());
        }
        if (_active) unawaited(_core(client.mailboxForeground));
      case AppLifecycleState.hidden || AppLifecycleState.paused:
        if (_active && !_background) {
          _background = true;
          unawaited(_core(client.mailboxBackground));
        }
      case AppLifecycleState.inactive || AppLifecycleState.detached:
        break;
    }
  }

  Future<void> _restorePlatform() async {
    if (!enabled) return;
    _platformPending = !await platform.setMailbox(true) && enabled;
  }

  /// Windows "완전히 종료": hand off for at most 2 s, then quit.
  Future<void> exit() async {
    if (_active) {
      try {
        await _core(
          client.mailboxBackground,
        ).timeout(const Duration(seconds: 2));
      } on TimeoutException {
        debugPrint('ChatProject mailbox hand-off timed out at exit');
      }
    }
    await platform.quit();
  }

  Future<void> setEnabled(bool on) => _busy(() => _setEnabled(on));
  Future<void> setAutostart(bool on) => _busy(() => _setAutostart(on));

  /// The first-link screen's OK: applies both choices; it is not shown again.
  Future<bool> confirmFirstLink({
    required bool mailbox,
    required bool autostart,
  }) => _busy(() async {
    await _setEnabled(mailbox);
    await _setAutostart(autostart);
    try {
      await client.markFirstLinkShown();
    } catch (failure) {
      _failed(failure);
    }
    firstLinkShown = true;
  });

  Future<bool> _busy(Future<void> Function() operation) async {
    if (_closed || busy || !loaded) return false;
    busy = true;
    error = null;
    _changed();
    try {
      await operation();
    } finally {
      busy = false;
      _changed();
    }
    return true;
  }

  Future<void> _setEnabled(bool on) async {
    try {
      await client.setMailbox(on);
    } catch (failure) {
      _failed(failure);
      return;
    }
    enabled = on;
    if (!supported) return;
    if (await platform.setMailbox(on)) {
      _platformPending = false;
      if (on && android) await platform.requestBatteryExemption();
      return;
    }
    // Android starts the service only while the app is on screen (spec 8.2).
    error = '메일박스를 켜지 못했어요. 앱을 화면에 연 채로 다시 시도해 주세요.';
    try {
      await client.setMailbox(false);
      enabled = false;
    } catch (failure) {
      _platformPending = true;
      _failed(failure);
    }
  }

  Future<void> _setAutostart(bool on) async {
    if (!windows) return;
    autostart = await platform.setAutostart(on)
        ? on
        : await platform.autostart();
  }

  Future<void> _core(Future<Object?> Function() call) async {
    try {
      await call();
    } catch (failure) {
      _log(failure);
    }
  }

  void _failed(Object failure) {
    _log(failure);
    error = '메일박스 설정을 바꾸지 못했어요. 잠시 뒤 다시 시도해 주세요.';
    _changed();
  }

  void _log(Object failure) => debugPrint(
    'ChatProject mailbox operation failed: '
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
