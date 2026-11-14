import 'dart:async';

import 'package:chat_app/account/account_model.dart';
import 'package:chat_app/account/account_routes.dart';
import 'package:chat_app/mailbox/mailbox_model.dart';
import 'package:chat_app/mailbox/mailbox_settings_screen.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'support/fake_mailbox.dart';

const _pcSwitch = '이 PC를 메일박스로 쓰기';
const _deviceSwitch = '이 기기를 메일박스로 쓰기';
const _autostartSwitch = 'Windows 시작 시 자동 실행';

bool _switchOn(WidgetTester tester, String label) => tester
    .widget<SwitchListTile>(find.widgetWithText(SwitchListTile, label))
    .value;

Future<MailboxModel> _settings(
  WidgetTester tester,
  FakeMailboxClient client,
  FakeMailboxPlatform platform,
) async {
  final model = MailboxModel(client, platform);
  addTearDown(model.dispose);
  await model.accountActive();
  await tester.pumpWidget(
    MaterialApp(home: MailboxSettingsScreen(model: model)),
  );
  await tester.pumpAndSettle();
  return model;
}

void main() {
  testWidgets('Windows shows both switches and turning the mailbox on reaches '
      'the core and the platform', (tester) async {
    final client = FakeMailboxClient()..firstLinkShown = true;
    final platform = FakeMailboxPlatform();
    await _settings(tester, client, platform);
    expect(find.widgetWithText(SwitchListTile, _pcSwitch), findsOneWidget);
    expect(
      find.widgetWithText(SwitchListTile, _autostartSwitch),
      findsOneWidget,
    );
    expect(find.text(_deviceSwitch), findsNothing);
    expect(_switchOn(tester, _pcSwitch), isFalse);

    await tester.tap(find.text(_pcSwitch));
    await tester.pumpAndSettle();
    expect(client.sets, [true]);
    expect(platform.calls, ['setMailbox:false', 'setMailbox:true']);
    expect(_switchOn(tester, _pcSwitch), isTrue);

    await tester.tap(find.text(_autostartSwitch));
    await tester.pumpAndSettle();
    expect(platform.calls.last, 'setAutostart:true');
    expect(_switchOn(tester, _autostartSwitch), isTrue);

    await tester.tap(find.text(_pcSwitch));
    await tester.pumpAndSettle();
    expect(client.sets, [true, false]);
    expect(platform.calls.last, 'setMailbox:false');
    expect(_switchOn(tester, _pcSwitch), isFalse);
  }, variant: TargetPlatformVariant.only(TargetPlatform.windows));

  testWidgets('Android shows one switch, asks the battery exemption when it '
      'turns on', (tester) async {
    final client = FakeMailboxClient();
    final platform = FakeMailboxPlatform();
    await _settings(tester, client, platform);
    expect(find.byType(SwitchListTile), findsOneWidget);
    expect(find.widgetWithText(SwitchListTile, _deviceSwitch), findsOneWidget);
    expect(find.text(_autostartSwitch), findsNothing);
    expect(_switchOn(tester, _deviceSwitch), isFalse);

    await tester.tap(find.text(_deviceSwitch));
    await tester.pumpAndSettle();
    expect(client.sets, [true]);
    expect(platform.calls.skip(2), [
      'setMailbox:true',
      'requestBatteryExemption',
    ]);
    expect(_switchOn(tester, _deviceSwitch), isTrue);
  }, variant: TargetPlatformVariant.only(TargetPlatform.android));

  testWidgets('Android keeps the switch off and reverts the core when the '
      'platform cannot start the mailbox', (tester) async {
    final client = FakeMailboxClient();
    final platform = FakeMailboxPlatform()..mailboxResult = false;
    await _settings(tester, client, platform);

    await tester.tap(find.text(_deviceSwitch));
    await tester.pumpAndSettle();
    expect(client.sets, [true, false]);
    expect(client.enabled, isFalse);
    expect(platform.calls, isNot(contains('requestBatteryExemption')));
    expect(_switchOn(tester, _deviceSwitch), isFalse);
    expect(find.textContaining('켜지 못했어요'), findsOneWidget);
  }, variant: TargetPlatformVariant.only(TargetPlatform.android));

  test('the stored setting reaches the platform once the account is Active; '
      'Android asks the notification permission once', () async {
    debugDefaultTargetPlatformOverride = TargetPlatform.android;
    addTearDown(() => debugDefaultTargetPlatformOverride = null);
    final client = FakeMailboxClient()..enabled = true;
    final platform = FakeMailboxPlatform();
    final model = MailboxModel(client, platform);
    addTearDown(model.dispose);
    await model.accountActive();
    await model.accountActive();
    expect(client.states, 1);
    expect(model.enabled, isTrue);
    expect(platform.calls, [
      'setMailbox:true',
      'requestNotificationPermission',
    ]);
  });

  test('Windows never asks a notification permission at startup', () async {
    debugDefaultTargetPlatformOverride = TargetPlatform.windows;
    addTearDown(() => debugDefaultTargetPlatformOverride = null);
    final platform = FakeMailboxPlatform();
    final model = MailboxModel(FakeMailboxClient(), platform);
    addTearDown(model.dispose);
    await model.accountActive();
    expect(platform.calls, ['setMailbox:false']);
  });

  test('an Android start in the background restores the mailbox on the next '
      'resume, without reverting the setting', () async {
    debugDefaultTargetPlatformOverride = TargetPlatform.android;
    addTearDown(() => debugDefaultTargetPlatformOverride = null);
    final client = FakeMailboxClient()..enabled = true;
    final platform = FakeMailboxPlatform()..mailboxResult = false;
    final model = MailboxModel(client, platform);
    addTearDown(model.dispose);
    await model.accountActive();
    expect(client.sets, isEmpty);
    expect(model.enabled, isTrue);

    platform.mailboxResult = true;
    model.lifecycle(AppLifecycleState.resumed);
    await pumpEventQueue();
    expect(platform.calls.where((c) => c == 'setMailbox:true').length, 2);
    model.lifecycle(AppLifecycleState.resumed);
    await pumpEventQueue();
    expect(platform.calls.where((c) => c == 'setMailbox:true').length, 2);
  });

  test('resume syncs, going to the background hands off once', () async {
    debugDefaultTargetPlatformOverride = TargetPlatform.iOS;
    addTearDown(() => debugDefaultTargetPlatformOverride = null);
    final client = FakeMailboxClient();
    final model = MailboxModel(client, FakeMailboxPlatform());
    addTearDown(model.dispose);
    model.lifecycle(AppLifecycleState.hidden);
    await pumpEventQueue();
    expect(client.backgrounds, 0, reason: 'no account yet');

    await model.accountActive();
    model.lifecycle(AppLifecycleState.resumed);
    await pumpEventQueue();
    expect(client.foregrounds, 1);
    model.lifecycle(AppLifecycleState.inactive);
    model.lifecycle(AppLifecycleState.hidden);
    model.lifecycle(AppLifecycleState.paused);
    await pumpEventQueue();
    expect(client.backgrounds, 1);
    model.lifecycle(AppLifecycleState.resumed);
    model.lifecycle(AppLifecycleState.paused);
    await pumpEventQueue();
    expect(client.foregrounds, 2);
    expect(client.backgrounds, 2);
  });

  testWidgets(
    'exit hands off for at most 2 s, then quits',
    (tester) async {
      final client = FakeMailboxClient()..backgroundWait = Completer<void>();
      final platform = FakeMailboxPlatform();
      final model = MailboxModel(client, platform);
      addTearDown(model.dispose);
      await model.accountActive();
      var done = false;
      unawaited(model.exit().then((_) => done = true));
      await tester.pump(const Duration(milliseconds: 1900));
      expect(client.backgrounds, 1);
      expect(platform.calls, isNot(contains('quit')));
      await tester.pump(const Duration(milliseconds: 200));
      expect(platform.calls.last, 'quit');
      expect(done, isTrue);
    },
    variant: TargetPlatformVariant.only(TargetPlatform.windows),
  );

  for (final (platform, shown) in [
    (TargetPlatform.windows, true),
    (TargetPlatform.android, true),
    (TargetPlatform.iOS, false),
  ]) {
    testWidgets('the account home ${shown ? 'opens' : 'hides'} the mailbox '
        'settings on ${platform.name}', (tester) async {
      final account = AccountModel(ActiveAccountClient());
      final client = FakeMailboxClient()..firstLinkShown = true;
      final channel = FakeMailboxPlatform();
      final mailbox = MailboxModel(client, channel);
      await account.open();
      await tester.pumpWidget(
        MaterialApp(
          home: AccountRoutes(
            model: account,
            mailbox: mailbox,
            child: const SizedBox(),
          ),
        ),
      );
      await tester.pumpAndSettle();
      addTearDown(() async {
        await tester.pumpWidget(const SizedBox());
        account.dispose();
        mailbox.dispose();
      });
      expect(client.states, 1, reason: 'loaded once the account is Active');
      expect(find.text('메일박스'), shown ? findsOneWidget : findsNothing);
      if (!shown) return;
      await tester.tap(find.text('메일박스'));
      await tester.pumpAndSettle();
      expect(find.byType(MailboxSettingsScreen), findsOneWidget);
    }, variant: TargetPlatformVariant.only(platform));
  }
}
