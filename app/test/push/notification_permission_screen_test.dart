import 'package:chat_app/account/account_model.dart';
import 'package:chat_app/account/account_routes.dart';
import 'package:chat_app/push/notification_permission_screen.dart';
import 'package:chat_app/push/notification_settings_screen.dart';
import 'package:chat_app/push/push_model.dart';
import 'package:chat_app/push/push_platform.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import '../support/fake_mailbox.dart';
import '../support/fake_push.dart';

const _question = '새 메시지를 알림으로 알려 드릴까요?';

void main() {
  late FakePushClient client;
  late FakePushHost host;

  Future<(PushModel, List<String>)> mountScreen(WidgetTester tester) async {
    client = FakePushClient();
    host = FakePushHost();
    final model = PushModel(client, host);
    await model.start();
    final done = <String>[];
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: NotificationPermissionScreen(
            model: model,
            onDone: () => done.add('done'),
          ),
        ),
      ),
    );
    await tester.pumpAndSettle();
    addTearDown(() async {
      await tester.pumpWidget(const SizedBox());
      model.dispose();
    });
    return (model, done);
  }

  testWidgets(
    '알림 켜기 asks the OS; granted turns push on',
    (tester) async {
      final (model, done) = await mountScreen(tester);
      expect(find.text(_question), findsOneWidget);
      expect(find.text('나중에'), findsOneWidget);
      client.calls.clear();
      await tester.tap(find.text('알림 켜기'));
      await tester.pumpAndSettle();
      expect(host.calls, ['requestPermission']);
      expect(client.calls, ['setEnabled:true', 'setToken:fcm:TOKEN']);
      expect(done, ['done']);
      expect(model.askDue, isFalse);
    },
    variant: TargetPlatformVariant.only(TargetPlatform.android),
  );

  testWidgets(
    'a refused prompt turns push off',
    (tester) async {
      await mountScreen(tester);
      host.grant = false;
      client.calls.clear();
      await tester.tap(find.text('알림 켜기'));
      await tester.pumpAndSettle();
      expect(client.calls, ['setEnabled:false']);
    },
    variant: TargetPlatformVariant.only(TargetPlatform.iOS),
  );

  testWidgets(
    '나중에 marks it asked and leaves push off',
    (tester) async {
      final (model, done) = await mountScreen(tester);
      client.calls.clear();
      await tester.tap(find.text('나중에'));
      await tester.pumpAndSettle();
      expect(host.calls, ['markAsked']);
      expect(client.calls, ['setEnabled:false']);
      expect(done, ['done']);
      expect(model.askDue, isFalse);
    },
    variant: TargetPlatformVariant.only(TargetPlatform.android),
  );

  Future<PushModel> mountRoutes(WidgetTester tester) async {
    client = FakePushClient();
    final push = PushModel(client, host);
    final account = AccountModel(ActiveAccountClient());
    await account.open();
    await tester.pumpWidget(
      MaterialApp(
        home: AccountRoutes(
          model: account,
          push: push,
          child: const Text('diagnostic host'),
        ),
      ),
    );
    await tester.pumpAndSettle();
    addTearDown(() async {
      await tester.pumpWidget(const SizedBox());
      push.dispose();
      account.dispose();
    });
    return push;
  }

  testWidgets('an onboarded device is asked once after the update, then '
      'finds the settings under 알림', (tester) async {
    host = FakePushHost();
    await mountRoutes(tester);
    expect(find.text(_question), findsOneWidget);
    await tester.tap(find.text('나중에'));
    await tester.pumpAndSettle();
    expect(find.text(_question), findsNothing);
    expect(find.text('연락처'), findsOneWidget);

    await tester.tap(find.text('알림'));
    await tester.pumpAndSettle();
    expect(find.byType(NotificationSettingsScreen), findsOneWidget);
    await tester.pageBack();
    await tester.pumpAndSettle();
    expect(find.text(_question), findsNothing, reason: 'asked once');
  }, variant: TargetPlatformVariant.only(TargetPlatform.android));

  testWidgets(
    'a device asked already is not asked again',
    (tester) async {
      host = FakePushHost()..state = NotificationPermission.denied;
      await mountRoutes(tester);
      expect(find.text(_question), findsNothing);
      expect(client.calls, contains('setEnabled:false'));
    },
    variant: TargetPlatformVariant.only(TargetPlatform.iOS),
  );

  testWidgets(
    'Windows neither asks nor offers push settings',
    (tester) async {
      host = FakePushHost();
      await mountRoutes(tester);
      expect(find.text(_question), findsNothing);
      expect(find.text('알림'), findsNothing);
      expect(client.calls, ['configure:null:null:null']);
    },
    variant: TargetPlatformVariant.only(TargetPlatform.windows),
  );
}
