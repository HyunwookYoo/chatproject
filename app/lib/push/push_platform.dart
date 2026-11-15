import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';

/// The OS notification permission as the app sees it.
enum NotificationPermission {
  granted,
  denied,

  /// The app has not asked yet (Android: no `notification_permission_asked`
  /// mark; iOS: `UNAuthorizationStatus.notDetermined` and not put off).
  notDetermined,
}

/// The platform side of push (M7 spec 5.1, 5.6, 8): Android's
/// `chat_app/mailbox`, iOS's `chat_app/push`. Only tokens, tags and ids cross
/// it, never message text.
abstract interface class PushHost {
  Future<NotificationPermission> permission();

  /// Prompts only while not asked yet; later calls answer the current state.
  Future<bool> requestPermission();

  /// "나중에": not asked again.
  Future<void> markAsked();

  /// The OS notification settings of this app.
  Future<void> openSettings();

  /// The current FCM (Android) or APNs (iOS, hex) token, if there is one.
  Future<String?> token();

  /// iOS: the App Group folder.
  Future<String?> appGroupDir();

  /// iOS: the `t` of a notification tapped before Dart listened, once.
  Future<String?> takeOpenedTag();

  /// Android: the notification id of the conversation on screen, or null.
  Future<void> setOpenConversation(int? id);

  /// iOS calls: a new APNs token, and a tapped notification's `t` (answer
  /// true when taken).
  void listen({
    required void Function(String token) onToken,
    required Future<bool> Function(String tag) onOpened,
  });
}

class ChannelPushHost implements PushHost {
  const ChannelPushHost();
  static const _android = MethodChannel('chat_app/mailbox');
  static const _ios = MethodChannel('chat_app/push');

  static bool get _isAndroid => defaultTargetPlatform == TargetPlatform.android;
  static bool get _isIos => defaultTargetPlatform == TargetPlatform.iOS;

  /// Null on platforms without push receiving, or where the method is missing.
  Future<T?> _call<T>(String android, String ios, [Object? arguments]) async {
    final channel = _isAndroid
        ? _android
        : _isIos
        ? _ios
        : null;
    if (channel == null) return null;
    final method = _isAndroid ? android : ios;
    try {
      return await channel.invokeMethod<T>(method, arguments);
    } on MissingPluginException {
      return null;
    } on PlatformException catch (error) {
      debugPrint('ChatProject push call $method failed: ${error.code}');
      return null;
    }
  }

  @override
  Future<NotificationPermission> permission() async =>
      switch (await _call<String>(
        'notificationPermission',
        'permissionStatus',
      )) {
        'granted' => NotificationPermission.granted,
        'notDetermined' => NotificationPermission.notDetermined,
        _ => NotificationPermission.denied,
      };
  @override
  Future<bool> requestPermission() async =>
      await _call<bool>('requestNotificationPermission', 'requestPermission') ??
      false;
  @override
  Future<void> markAsked() =>
      _call<void>('markNotificationPermissionAsked', 'markPermissionAsked');
  @override
  Future<void> openSettings() =>
      _call<void>('openNotificationSettings', 'openNotificationSettings');
  @override
  Future<String?> token() => _call<String>('pushToken', 'token');
  @override
  Future<String?> appGroupDir() =>
      _isIos ? _call<String>('', 'appGroupDir') : Future.value();
  @override
  Future<String?> takeOpenedTag() =>
      _isIos ? _call<String>('', 'takeOpenedTag') : Future.value();
  @override
  Future<void> setOpenConversation(int? id) =>
      _isAndroid ? _call<void>('setOpenConversation', '', id) : Future.value();
  @override
  void listen({
    required void Function(String token) onToken,
    required Future<bool> Function(String tag) onOpened,
  }) {
    // Android's taps arrive on `chat_app/mailbox` (MailboxPlatform.listen).
    if (!_isIos) return;
    _ios.setMethodCallHandler((call) async {
      switch (call.method) {
        // Untrusted: only strings, and the receiver checks them.
        case 'tokenChanged':
          if (call.arguments case final String token) onToken(token);
          return null;
        case 'notificationOpened':
          if (call.arguments case final String tag) return onOpened(tag);
          return false;
        default:
          throw MissingPluginException();
      }
    });
  }
}
