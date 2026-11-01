import 'dart:io';
import 'dart:convert';
import 'dart:isolate';

import 'package:path/path.dart' as p;

/// Paths of the Android SDK tools the device tool drives.
class AndroidSdk {
  AndroidSdk(this.root, {required this.windows});

  /// Finds the SDK from ANDROID_HOME, ANDROID_SDK_ROOT, or the default install folder.
  factory AndroidSdk.locate({Map<String, String>? environment, bool? windows}) {
    final env = environment ?? Platform.environment;
    final onWindows = windows ?? Platform.isWindows;
    final fromEnv = env['ANDROID_HOME'] ?? env['ANDROID_SDK_ROOT'];
    if (fromEnv != null && fromEnv.isNotEmpty) return AndroidSdk(fromEnv, windows: onWindows);
    final base = onWindows ? env['LOCALAPPDATA'] : env['HOME'];
    if (base == null) throw StateError('Android SDK not found. Set ANDROID_HOME.');
    return AndroidSdk(p.join(base, 'Android', 'Sdk'), windows: onWindows);
  }

  final String root;
  final bool windows;

  String get adb => p.join(root, 'platform-tools', windows ? 'adb.exe' : 'adb');
  String get emulator => p.join(root, 'emulator', windows ? 'emulator.exe' : 'emulator');
  String get avdmanager => p.join(root, 'cmdline-tools', 'latest', 'bin', windows ? 'avdmanager.bat' : 'avdmanager');
  String systemImage(AvdConfig config) => p.join(root, 'system-images', 'android-${config.api}', config.flavor, config.abi);
}

/// The checked-in image selection is shared by creation and config validation.
class AvdConfig {
  AvdConfig({required this.api, required this.flavor, required this.abi});

  factory AvdConfig.load({bool? ci}) {
    final source = Isolate.resolvePackageUriSync(Uri.parse('package:chat_e2e/src/android_sdk.dart'))!;
    final file = File.fromUri(source.resolve('../../avd_config.json'));
    final profile = (ci ?? Platform.environment['CI'] == 'true') ? 'ci' : 'local';
    final data = (jsonDecode(file.readAsStringSync()) as Map<String, dynamic>)[profile] as Map<String, dynamic>;
    return AvdConfig(api: data['api'] as int, flavor: data['flavor'] as String, abi: data['abi'] as String);
  }

  final int api;
  final String flavor;
  final String abi;
  String get package => 'system-images;android-$api;$flavor;$abi';
}

/// Folder that holds `<name>.avd` folders: ANDROID_AVD_HOME, ANDROID_USER_HOME/avd,
/// or `~/.android/avd`.
String defaultAvdHome({Map<String, String>? environment, bool? windows}) {
  final env = environment ?? Platform.environment;
  final avdHome = env['ANDROID_AVD_HOME'];
  if (avdHome != null && avdHome.isNotEmpty) return avdHome;
  final userHome = env['ANDROID_USER_HOME'];
  if (userHome != null && userHome.isNotEmpty) return p.join(userHome, 'avd');
  final home = (windows ?? Platform.isWindows) ? env['USERPROFILE'] : env['HOME'];
  if (home == null) throw StateError('Cannot find the home folder for AVDs. Set ANDROID_AVD_HOME.');
  return p.join(home, '.android', 'avd');
}
