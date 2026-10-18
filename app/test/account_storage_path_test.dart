import 'dart:io';
import 'package:chat_app/core_client.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test(
    'Windows fresh account uses LocalAppData and preserves incomplete legacy state',
    () async {
      final directory = await Directory.systemTemp.createTemp(
        'account-path-test',
      );
      try {
        expect(
          await accountStorageRoot(directory.path),
          '${Platform.environment['LOCALAPPDATA']}\\ChatProject\\account',
        );
        for (final name in [
          'store.id',
          'key.wrap',
          'account.bin',
          '.store.id.tmp',
          '.key.wrap.tmp',
          '.account.bin.tmp',
        ]) {
          final artifact = File('${directory.path}/$name');
          await artifact.writeAsBytes([1]);
          expect(await accountStorageRoot(directory.path), directory.path);
          await artifact.delete();
        }
      } finally {
        await directory.delete();
      }
    },
    skip: !Platform.isWindows,
  );
}
