import 'package:chat_app/startup_error_app.dart';
import 'package:chat_app/theme/tokens.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  testWidgets('shows why the core could not load, on the themed background', (tester) async {
    await tester.pumpWidget(const StartupErrorApp(reason: 'boom'));

    expect(find.text('코어를 시작하지 못했어요 · boom'), findsOneWidget);
    final scaffoldMaterial = find.descendant(of: find.byType(Scaffold), matching: find.byType(Material)).first;
    expect(tester.widget<Material>(scaffoldMaterial).color, AppColors.background);
  });
}
