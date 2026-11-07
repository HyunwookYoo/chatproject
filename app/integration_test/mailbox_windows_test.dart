import 'dart:io';

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

const _channel = MethodChannel('chat_app/mailbox');
const _runKey = r'HKCU\Software\Microsoft\Windows\CurrentVersion\Run';
const _settingsKey = r'HKCU\Software\ChatProject';

/// The `ChatProject` Run value as Windows stores it, read without the runner.
Future<String?> _runValue() async {
  final result = await Process.run('reg', [
    'query',
    _runKey,
    '/v',
    'ChatProject',
  ]);
  if (result.exitCode != 0) return null;
  final match = RegExp(
    r'^\s*ChatProject\s+REG_SZ\s+(.*?)\s*$',
    multiLine: true,
  ).firstMatch(result.stdout as String);
  return match?.group(1);
}

Future<void> _restoreRunValue(String? value) async {
  final arguments = value == null
      ? ['delete', _runKey, '/v', 'ChatProject', '/f']
      : [
          'add',
          _runKey,
          '/v',
          'ChatProject',
          '/t',
          'REG_SZ',
          '/d',
          value,
          '/f',
        ];
  await Process.run('reg', arguments);
}

/// The runner's remembered setMailbox value, read without the runner: whether
/// its key exists and the `Mailbox` DWORD (null when absent).
Future<({bool key, int? mailbox})> _rememberedValue() async {
  final key = await Process.run('reg', ['query', _settingsKey]);
  if (key.exitCode != 0) return (key: false, mailbox: null);
  final match = RegExp(
    r'^\s*Mailbox\s+REG_DWORD\s+0x([0-9a-fA-F]+)\s*$',
    multiLine: true,
  ).firstMatch(key.stdout as String);
  return (
    key: true,
    mailbox: match == null ? null : int.parse(match.group(1)!, radix: 16),
  );
}

Future<void> _restoreRemembered(({bool key, int? mailbox}) value) async {
  if (!value.key) {
    await Process.run('reg', ['delete', _settingsKey, '/f']);
  } else if (value.mailbox == null) {
    await Process.run('reg', ['delete', _settingsKey, '/v', 'Mailbox', '/f']);
  } else {
    await Process.run('reg', [
      'add',
      _settingsKey,
      '/v',
      'Mailbox',
      '/t',
      'REG_DWORD',
      '/d',
      '${value.mailbox}',
      '/f',
    ]);
  }
}

Future<Map<String, dynamic>> _probe([
  Map<String, Object?> arguments = const {},
]) async => (await _channel.invokeMapMethod<String, dynamic>(
  'mailboxTest',
  arguments,
))!;

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('tray, power request, close to tray and autostart', (
    tester,
  ) async {
    // Leave this machine as it was: no tray icon, the Run value and the
    // remembered mailbox setting it had before.
    final original = await _runValue();
    final remembered = await _rememberedValue();
    addTearDown(() async {
      await _channel.invokeMethod<bool>('setMailbox', false);
      await _restoreRunValue(original);
      await _restoreRemembered(remembered);
      expect(await _runValue(), original);
      final restored = await _rememberedValue();
      expect(
        (restored.key, restored.mailbox),
        (remembered.key, remembered.mailbox),
      );
    });

    expect(await _channel.invokeMethod<bool>('setMailbox', true), isTrue);
    var state = await _probe();
    expect(state['tray'], isTrue);
    // A --tray start reads this before Dart reports the setting.
    expect(state['remembered'], isTrue);
    expect((await _rememberedValue()).mailbox, 1);
    // Without overrides the probe uses this machine's real power status.
    expect(state['powerHeld'], state['hold']);

    // On AC, on battery, no battery (desktop), unknown status.
    state = await _probe({'ac': 1, 'battery': 1});
    expect(state['hold'], isTrue);
    expect(state['powerHeld'], isTrue);
    state = await _probe({'ac': 0, 'battery': 1});
    expect(state['hold'], isFalse);
    expect(state['powerHeld'], isFalse);
    state = await _probe({'ac': 0, 'battery': 128});
    expect(state['hold'], isTrue);
    expect(state['powerHeld'], isTrue);
    state = await _probe({'ac': 255, 'battery': 1});
    expect(state['hold'], isTrue);
    expect(state['powerHeld'], isTrue);

    // A real WM_CLOSE with the mailbox on hides the window; the app keeps running.
    state = await _probe({'close': true});
    expect(state['closeAction'], 'hide');
    expect(state['visible'], isFalse);
    expect(state['tray'], isTrue);
    state = await _probe({'open': true});
    expect(state['visible'], isTrue);

    expect(await _channel.invokeMethod<bool>('setMailbox', false), isTrue);
    state = await _probe();
    expect(state['tray'], isFalse);
    expect(state['powerHeld'], isFalse);
    expect(state['remembered'], isFalse);
    expect((await _rememberedValue()).mailbox, 0);
    state = await _probe({'ac': 1, 'battery': 1});
    expect(state['hold'], isFalse);
    expect(state['powerHeld'], isFalse);

    // With the mailbox off WM_CLOSE takes the quit path; the probe keeps the window.
    state = await _probe({'close': true});
    expect(state['closeAction'], 'destroy');
    expect(state['visible'], isTrue);

    // A count update of a notification that is not showing shows nothing
    // (Ruling 31): no balloon, so no icon either.
    await _channel.invokeMethod<void>('notify', {
      'id': 8,
      'title': 'PUBLIC MAILBOX FIXTURE',
      'body': 'PUBLIC MAILBOX FIXTURE',
      'update': true,
    });
    state = await _probe();
    expect(state['tray'], isFalse);
    expect(state['balloon'], isNull);

    // A notification with the mailbox off adds an icon only while it is shown.
    await _channel.invokeMethod<void>('notify', {
      'id': 7,
      'title': 'PUBLIC MAILBOX FIXTURE',
      'body': 'PUBLIC MAILBOX FIXTURE',
    });
    expect((await _probe())['tray'], isTrue);
    await _channel.invokeMethod<void>('cancelNotification', 7);
    expect((await _probe())['tray'], isFalse);
    await _channel.invokeMethod<void>('notify', {
      'id': 7,
      'title': 'PUBLIC MAILBOX FIXTURE',
      'body': 'PUBLIC MAILBOX FIXTURE',
      'update': true,
    });
    expect((await _probe())['tray'], isFalse);

    expect(await _channel.invokeMethod<bool>('setAutostart', true), isTrue);
    expect(await _channel.invokeMethod<bool>('autostart'), isTrue);
    final value = await _runValue();
    expect(value, '"${Platform.resolvedExecutable}" --tray');
    expect((await _probe())['autostart'], value);
    expect(await _channel.invokeMethod<bool>('setAutostart', false), isTrue);
    expect(await _channel.invokeMethod<bool>('autostart'), isFalse);
    expect(await _runValue(), isNull);
    expect((await _probe())['autostart'], isNull);
  }, skip: !Platform.isWindows);
}
