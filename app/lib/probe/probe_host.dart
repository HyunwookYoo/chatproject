import 'dart:io';

import 'package:flutter/services.dart';
import 'package:path_provider_foundation/path_provider_foundation.dart';

/// What the M1b probe screen needs from the platform. iOS only; widget tests use a fake.
abstract class ProbeHost {
  /// Asks for notification permission, then registers for a device token.
  /// Returns whether notifications are allowed.
  Future<bool> requestPush();

  /// Hex APNs device token, or null until iOS has delivered one.
  Future<String?> deviceToken();

  /// The NSE log (`nse_memory.jsonl`), or null when it does not exist yet.
  Future<String?> readNseLog();

  Future<void> clearNseLog();
}

class IosProbeHost implements ProbeHost {
  static const appGroup = 'group.dev.chatproject.chatapp';
  static const _channel = MethodChannel('chat_app/ios_probe');

  @override
  Future<bool> requestPush() async => await _channel.invokeMethod<bool>('requestPush') ?? false;

  @override
  Future<String?> deviceToken() => _channel.invokeMethod<String>('deviceToken');

  Future<File?> _log() async {
    final dir = await PathProviderFoundation().getContainerPath(appGroupIdentifier: appGroup);
    return dir == null ? null : File('$dir/nse_memory.jsonl');
  }

  @override
  Future<String?> readNseLog() async {
    final file = await _log();
    if (file == null || !await file.exists()) return null;
    return file.readAsString();
  }

  @override
  Future<void> clearNseLog() async {
    final file = await _log();
    if (file != null && await file.exists()) await file.delete();
  }
}
