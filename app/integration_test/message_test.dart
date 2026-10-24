import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:typed_data';
import 'dart:ui' show FrameTiming;

import 'package:chat_app/account/account_model.dart';
import 'package:chat_app/core_client.dart';
import 'package:chat_app/message/message_bridge.dart';
import 'package:chat_app/message/message_model.dart';
import 'package:chat_app/message/screens/conversation_screen.dart';
import 'package:chat_app/src/rust/api/account.dart' as account;
import 'package:chat_app/src/rust/api/chat.dart' as rust;
import 'package:chat_app/src/rust/frb_generated.dart';
import 'package:chat_app/theme/app_theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

// PREPARED for Task 13: two distinct owned native processes, write then read,
// same explicit MESSAGE_TEST_ROOT. Requires the existing Debug test-hooks build.
// No network start, raw message bridge, new native fixture API, or default data.
void main() {
  final binding = IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  const phase = String.fromEnvironment('MESSAGE_TEST_PHASE');
  const root = String.fromEnvironment('MESSAGE_TEST_ROOT');
  testWidgets(
    'native message enqueue restart cursor and typed event probe',
    (tester) async {
      expect(
        ['write', 'read'].contains(phase),
        true,
        reason: 'Explicit write/read phase required',
      );
      expect(
        root.isNotEmpty && Directory(root).isAbsolute,
        true,
        reason: 'Explicit owned absolute root required',
      );
      final receipt = File('$root/message-probe-receipt.json');
      expect(
        await receipt.exists(),
        phase == 'read',
        reason: 'Write requires a fresh fixture; read requires its receipt',
      );
      await RustLib.init();
      final info = await account.testStorageStart(dataDir: root);
      final core = FrbCoreClient(dataDir: root);
      final accounts = AccountModel(core.account);
      await accounts.open();
      final peer = Uint8List.fromList(List.filled(32, 253));
      final client =
          core.messages; // checked FrbMessageClient, shared event bus
      var changes = 0;
      BigInt? eventRevision;
      final subscription = core.events().listen((event) {
        if (event is rust.ChatEvent_MessagesChanged) {
          changes++;
          eventRevision = event.revision;
        }
      });
      final commitMicros = <int>[];
      if (phase == 'write') {
        expect(
          await receipt.exists(),
          false,
          reason: 'Write requires a fresh owned fixture',
        );
        expect(accounts.snapshot!.phase, account.AccountPhase.unconfigured);
        var display = await accounts.beginCreate();
        expect(display != null, true);
        final answers = display!.positions
            .map((p) => display!.words[p - 1])
            .toList();
        expect(await accounts.confirmCreate(display.flow, answers), true);
        answers.clear();
        display = null;
        for (var i = 0; i < 55; i++) {
          final watch = Stopwatch()..start();
          final saved = await client.sendText(
            peer: peer,
            id: List.filled(16, i),
            body: 'Public native fixture $i',
          );
          commitMicros.add(watch.elapsedMicroseconds);
          expect(
            saved.state,
            DeliveryState.queued,
          ); // returned only after commit
        }
        await receipt.writeAsString(
          jsonEncode({
            'instance': info.instanceId,
            'peer': accounts.snapshot!.publicPeerId,
            'installation': accounts.snapshot!.installation,
            'count': 55,
          }),
          flush: true,
        );
      } else {
        expect(
          await receipt.exists(),
          true,
          reason: 'Read requires the prior write process',
        );
        final previous =
            jsonDecode(await receipt.readAsString()) as Map<String, dynamic>;
        expect(info.instanceId == previous['instance'], false);
        expect(accounts.snapshot!.publicPeerId, previous['peer']);
        expect(accounts.snapshot!.installation, previous['installation']);
      }
      final conversation = await client.openDm(peer: peer);
      final page = await client.loadMessages(
        conversation: conversation.id,
        limit: 50,
      );
      expect(page.items.length, 50);
      expect(page.next != null, true);
      final older = await client.loadMessages(
        conversation: conversation.id,
        before: page.next,
        limit: 50,
      );
      expect(older.items.length, 5);
      final all = [...page.items, ...older.items];
      expect(all.map((row) => row.key.id.join(',')).toSet().length, 55);
      expect(
        all.every(
          (row) => row.body == 'Public native fixture ${row.key.id.first}',
        ),
        true,
      );
      expect(
        all.any(
          (row) =>
              row.state == DeliveryState.delivered ||
              row.state == DeliveryState.read,
        ),
        false,
      );

      final model = MessageModel(client, account: accounts);
      await tester.pumpWidget(
        MaterialApp(
          theme: buildAppTheme(),
          home: ConversationScreen(model: model, peer: peer),
        ),
      );
      final openingDeadline = DateTime.now().add(const Duration(seconds: 10));
      while (model.busy && DateTime.now().isBefore(openingDeadline)) {
        await tester.pump(const Duration(milliseconds: 10));
      }
      await tester.pumpAndSettle();
      expect(model.messages.length, 50);
      final loading = model.loadOlder();
      await tester.pumpAndSettle();
      await loading;
      expect(model.messages.length, 55);

      // Input -> first completed framework frame is distinct from native commit.
      // This is not physical key-to-photon latency; report exact scopes and misses.
      await tester.showKeyboard(find.byType(TextField));
      await tester.pumpAndSettle();
      final frames = <FrameTiming>[];
      void timings(List<FrameTiming> batch) => frames.addAll(batch);
      WidgetsBinding.instance.addTimingsCallback(timings);
      final watch = Stopwatch()..start();
      final rendered = Completer<int>();
      WidgetsBinding.instance.addPostFrameCallback(
        (_) => rendered.complete(watch.elapsedMicroseconds),
      );
      tester.testTextInput.enterText('Public input probe');
      await tester.pump();
      final inputToFrameUs = await rendered.future;
      expect(
        tester.widget<TextField>(find.byType(TextField)).controller!.text ==
            'Public input probe',
        true,
      );
      await tester.pumpAndSettle();
      WidgetsBinding.instance.removeTimingsCallback(timings);
      if (phase == 'write') {
        expect(changes > 0, true);
        expect(eventRevision != null, true);
      }
      final metrics = <String, Object?>{
        'phase': phase,
        'scope':
            'native debug; automation input to framework frame; checked sendText durable return; frame build/raster',
        'input_to_frame_us': inputToFrameUs,
        'input_target_16ms_met': inputToFrameUs <= 16000,
        'durable_commit_us': commitMicros,
        'durable_target_misses': commitMicros.where((us) => us > 16000).length,
        'frame_build_us': frames
            .map((f) => f.buildDuration.inMicroseconds)
            .toList(),
        'frame_raster_us': frames
            .map((f) => f.rasterDuration.inMicroseconds)
            .toList(),
        'typed_changes': changes,
        'last_event_revision': eventRevision?.toString(),
        'message_count': all.length,
      };
      // Metadata only: never persist bodies, recovery words, contact names or keys.
      await File(
        '$root/message-probe-$phase-metrics.json',
      ).writeAsString(jsonEncode(metrics), flush: true);
      binding.reportData = metrics;
      await tester.pumpWidget(const SizedBox());
      model.dispose();
      await accounts.close();
      accounts.dispose();
      await subscription.cancel();
    },
    timeout: const Timeout(Duration(minutes: 3)),
  );
}
