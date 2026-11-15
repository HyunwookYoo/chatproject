// iOS only (CI job `ios`): the App Group folder the notification extension shares with the app
// (M7 spec 5.6, Task 11). Other platforms have no `chat_app/push` channel and skip these tests;
// the Android and Windows jobs run every integration file, so this one passes there skipped.
import 'dart:convert';
import 'dart:io';
import 'dart:math';

import 'package:chat_app/account/account_model.dart';
import 'package:chat_app/core_client.dart';
import 'package:chat_app/src/rust/api/account.dart' as account;
import 'package:chat_app/src/rust/api/push.dart' as push;
import 'package:chat_app/src/rust/frb_generated.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

const _channel = MethodChannel('chat_app/push');

Future<Directory> _pushDir() async {
  final group = await _channel.invokeMethod<String>('appGroupDir');
  expect(group, isNotNull);
  return Directory('$group/push')..createSync(recursive: true);
}

String _b64(List<int> bytes) => base64Url.encode(bytes).replaceAll('=', '');

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  // Files in the folder (the core's state files, the extension's inbox) get the app's default
  // class, which the extension can read on a locked phone after the first unlock: never None.
  // The core and the extension create them with open(2) like this file, without a class of
  // their own.
  testWidgets('push_dir_files_are_protected_until_first_unlock', (tester) async {
    final dir = await _pushDir();
    final file = File('${dir.path}/integration-test.state')..writeAsBytesSync([1, 2, 3]);
    try {
      final protection = await _channel.invokeMethod<String>('fileProtection', file.path);
      expect(protection, isNotNull, reason: 'the file attributes are readable');
      expect(protection, isNot('NSFileProtectionNone'));
    } finally {
      file.deleteSync();
    }
  }, skip: !Platform.isIOS);

  // What the extension appends is taken by the ingest the app runs at launch and on every
  // return to the foreground (Task 12 calls it there), and the inbox is gone afterwards.
  testWidgets('inbox_is_ingested_and_cleared_on_resume', (tester) async {
    await RustLib.init();
    final root = await Directory.systemTemp.createTemp('chat-push-ios-');
    await account.testStorageStart(dataDir: root.path);
    final core = FrbCoreClient(dataDir: root.path);
    final model = AccountModel(core.account);
    await model.open();
    final display = await model.beginCreate();
    expect(display, isNotNull);
    final answers = display!.positions.map((p) => display.words[p - 1]).toList();
    expect(await model.confirmCreate(display.flow, answers), true);

    final dir = await _pushDir();
    final inbox = File('${dir.path}/inbox.jsonl');
    final random = Random.secure();
    final tag = Uint8List.fromList(List.generate(16, (_) => random.nextInt(256)));
    // A tag of no conversation here: taken and skipped, as a withdrawn one would be.
    inbox.writeAsStringSync('{"t":"${_b64(tag)}","e":"${_b64([1, 2, 3, 4])}"}\n');
    expect(await push.pushIngestInbox(dir: dir.path), 0);
    expect(inbox.existsSync(), false);
    expect(File('${dir.path}/inbox.taken.jsonl').existsSync(), false);
    await model.close();
  }, skip: !Platform.isIOS);
}
