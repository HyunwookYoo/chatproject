import 'package:chat_app/mailbox/first_link_screen.dart';
import 'package:chat_app/mailbox/mailbox_model.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

import 'support/fake_mailbox.dart';

const _pcSwitch = '이 PC를 메일박스로 쓰기';
const _autostartSwitch = 'Windows 시작 시 자동 실행';

bool _switchOn(WidgetTester tester, String label) => tester
    .widget<SwitchListTile>(find.widgetWithText(SwitchListTile, label))
    .value;

/// The account home behind the gate, with the account becoming Active.
Future<MailboxModel> _start(
  WidgetTester tester,
  FakeMailboxClient client,
  FakeMailboxPlatform platform,
) async {
  final model = MailboxModel(client, platform);
  addTearDown(model.dispose);
  await tester.pumpWidget(
    MaterialApp(
      home: FirstLinkGate(model: model, child: const Text('account home')),
    ),
  );
  await model.accountActive();
  await tester.pumpAndSettle();
  return model;
}

void main() {
  testWidgets('a Windows PC not shown the screen yet sees it once, both '
      'switches on, and OK applies both', (tester) async {
    final client = FakeMailboxClient();
    final platform = FakeMailboxPlatform();
    final model = await _start(tester, client, platform);
    expect(find.byType(FirstLinkScreen), findsOneWidget);
    expect(_switchOn(tester, _pcSwitch), isTrue);
    expect(_switchOn(tester, _autostartSwitch), isTrue);

    await tester.tap(find.text('확인'));
    await tester.pumpAndSettle();
    expect(find.byType(FirstLinkScreen), findsNothing);
    expect(find.text('account home'), findsOneWidget);
    expect(client.sets, [true]);
    expect(client.marks, 1);
    expect(platform.calls, [
      'setMailbox:false',
      'setMailbox:true',
      'setAutostart:true',
    ]);
    expect(model.enabled, isTrue);
    expect(model.autostart, isTrue);

    // Later changes of the model do not bring it back.
    await model.setEnabled(false);
    await tester.pumpAndSettle();
    expect(find.byType(FirstLinkScreen), findsNothing);
  }, variant: TargetPlatformVariant.only(TargetPlatform.windows));

  testWidgets(
    'switches turned off on the screen are applied off',
    (tester) async {
      final client = FakeMailboxClient();
      final platform = FakeMailboxPlatform()..autostartValue = true;
      await _start(tester, client, platform);
      await tester.tap(find.text(_pcSwitch));
      await tester.tap(find.text(_autostartSwitch));
      await tester.pump();
      await tester.tap(find.text('확인'));
      await tester.pumpAndSettle();
      expect(client.sets, [false]);
      expect(client.marks, 1);
      expect(platform.calls.last, 'setAutostart:false');
    },
    variant: TargetPlatformVariant.only(TargetPlatform.windows),
  );

  testWidgets(
    'the next start does not show it again',
    (tester) async {
      final client = FakeMailboxClient();
      await _start(tester, client, FakeMailboxPlatform());
      await tester.tap(find.text('확인'));
      await tester.pumpAndSettle();

      await tester.pumpWidget(const SizedBox());
      await _start(tester, client, FakeMailboxPlatform());
      expect(find.byType(FirstLinkScreen), findsNothing);
      expect(find.text('account home'), findsOneWidget);
      expect(client.marks, 1);
    },
    variant: TargetPlatformVariant.only(TargetPlatform.windows),
  );

  for (final platform in [TargetPlatform.android, TargetPlatform.iOS]) {
    testWidgets(
      '${platform.name} never shows it',
      (tester) async {
        final client = FakeMailboxClient();
        await _start(tester, client, FakeMailboxPlatform());
        expect(client.states, 1);
        expect(find.byType(FirstLinkScreen), findsNothing);
        expect(client.marks, 0);
      },
      variant: TargetPlatformVariant.only(platform),
    );
  }
}
