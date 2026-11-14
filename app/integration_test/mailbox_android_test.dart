import 'dart:io';

import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

const _channel = MethodChannel('chat_app/mailbox');

/// `ServiceInfo.FOREGROUND_SERVICE_TYPE_REMOTE_MESSAGING`.
const _remoteMessaging = 1 << 9;

/// `NotificationManager.IMPORTANCE_MIN`.
const _importanceMin = 1;

/// `NotificationManager.IMPORTANCE_DEFAULT`.
const _importanceDefault = 3;

Future<Map<String, dynamic>> _probe() async =>
    (await _channel.invokeMapMethod<String, dynamic>('mailboxTest', {}))!;

/// The service starts and stops after the call returns; wait for it.
Future<Map<String, dynamic>> _waitRunning(bool running) async {
  final deadline = DateTime.now().add(const Duration(seconds: 10));
  while (true) {
    final state = await _probe();
    if (state['serviceRunning'] == running ||
        DateTime.now().isAfter(deadline)) {
      return state;
    }
    await Future<void>.delayed(const Duration(milliseconds: 100));
  }
}

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets(
    'mailbox mode runs a minimal remoteMessaging service',
    (tester) async {
      addTearDown(() => _channel.invokeMethod<bool>('setMailbox', false));

      // An everyday phone that never turned mailbox mode on has no service.
      var state = await _probe();
      expect(state['serviceRunning'], isFalse);
      expect(state['fgsType'], 0);

      expect(await _channel.invokeMethod<bool>('setMailbox', true), isTrue);
      state = await _waitRunning(true);
      expect(state['serviceRunning'], isTrue);
      expect(state['channelImportance'], _importanceMin);
      expect((state['fgsType'] as int) & _remoteMessaging, _remoteMessaging);

      // Turning it on again keeps the one service.
      expect(await _channel.invokeMethod<bool>('setMailbox', true), isTrue);
      expect((await _waitRunning(true))['serviceRunning'], isTrue);

      // Most likely before that second start reached startForeground, which
      // must not crash the app.
      expect(await _channel.invokeMethod<bool>('setMailbox', false), isTrue);
      state = await _waitRunning(false);
      expect(state['serviceRunning'], isFalse);
      expect(state['fgsType'], 0);
      // The channel stays as created; only the service is gone.
      expect(state['channelImportance'], _importanceMin);

      // Message notifications post and cancel without the service.
      await _channel.invokeMethod<void>('notify', {
        'id': 7,
        'title': 'PUBLIC MAILBOX FIXTURE',
        'body': 'PUBLIC MAILBOX FIXTURE',
      });
      await _channel.invokeMethod<void>('cancelNotification', 7);
      // A count update of a notification that is not shown posts nothing
      // (Ruling 31).
      await _channel.invokeMethod<void>('notify', {
        'id': 8,
        'title': 'PUBLIC MAILBOX FIXTURE',
        'body': 'PUBLIC MAILBOX FIXTURE',
        'update': true,
      });
      state = await _probe();
      expect(state['activeMessages'], isNot(contains(8)));
      expect(state['messagesImportance'], _importanceDefault);
      expect(state['serviceRunning'], isFalse);
    },
    skip: !Platform.isAndroid,
  );
}
