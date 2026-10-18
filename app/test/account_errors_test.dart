import 'package:flutter_test/flutter_test.dart';
import 'package:flutter/material.dart';
import 'package:chat_app/src/rust/api/account.dart' as a;
import 'account_onboarding_test.dart' show ScreenClient, mountAccount, tapText;
import 'package:chat_app/account/account_model.dart';
import 'package:chat_app/src/rust/api/chat.dart';

void main() {
  testWidgets(
    'invalid invite exposes Korean code-only guidance without saving',
    (t) async {
      final c = ScreenClient()
        ..phase = a.AccountPhase.active
        ..failure = ErrorCode.invalidInvite;
      await mountAccount(t, c);
      await tapText(t, '초대 붙여넣기');
      await t.enterText(find.byType(TextField), 'PUBLIC_INVALID');
      await tapText(t, '초대 확인');
      expect(find.text('서명을 확인할 수 없는 초대예요.'), findsOneWidget);
      expect(find.textContaining('PUBLIC_RAW_DETAIL'), findsNothing);
      expect(c.accepts, 0);
    },
  );

  test('fixed Korean errors never expose enum', () {
    for (final code in ErrorCode.values) {
      expect(accountErrorMessage(code), isNot(contains(code.name)));
    }
    expect(accountErrorMessage(ErrorCode.invalidInvite), '서명을 확인할 수 없는 초대예요.');
    expect(
      accountErrorMessage(ErrorCode.recordConflict),
      '다른 기기에서 목록이 바뀌었어요. 현재 목록을 확인해 주세요.',
    );
  });
}
