import 'dart:convert';

import 'package:chat_app/push/push_model.dart';
import 'package:chat_app/push/push_platform.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/fake_push.dart';

void main() {
  late FakePushClient client;
  late FakePushHost host;
  late PushModel model;

  PushModel create(TargetPlatform platform, {String? proxyUrl}) {
    debugDefaultTargetPlatformOverride = platform;
    addTearDown(() => debugDefaultTargetPlatformOverride = null);
    client = FakePushClient();
    host = FakePushHost();
    model = PushModel(client, host, proxyUrl: proxyUrl);
    addTearDown(model.dispose);
    return model;
  }

  test('Android with permission: configures for FCM and hands the token over '
      'at every start', () async {
    create(TargetPlatform.android, proxyUrl: 'http://10.0.2.2:8080');
    host.state = NotificationPermission.granted;
    await model.start();
    await model.start();
    expect(client.calls, [
      'configure:http://10.0.2.2:8080:fcm:null',
      'setToken:fcm:TOKEN',
    ]);
    expect(model.enabled, isTrue);
    expect(model.loaded, isTrue);
  });

  test('without OS permission push is turned off at start (spec 8)', () async {
    create(TargetPlatform.android);
    host.state = NotificationPermission.denied;
    await model.start();
    expect(client.calls, ['configure:null:fcm:null', 'setEnabled:false']);
    expect(model.enabled, isFalse);
  });

  test('Windows configures for sending only and asks nothing', () async {
    create(TargetPlatform.windows);
    await model.start();
    expect(client.calls, ['configure:null:null:null']);
    expect(model.askDue, isFalse);
    expect(PushModel.supported, isFalse);
  });

  test('iOS: App Group snapshot, APNs token, inbox at launch and every '
      'resume', () async {
    create(TargetPlatform.iOS);
    host.state = NotificationPermission.granted;
    await model.start();
    expect(client.calls, [
      'configure:null:apns:/group',
      'setToken:apns:TOKEN',
      'ingest:/group/push',
    ]);
    client.calls.clear();
    model.lifecycle(AppLifecycleState.resumed);
    await pumpEventQueue();
    expect(client.calls, ['setToken:apns:TOKEN', 'ingest:/group/push']);

    client.calls.clear();
    host.onToken!('NEWTOKEN');
    await pumpEventQueue();
    expect(client.calls, ['setToken:apns:NEWTOKEN']);
  });

  test('a permission turned off in the OS turns push off on resume', () async {
    create(TargetPlatform.android);
    host.state = NotificationPermission.granted;
    await model.start();
    client.calls.clear();
    host.state = NotificationPermission.denied;
    model.lifecycle(AppLifecycleState.resumed);
    await pumpEventQueue();
    expect(client.calls, ['setEnabled:false']);
    expect(model.permission, NotificationPermission.denied);
  });

  test('iOS: a tapped notification opens the conversation its tag names; one '
      'tapped before start is taken at start', () async {
    create(TargetPlatform.iOS);
    host.state = NotificationPermission.granted;
    final opened = <List<int>>[];
    model.onOpen = opened.add;
    final tag = List.generate(16, (i) => i);
    final t = base64Url.encode(tag).replaceAll('=', '');
    expect(await host.onOpened!(t), isFalse, reason: 'not started yet');

    host.openedTag = t;
    client.conversation = Uint8List.fromList(List.filled(32, 7));
    await model.start();
    expect(client.tags, [tag]);
    expect(opened, [List.filled(32, 7)]);

    expect(await host.onOpened!('not base64!'), isTrue);
    expect(await host.onOpened!(base64Url.encode([1, 2])), isTrue);
    expect(client.tags, hasLength(1), reason: 'malformed tags are dropped');

    client.conversation = null;
    expect(await host.onOpened!(t), isTrue);
    expect(opened, hasLength(1), reason: 'an unknown tag opens nothing');
  });

  test('Android reports the conversation on screen; other platforms do '
      'not', () async {
    create(TargetPlatform.android);
    model.conversationShown([1, 1, 1, 2, 9]);
    model.conversationShown(null);
    await pumpEventQueue();
    expect(host.calls, [
      'setOpenConversation:${0x01010102}',
      'setOpenConversation:null',
    ]);

    create(TargetPlatform.iOS);
    model.conversationShown([1, 1, 1, 2]);
    await pumpEventQueue();
    expect(host.calls, isEmpty);
  });

  test('asked only while the platform has not asked yet', () async {
    create(TargetPlatform.android);
    await pumpEventQueue();
    expect(model.askDue, isTrue);
    await model.later();
    expect(host.calls, ['markAsked']);
    expect(model.askDue, isFalse);

    create(TargetPlatform.iOS);
    host.state = NotificationPermission.granted;
    model = PushModel(client, host);
    addTearDown(model.dispose);
    await pumpEventQueue();
    expect(model.askDue, isFalse);
  });

  test('allow before the account is active only asks; start applies '
      'it', () async {
    create(TargetPlatform.android);
    await model.allow();
    expect(host.calls, ['requestPermission']);
    expect(client.calls, isEmpty);
    await model.start();
    expect(client.calls, ['configure:null:fcm:null', 'setToken:fcm:TOKEN']);
  });

  test('allow after start: granted turns push on with the token, refused '
      'turns it off', () async {
    create(TargetPlatform.android);
    await model.start();
    client.calls.clear();
    await model.allow();
    await pumpEventQueue();
    expect(client.calls, ['setEnabled:true', 'setToken:fcm:TOKEN']);

    create(TargetPlatform.android);
    host.grant = false;
    await model.start();
    client.calls.clear();
    await model.allow();
    expect(client.calls, ['setEnabled:false']);
    expect(model.enabled, isFalse);
  });

  test('settings switch: off turns push off; on without permission asks, '
      'then opens the OS settings', () async {
    create(TargetPlatform.android);
    host.state = NotificationPermission.granted;
    await model.start();
    client.calls.clear();
    await model.setEnabled(false);
    expect(client.calls, ['setEnabled:false']);
    expect(model.enabled, isFalse);

    host.state = NotificationPermission.denied;
    client.calls.clear();
    await model.setEnabled(true);
    expect(host.calls, ['requestPermission', 'openSettings']);
    expect(client.calls, isEmpty);
    expect(model.enabled, isFalse);

    host.state = NotificationPermission.granted;
    await model.setEnabled(true);
    expect(client.calls, ['setEnabled:true', 'setToken:fcm:TOKEN']);
    expect(model.enabled, isTrue);
  });
}
