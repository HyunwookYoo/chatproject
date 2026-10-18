import 'package:flutter/services.dart';

/// Public invitation intake only. Rust still parses and verifies every payload.
class InviteLinks {
  static String? initial;
  static bool isPublicInvite(String value) =>
      value.startsWith('chatproject:identity-invite/');
  static const _channel = MethodChannel('chat_app/account_storage');
  Future<void> start(void Function(String) onInvite) async {
    _channel.setMethodCallHandler((call) async {
      if (call.method == 'identityInvite' &&
          call.arguments is String &&
          isPublicInvite(call.arguments as String)) {
        onInvite(call.arguments as String);
        return true;
      }
    });
    final argument = initial;
    initial = null;
    if (argument != null && isPublicInvite(argument)) onInvite(argument);
    try {
      final payload = await _channel.invokeMethod<String>('takeIdentityInvite');
      if (payload != null && isPublicInvite(payload)) onInvite(payload);
    } on MissingPluginException {
      // Windows public URI intake uses the existing argv path.
    }
  }

  void dispose() => _channel.setMethodCallHandler(null);
}
