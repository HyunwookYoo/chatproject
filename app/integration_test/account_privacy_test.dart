import 'dart:io';
import 'package:chat_app/account/sensitive_screen.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  const channel = MethodChannel('chat_app/account_storage');
  testWidgets('native sensitive activation, handlers, restoration and failure', (
    tester,
  ) async {
    expect(Platform.isWindows || Platform.isAndroid || Platform.isIOS, isTrue);
    final initial = Platform.isWindows
        ? await channel.invokeMapMethod<String, dynamic>('privacyTest', 'state')
        : null;
    // Lifecycle transitions around activation, to tell startup focus changes
    // from ones caused by turning protection on.
    final transitions = <String>['start:${tester.binding.lifecycleState}'];
    final lifecycle = AppLifecycleListener(
      onStateChange: (state) => transitions.add('$state'),
    );
    transitions.add('pump');
    await tester.pumpWidget(
      const MaterialApp(
        home: Scaffold(
          body: SensitiveScreen(child: Text('PUBLIC PRIVACY FIXTURE')),
        ),
      ),
    );
    await tester.pumpAndSettle();
    lifecycle.dispose();
    if (Platform.isWindows) {
      debugPrint(
        'PUBLIC_NATIVE_API: ${await channel.invokeMapMethod<String, dynamic>("privacyTest", "state")}',
      );
    }
    // Which placeholder replaced the child, if any: hidden by a lifecycle change or failed protection.
    debugPrint(
      'PUBLIC_PRIVACY_STATE: transitions=$transitions '
      'lifecycle=${tester.binding.lifecycleState} '
      'hidden=${find.textContaining('숨겼어요').evaluate().length} '
      'failed=${find.textContaining('보호할 수 없어요').evaluate().length}',
    );
    expect(find.text('PUBLIC PRIVACY FIXTURE'), findsOneWidget);
    if (Platform.isWindows) {
      final state = (await channel.invokeMapMethod<String, dynamic>(
        'privacyTest',
        'probe',
      ))!;
      expect(state['baselineReady'], isTrue);
      expect(state['attributesOk'], isTrue);
      expect(state['sensitive'], isTrue);
      expect(state['force'], isTrue);
      expect(state['bitmap'], isTrue);
      expect(state['thumbnailOk'], isTrue);
      expect(state['previewOk'], isTrue);
      expect(state['gdiDelta'], 0);
    }
    const hold = int.fromEnvironment('PRIVACY_VISUAL_HOLD_SECONDS');
    if (hold > 0) {
      await tester.runAsync(
        () => Future<void>.delayed(Duration(seconds: hold)),
      );
    }
    await tester.pumpWidget(
      const MaterialApp(home: Scaffold(body: Text('PUBLIC NORMAL FIXTURE'))),
    );
    await tester.pumpAndSettle();
    if (Platform.isWindows) {
      for (var i = 0; i < 30; i++) {
        final current = await channel.invokeMapMethod<String, dynamic>(
          'privacyTest',
          'state',
        );
        if (current!['sensitive'] == false) break;
        await tester.pump(const Duration(milliseconds: 100));
      }
      final restored = (await channel.invokeMapMethod<String, dynamic>(
        'privacyTest',
        'state',
      ))!;
      expect(restored['sensitive'], isFalse);
      expect(restored['force'], initial!['force']);
      expect(restored['bitmap'], initial['bitmap']);
      for (final mode in ['fail', 'failPartial', 'failBitmap']) {
        await channel.invokeMethod<void>('privacyTest', mode);
        var failed = false;
        await tester.pumpWidget(
          MaterialApp(
            home: Scaffold(
              body: SensitiveScreen(
                onProtectionFailure: () => failed = true,
                child: const Text('WITHHELD PUBLIC FIXTURE'),
              ),
            ),
          ),
        );
        await tester.pumpAndSettle();
        expect(failed, isTrue);
        expect(find.text('WITHHELD PUBLIC FIXTURE'), findsNothing);
        await tester.pumpWidget(const SizedBox());
        await tester.pumpAndSettle();
        await channel.invokeMethod<void>('privacyTest', 'clear');
      }
      await tester.pumpWidget(
        const MaterialApp(
          home: Scaffold(
            body: SensitiveScreen(child: Text('PUBLIC PRIVACY FIXTURE')),
          ),
        ),
      );
      await tester.pumpAndSettle();
      expect(find.text('PUBLIC PRIVACY FIXTURE'), findsOneWidget);
      await channel.invokeMethod<void>('privacyTest', 'failBitmap');
      final failedHandlers = (await channel.invokeMapMethod<String, dynamic>(
        'privacyTest',
        'probe',
      ))!;
      expect(failedHandlers['thumbnailOk'], isFalse);
      expect(failedHandlers['previewOk'], isFalse);
      expect(failedHandlers['gdiDelta'], 0);
      await tester.pumpAndSettle();
      expect(find.text('PUBLIC PRIVACY FIXTURE'), findsNothing);
      await channel.invokeMethod<void>('privacyTest', 'clear');
      // Native hides the failed surface; no later raster means retain protection.
      await tester.pumpWidget(const SizedBox());
      await tester.pumpAndSettle();
      // Explicit public-fixture reset between injected native cases; not UI recovery.
      await channel.invokeMethod<void>('setSensitiveScreen', false);
      // No sensitive widget is present: exercise a failed native restoration.
      await channel.invokeMethod<void>('setSensitiveScreen', true);
      await channel.invokeMethod<void>('privacyTest', 'failRestore');
      await expectLater(
        channel.invokeMethod<void>('setSensitiveScreen', false),
        throwsA(isA<PlatformException>()),
      );
      await expectLater(
        channel.invokeMethod<void>('setSensitiveScreen', true),
        throwsA(isA<PlatformException>()),
      );
      await channel.invokeMethod<void>('privacyTest', 'clear');
      await channel.invokeMethod<void>('setSensitiveScreen', false);
    }
  });
}
