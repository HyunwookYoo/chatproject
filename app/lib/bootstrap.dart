import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';

import 'core_client.dart';
import 'main.dart';
import 'network/network_client.dart';
import 'probe/probe_host.dart';
import 'src/rust/api/chat.dart' as rust;
import 'startup_error_app.dart';

Future<Widget> bootstrap(
  List<String> arguments, {
  required Future<void> Function() initializeRust,
  required CoreClient Function(String?) createClient,
}) async {
  try {
    final dataDir = startupDataDir(arguments, releaseMode: kReleaseMode);
    await initializeRust();
    final core = createClient(dataDir);
    await core.start();
    return ChatApp(
      core: core,
      network: core is NetworkClient ? core as NetworkClient : null,
      probe: Platform.isIOS ? IosProbeHost() : null,
    );
  } catch (error, stack) {
    final reason = error is rust.CoreError ? error.code.name : '$error';
    debugPrint('ChatProject startup failed: $reason');
    debugPrintStack(label: 'ChatProject startup failed', stackTrace: stack);
    return StartupErrorApp(reason: reason);
  }
}

/// Parses startup configuration before library loading or core creation.
String? startupDataDir(List<String> arguments, {required bool releaseMode}) {
  String? dataDir;
  for (var i = 0; i < arguments.length; i++) {
    final argument = arguments[i];
    if (releaseMode &&
        [
          '--debug-hooks',
          '--control-url',
          '--name',
        ].any((flag) => argument == flag || argument.startsWith('$flag='))) {
      throw const FormatException(
        'Debug control arguments are unavailable in Release',
      );
    }
    if (argument == '--data-dir') {
      if (++i >= arguments.length) {
        throw const FormatException('--data-dir requires an absolute path');
      }
      dataDir = arguments[i];
    } else if (argument.startsWith('--data-dir=')) {
      dataDir = argument.substring('--data-dir='.length);
    }
  }
  if (dataDir != null &&
      !(Platform.isWindows
          ? RegExp(r'^(?:[A-Za-z]:[\\/]|\\\\[^\\]+\\[^\\]+)').hasMatch(dataDir)
          : dataDir.startsWith('/'))) {
    throw const FormatException('--data-dir requires an absolute path');
  }
  return dataDir;
}
