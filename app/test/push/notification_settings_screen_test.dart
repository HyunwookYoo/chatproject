import 'package:chat_app/push/notification_settings_screen.dart';
import 'package:chat_app/push/push_model.dart';
import 'package:chat_app/push/push_platform.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/fake_push.dart';

void main() {
  late FakePushClient client;
  late FakePushHost host;

  Future<PushModel> mount(WidgetTester tester) async {
    client = FakePushClient();
    host = FakePushHost()..state = NotificationPermission.granted;
    final model = PushModel(client, host);
    await model.start();
    await tester.pumpWidget(
      MaterialApp(home: NotificationSettingsScreen(model: model)),
    );
    await tester.pumpAndSettle();
    addTearDown(() async {
      await tester.pumpWidget(const SizedBox());
      model.dispose();
    });
    return model;
  }

  bool switchOn(WidgetTester tester) => tester
      .widget<SwitchListTile>(find.widgetWithText(SwitchListTile, '푸시 알림'))
      .value;

  testWidgets(
    'the switch turns push off',
    (tester) async {
      await mount(tester);
      expect(switchOn(tester), isTrue);
      expect(find.text('휴대폰 설정에서 알림이 꺼져 있어요'), findsNothing);
      client.calls.clear();
      await tester.tap(find.text('푸시 알림'));
      await tester.pumpAndSettle();
      expect(client.calls, ['setEnabled:false']);
      expect(switchOn(tester), isFalse);
    },
    variant: TargetPlatformVariant.only(TargetPlatform.android),
  );

  testWidgets('without OS permission it says why, and turning it on opens '
      'the OS settings', (tester) async {
    final model = await mount(tester);
    host.state = NotificationPermission.denied;
    model.lifecycle(AppLifecycleState.resumed);
    await tester.pumpAndSettle();
    expect(switchOn(tester), isFalse);
    expect(find.text('휴대폰 설정에서 알림이 꺼져 있어요'), findsOneWidget);
    await tester.tap(find.text('푸시 알림'));
    await tester.pumpAndSettle();
    expect(host.calls, ['requestPermission', 'openSettings']);
    expect(client.calls, contains('setEnabled:false'));
    expect(client.calls, isNot(contains('setEnabled:true')));
  }, variant: TargetPlatformVariant.only(TargetPlatform.iOS));
}
