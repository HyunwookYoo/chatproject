import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:chat_app/src/rust/api/account.dart' as a;
import 'account_onboarding_test.dart' show ScreenClient, mountAccount, tapText;

void main() {
  testWidgets(
    'selected contact status follows the coherent account projection',
    (t) async {
      final c = ScreenClient()..phase = a.AccountPhase.active;
      await mountAccount(t, c);
      await tapText(t, '초대 붙여넣기');
      await t.enterText(find.byType(TextField), 'PUBLIC_INVITE');
      await tapText(t, '초대 확인');
      await t.enterText(find.byType(TextField), '친구');
      await tapText(t, '연락처 추가');
      await tapText(t, '\u2068친구\u2069');
      final previous = c.saved.single;
      c.saved = [
        a.ContactView(
          peerId: previous.peerId,
          alias: previous.alias,
          verified: true,
          newDevice: true,
          deviceUnverified: false,
        ),
      ];
      c.emit();
      await t.pumpAndSettle();
      expect(find.text('안전번호 확인됨'), findsOneWidget);
      expect(find.text('새 기기가 있어요'), findsOneWidget);
    },
  );

  testWidgets(
    'wide layout keeps contacts beside selected detail at 200 percent',
    (t) async {
      await t.binding.setSurfaceSize(const Size(1200, 900));
      addTearDown(() => t.binding.setSurfaceSize(null));
      final c = ScreenClient()..phase = a.AccountPhase.active;
      await mountAccount(t, c, scale: 2);
      await tapText(t, '초대 붙여넣기');
      await t.enterText(find.byType(TextField), 'PUBLIC_INVITE');
      await tapText(t, '초대 확인');
      await t.enterText(find.byType(TextField), '친구');
      await tapText(t, '연락처 추가');
      await tapText(t, '\u2068친구\u2069');
      expect(find.text('연락처'), findsOneWidget);
      expect(find.text('안전번호 확인'), findsOneWidget);
      expect(find.text('\u2068친구\u2069'), findsNWidgets(2));
      expect(t.takeException(), isNull);
    },
  );

  testWidgets('64/65 UTF8 name and isolated suggestion require explicit add', (
    t,
  ) async {
    final c = ScreenClient()..phase = a.AccountPhase.active;
    await mountAccount(t, c);
    await tapText(t, '초대 붙여넣기');
    await t.enterText(find.byType(TextField), 'PUBLIC_INVITE');
    await tapText(t, '초대 확인');
    expect(c.accepts, 0);
    expect(find.textContaining('\u2068'), findsWidgets);
    await t.enterText(find.byType(TextField), '가' * 21 + 'ab');
    await tapText(t, '연락처 추가');
    expect(c.accepts, 0);
    expect(find.text('이름은 UTF-8 64바이트까지 입력할 수 있어요.'), findsOneWidget);
    await t.enterText(find.byType(TextField), '가' * 21 + 'a');
    await tapText(t, '연락처 추가');
    expect(c.accepts, 1);
  });
  testWidgets('duplicate_contact_keeps_alias', (t) async {
    final c = ScreenClient()..phase = a.AccountPhase.active;
    await mountAccount(t, c);
    for (var i = 0; i < 2; i++) {
      await tapText(t, '초대 붙여넣기');
      await t.enterText(find.byType(TextField), 'PUBLIC_INVITE');
      await tapText(t, '초대 확인');
      await t.enterText(find.byType(TextField), i == 0 ? '내 별칭' : '다른 별칭');
      await tapText(t, '연락처 추가');
    }
    expect(c.saved.single.alias, '내 별칭');
    expect(find.text('다른 별칭'), findsNothing);
  });
  testWidgets('verification_requires_explicit_digest_confirm', (t) async {
    final c = ScreenClient()..phase = a.AccountPhase.active;
    await mountAccount(t, c);
    await tapText(t, '초대 붙여넣기');
    await t.enterText(find.byType(TextField), 'PUBLIC_INVITE');
    await tapText(t, '초대 확인');
    await tapText(t, '연락처 추가');
    await tapText(t, '\u2068${c.saved.first.alias}\u2069');
    await tapText(t, '안전번호 확인');
    expect(
      find.text(
        List.generate(32, (i) => i.toRadixString(16).padLeft(2, '0')).join(),
      ),
      findsOneWidget,
    );
    expect(c.verifications, 0);
    await tapText(t, '확인 완료');
    expect(c.verifications, 0);
    await tapText(t, '상대와 별도 경로로 전체 번호를 비교했어요');
    await tapText(t, '확인 완료');
    expect(c.verifications, 1);
    expect(c.saved.single.verified, isTrue);
  });
}
