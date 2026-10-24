import 'dart:io';
import 'package:chat_app/core_client.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  test(
    'Windows fresh account uses LocalAppData and preserves incomplete legacy or SQL state',
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
          'db.sqlite3',
          'db.sqlite3-wal',
          'db.sqlite3-shm',
          'db.keys',
          'db.keys.pending',
          '.db.keys.tmp',
          '.db.keys.pending.tmp',
          'storage.mode',
          '.storage.mode.tmp',
          'migration.prep',
          '.migration.prep.tmp',
          'migration.checkpoint',
          '.migration.checkpoint.tmp',
        ]) {
          final artifact = File('${directory.path}/$name');
          await artifact.writeAsBytes([1]);
          try {
            expect(
              await accountStorageRoot(directory.path),
              directory.path,
              reason: 'Preserve existing path for $name',
            );
          } finally {
            await artifact.delete();
          }
        }
      } finally {
        await directory.delete();
      }
    },
    skip: !Platform.isWindows,
  );
}
