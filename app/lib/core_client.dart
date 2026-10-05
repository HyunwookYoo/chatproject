import 'dart:io';

import 'package:path_provider/path_provider.dart';

import 'src/rust/api/chat.dart' as rust;

/// The UI reaches the Rust core only through this interface, so widget tests can use
/// a fake core.
abstract interface class CoreClient {
  String get version;
  Future<rust.CoreInfo> start();
  Stream<rust.ChatEvent> events();
}

class FrbCoreClient implements CoreClient {
  @override
  String get version => rust.coreVersion();

  @override
  Future<rust.CoreInfo> start() async {
    final support = await getApplicationSupportDirectory();
    return rust.coreStart(dataDir: '${support.path}${Platform.pathSeparator}core');
  }

  @override
  Stream<rust.ChatEvent> events() => rust.events();
}
