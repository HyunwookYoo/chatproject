import 'dart:io';

import 'package:chat_app/src/rust/api/chat.dart';
import 'package:chat_app/src/rust/frb_generated.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  setUpAll(() async => RustLib.init());

  test('Kotlin reaches the same Rust core as Dart', () async {
    final dataDir = await Directory.systemTemp.createTemp('chat_native_it_');
    final info = await coreStart(dataDir: dataDir.path);
    const channel = MethodChannel('chat_app/native_core');
    final fromKotlin = await channel.invokeMethod<String>('instanceId');
    expect(fromKotlin, info.instanceId);
  }, skip: !Platform.isAndroid);
}
