import 'dart:convert';
import 'dart:io';
import 'dart:ffi';
import 'package:chat_app/account/account_model.dart';
import 'package:chat_app/bootstrap.dart';
import 'package:chat_app/core_client.dart';
import 'package:chat_app/src/rust/api/account.dart' as account;
import 'package:chat_app/src/rust/api/chat.dart' as rust;
import 'package:chat_app/src/rust/frb_generated.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

// Each invocation is a distinct native process: CORE intentionally has no reset.
// Example: --dart-define=ACCOUNT_STORAGE_PHASE=write --dart-define=ACCOUNT_STORAGE_ROOT=C:/owned/path
void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  const phase = String.fromEnvironment('ACCOUNT_STORAGE_PHASE');
  const root = String.fromEnvironment('ACCOUNT_STORAGE_ROOT');
  testWidgets(
    'native protected storage phase (explicit process boundary)',
    (tester) async {
      expect(
        ['write', 'read', 'locked', 'missing', 'lifecycle'].contains(phase),
        isTrue,
        reason: 'An explicit valid storage phase is required',
      );
      expect(
        root.isNotEmpty && Directory(root).isAbsolute,
        isTrue,
        reason: 'An explicit owned absolute root is required',
      );
      if (phase == 'lifecycle') expect(Platform.isIOS, isTrue);
      await RustLib.init();
      final injected = switch (phase) {
        'locked' => rust.ErrorCode.storageLocked,
        'missing' => rust.ErrorCode.storageMissing,
        _ => null,
      };
      final info = await account.testStorageStart(
        dataDir: root,
        failure: injected,
      );
      final core = FrbCoreClient(dataDir: root);
      final receiptFile = File('$root/bridge-receipt.json');
      if (injected != null) {
        final app = await bootstrap(
          [],
          initializeRust: () async {},
          createClient: (_) => core,
        );
        expect(app, isA<AccountGate>());
        await tester.pumpWidget(app);
        await tester.pumpAndSettle();
        expect(find.text(accountErrorMessage(injected)), findsOneWidget);
        expect(find.text('다시 시도'), findsOneWidget);
        expect(
          (app as AccountGate).model.snapshot!.phase,
          account.AccountPhase.locked,
        );
        expect(app.model.errorCode, injected);
        await tester.tap(find.text('다시 시도'));
        await tester.pumpAndSettle();
        expect(app.model.errorCode, injected);
        expect(app.model.snapshot!.phase, account.AccountPhase.locked);
        await tester.pumpWidget(const SizedBox());
        // These modes inject a status around the real OS provider; they do not lock the OS.
        return;
      }
      await core.account.openAccount();
      final captured = await core.account.snapshot();
      // Native registration occurs after snapshot capture: the bus must replay its public projection.
      final replay = await core
          .events()
          .where((e) => e is rust.ChatEvent_AccountChanged)
          .first
          .timeout(const Duration(seconds: 10));
      expect(
        (replay as rust.ChatEvent_AccountChanged).revision,
        greaterThanOrEqualTo(captured.revision),
      );
      expect(replay.revision, replay.view.revision);
      final events = <rust.ChatEvent>[];
      final subscription = core.events().listen(events.add);
      final model = AccountModel(core.account);
      await model.open();
      if (phase == 'write') {
        expect(
          await receiptFile.exists(),
          isFalse,
          reason: 'Write phase must own a fresh directory',
        );
        expect(model.snapshot!.phase, account.AccountPhase.unconfigured);
        var display = await model.beginCreate();
        expect(display != null && display.words.length == 24, isTrue);
        final answers = display!.positions
            .map((position) => display!.words[position - 1])
            .toList();
        expect(await model.confirmCreate(display.flow, answers), isTrue);
        answers.clear();
        display = null;
        expect(model.recoveryDisplay, isNull);
        final saved = await core.account.snapshot();
        expect(saved.phase, account.AccountPhase.localReady);
        expect(saved.publicPeerId?.length, 32);
        // The identity, installation and transport PUBLIC keys are the only persisted test receipt.
        await receiptFile.writeAsString(
          jsonEncode({
            'peer': saved.publicPeerId,
            'installation': saved.installation,
            'transport': saved.devices.single.transportPublicKey,
            'instance': info.instanceId,
          }),
          flush: true,
        );
      } else {
        expect(
          await receiptFile.exists(),
          isTrue,
          reason: 'Read phase requires prior successful write process',
        );
        final receipt =
            jsonDecode(await receiptFile.readAsString())
                as Map<String, dynamic>;
        final restored = await core.account.snapshot();
        expect(restored.phase, account.AccountPhase.localReady);
        expect(restored.publicPeerId, receipt['peer']);
        expect(restored.installation, receipt['installation']);
        expect(
          restored.devices.single.transportPublicKey,
          receipt['transport'],
        );
        expect(info.instanceId, isNot(receipt['instance']));
      }
      final latest = await core.account.snapshot();
      await Future<void>.delayed(const Duration(milliseconds: 100));
      final projections = events
          .whereType<rust.ChatEvent_AccountChanged>()
          .toList();
      expect(projections.isNotEmpty || phase == 'read', isTrue);
      for (final event in projections) {
        expect(event.revision, event.view.revision);
        expect(event.revision <= latest.revision, isTrue);
      }
      // SQL authority retires the legacy envelope and keeps its guard. Opening
      // above already validated the runtime cipher/provider and SQL binding.
      for (final name in ['db.keys', 'db.sqlite3', 'storage.mode', 'store.id']) {
        expect(await File('$root/$name').exists(), isTrue, reason: name);
      }
      expect(await File('$root/db.sqlite3').length(), greaterThan(0));
      expect(await File('$root/store.id').length(), 16);
      expect(await File('$root/key.wrap').exists(), isFalse);
      expect(await File('$root/account.bin').exists(), isFalse);
      if (phase == 'lifecycle') {
        // Existing iOS ABI + real OS provider. Completed/status and readback are
        // observed here; actual native SQL handle/lock closure remains a separate
        // Apple-runner gate, since iOS ownership/connection lifetime is transient.
        final library = DynamicLibrary.process();
        final suspend = library.lookupFunction<Int32 Function(), int Function()>('cp_account_suspend');
        final resume = library.lookupFunction<Int32 Function(), int Function()>('cp_account_resume');
        Future<void> completed(int Function() operation) async {
          final deadline = DateTime.now().add(const Duration(seconds: 10));
          var status = operation();
          while (status == 1 && DateTime.now().isBefore(deadline)) {
            await Future<void>.delayed(const Duration(milliseconds: 10));
            status = operation();
          }
          expect(status, 0, reason: 'Pending/failed lifecycle is not Completed');
        }
        await completed(suspend);
        await completed(resume);
        final reopened = await core.account.snapshot();
        expect(reopened.phase, latest.phase);
        expect(reopened.publicPeerId, latest.publicPeerId);
        expect(reopened.installation, latest.installation);
      }
      await model.close();
      await subscription.cancel();
    },
    timeout: const Timeout(Duration(minutes: 2)),
  );
}
