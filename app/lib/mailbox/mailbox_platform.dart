import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

/// The `chat_app/mailbox` method channel of the Windows runner and Android's
/// MainActivity. Only ids, titles and counts cross it, never message text.
abstract interface class MailboxPlatform {
  /// Android answers false when it could not start the mailbox service.
  Future<bool> setMailbox(bool on);

  /// Windows. Answers whether the Run entry was changed.
  Future<bool> setAutostart(bool on);

  /// Windows.
  Future<bool> autostart();
  Future<void> notify({
    required int id,
    required String title,
    required String body,
  });
  Future<void> cancelNotification(int id);

  /// Android.
  Future<void> requestBatteryExemption();

  /// Android 13+.
  Future<bool> requestNotificationPermission();

  /// Windows: ends the app after an `exitRequested`.
  Future<void> quit();

  /// Calls from the platform: Windows "완전히 종료" and a tapped notification.
  void listen({
    required Future<void> Function() onExitRequested,
    required void Function(int id) onOpenConversation,
  });
}

class ChannelMailboxPlatform implements MailboxPlatform {
  const ChannelMailboxPlatform();
  static const _channel = MethodChannel('chat_app/mailbox');

  /// Null where the platform has no such method (iOS has no channel at all).
  Future<T?> _call<T>(String method, [Object? arguments]) async {
    try {
      return await _channel.invokeMethod<T>(method, arguments);
    } on MissingPluginException {
      return null;
    } on PlatformException catch (error) {
      debugPrint('ChatProject mailbox call $method failed: ${error.code}');
      return null;
    }
  }

  @override
  Future<bool> setMailbox(bool on) async =>
      await _call<bool>('setMailbox', on) ?? false;
  @override
  Future<bool> setAutostart(bool on) async =>
      await _call<bool>('setAutostart', on) ?? false;
  @override
  Future<bool> autostart() async => await _call<bool>('autostart') ?? false;
  @override
  Future<void> notify({
    required int id,
    required String title,
    required String body,
  }) => _call<void>('notify', {'id': id, 'title': title, 'body': body});
  @override
  Future<void> cancelNotification(int id) =>
      _call<void>('cancelNotification', id);
  @override
  Future<void> requestBatteryExemption() =>
      _call<void>('requestBatteryExemption');
  @override
  Future<bool> requestNotificationPermission() async =>
      await _call<bool>('requestNotificationPermission') ?? false;
  @override
  Future<void> quit() => _call<void>('quit');
  @override
  void listen({
    required Future<void> Function() onExitRequested,
    required void Function(int id) onOpenConversation,
  }) {
    _channel.setMethodCallHandler((call) async {
      switch (call.method) {
        case 'exitRequested':
          await onExitRequested();
        case 'openConversation':
          // Untrusted (Android's MainActivity is exported): only an int, and
          // the receiver looks it up.
          if (call.arguments case final int id) onOpenConversation(id);
        default:
          throw MissingPluginException();
      }
      return null;
    });
  }
}
