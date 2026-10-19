import 'dart:io';
import 'dart:typed_data';
import 'package:chat_app/account/account_model.dart';
import 'package:chat_app/core_client.dart';
import 'package:chat_app/src/rust/api/account.dart' as account;
import 'package:chat_app/src/rust/api/chat.dart' as rust;
import 'package:chat_app/src/rust/frb_generated.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();
  testWidgets(
    'native account local creation is pending without record service',
    (tester) async {
      await RustLib.init();
      final root = await Directory.systemTemp.createTemp(
        'chat-account-native-',
      );
      await account.testStorageStart(dataDir: root.path);
      final core = FrbCoreClient(dataDir: root.path);
      final model = AccountModel(core.account);
      await model.open();
      expect(model.snapshot!.phase, account.AccountPhase.unconfigured);
      expect(await model.pairConfirmCode(Uint8List(16), true), false);
      expect(model.errorCode, rust.ErrorCode.pairExpired);
      expect(model.error, '연결 시간이 지났어요. 새 QR로 다시 연결해 주세요.');
      expect(await model.pairCancel(Uint8List(16)), true);
      final events = <rust.ChatEvent>[];
      final subscription = core.events().listen(events.add);
      final display = await model.beginCreate();
      expect(display != null && display.words.length == 24, true);
      final answers = display!.positions
          .map((p) => display.words[p - 1])
          .toList();
      expect(await model.confirmCreate(display.flow, answers), true);
      answers.clear();
      expect(model.recoveryDisplay, isNull);
      final view = await core.account.snapshot();
      expect(
        [
          account.AccountPhase.localReady,
          account.AccountPhase.publishPending,
        ].contains(view.phase),
        true,
      );
      expect(view.publicPeerId != null, true);
      final invite = await core.account.createInvite('공개 초대 만료 시험');
      final preview = await core.account.parseInvite(
        invite,
        BigInt.from(DateTime.now().millisecondsSinceEpoch ~/ 1000),
      );
      expect(await model.parseInvite(invite, preview.expiresAt), isNull);
      expect(model.errorCode, rust.ErrorCode.inviteExpired);
      expect(model.error, '초대가 만료됐어요. 새 초대를 받아 주세요.');
      expect(await model.beginCreate(), isNull);
      expect(model.errorCode, rust.ErrorCode.accountExists);
      expect(
        events.whereType<rust.ChatEvent_AccountChanged>().isNotEmpty,
        true,
      );
      for (final event in events) {
        expect(event.toString().contains(display.words.join(' ')), false);
      }
      await model.close();
      await subscription.cancel();
    },
  );
}
