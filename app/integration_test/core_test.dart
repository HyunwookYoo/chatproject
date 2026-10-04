import 'dart:io';

import 'package:chat_app/src/rust/api/chat.dart';
import 'package:chat_app/src/rust/frb_generated.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  late Directory dataDir;

  setUpAll(() async {
    await RustLib.init();
    dataDir = await Directory.systemTemp.createTemp('chat_core_it_');
  });

  test('starting twice returns the same core', () async {
    final first = await coreStart(dataDir: dataDir.path);
    final again = await coreStart(dataDir: dataDir.path);
    expect(again.instanceId, first.instanceId);
    expect(first.version, coreVersion());
  });

  test('a late listener still gets the connection state', () async {
    await coreStart(dataDir: dataDir.path);
    final event = await events().first.timeout(const Duration(seconds: 5));
    expect(event, isA<ChatEvent_ConnectionState>());
    expect((event as ChatEvent_ConnectionState).directPeers, 0);
  });

  test('another data folder is refused while the core runs', () async {
    await coreStart(dataDir: dataDir.path);
    final other = await Directory.systemTemp.createTemp('chat_core_other_');
    await expectLater(
      coreStart(dataDir: other.path),
      throwsA(isA<CoreError>().having((e) => e.code, 'code', ErrorCode.alreadyStartedDifferentConfig)),
    );
  });
}
