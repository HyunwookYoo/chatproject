import 'dart:async';
import 'package:chat_app/account/account_model.dart';
import 'package:chat_app/core_client.dart';
import 'package:chat_app/src/rust/api/account.dart' as account;
import 'package:chat_app/src/rust/api/chat.dart' as rust;
import 'package:chat_app/src/rust/frb_generated.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';
import 'account_model_test.dart' show view;

// Public test markers only: not valid recovery words or a pairing QR.
const _wordsMarker = 'PUBLIC_TEST_RECOVERY_REPLY_MARKER';
const _qrMarker = 'PUBLIC_TEST_PAIR_REPLY_MARKER';

class _SecretReplyApi implements RustLibApi {
  final controller = StreamController<rust.ChatEvent>.broadcast(sync: true);
  int begins = 0;
  int pairs = 0;
  int subscriptions = 0;
  @override
  Future<void> crateApiAccountOpenAccount() async {}
  @override
  Future<account.AccountView> crateApiAccountSnapshot() async =>
      view(BigInt.one);
  @override
  Stream<rust.ChatEvent> crateApiChatEvents() {
    subscriptions++;
    return controller.stream;
  }

  void _publicChange(int revision) {
    final snapshot = view(BigInt.from(revision));
    controller.add(
      rust.ChatEvent.accountChanged(
        revision: snapshot.revision,
        view: snapshot,
      ),
    );
  }

  @override
  Future<account.RecoveryDisplay> crateApiAccountBeginCreate() async {
    begins++;
    _publicChange(2);
    return account.RecoveryDisplay(
      flow: Uint8List(16),
      words: [_wordsMarker],
      positions: Uint8List.fromList([1, 2, 3]),
    );
  }

  @override
  Future<account.PairDisplay> crateApiAccountPairBegin({
    required account.PairDirection direction,
    Uint8List? resume,
  }) async {
    pairs++;
    _publicChange(3);
    return account.PairDisplay(
      flow: Uint8List(16),
      qr: _qrMarker,
      expiresAt: BigInt.from(100),
    );
  }

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

void main() {
  test('secret_replies_not_in_events', () async {
    // Generated Dart entry wrappers + real FrbAccountClient forwarding. Rust/native
    // serialization secrecy remains separately covered by core/integration tests.
    final api = _SecretReplyApi();
    RustLib.initMock(api: api);
    addTearDown(RustLib.dispose);
    final diagnostics = <String?>[];
    final previousPrint = debugPrint;
    debugPrint = (message, {wrapWidth}) => diagnostics.add(message);
    addTearDown(() => debugPrint = previousPrint);
    final core = FrbCoreClient(dataDir: 'unused-public-test-path');
    final model = AccountModel(core.account);
    final events = <rust.ChatEvent>[];
    final sub = core.events().listen(events.add);
    await model.open();
    final recovery = await model.beginCreate();
    expect(api.begins, 1);
    expect(recovery!.words, [_wordsMarker]);
    expect(model.recoveryDisplay, same(recovery));
    final pair = await model.pairBegin(account.PairDirection.newDisplays);
    expect(api.pairs, 1);
    expect(pair!.qr, _qrMarker);
    expect(model.pairDisplay, same(pair));
    expect(model.recoveryDisplay, isNull);
    expect(api.subscriptions, 1);
    expect(model.snapshot!.revision, BigInt.from(3));
    expect(events, hasLength(2));
    expect(events, everyElement(isA<rust.ChatEvent_AccountChanged>()));
    final ordinaryOutput = [
      ...events.map((event) => event.toString()),
      model.snapshot.toString(),
      model.toString(),
      recovery.toString(),
      pair.toString(),
      model.error,
      ...diagnostics,
    ].join('\n');
    expect(ordinaryOutput, isNot(contains(_wordsMarker)));
    expect(ordinaryOutput, isNot(contains(_qrMarker)));
    await model.close();
    expect(model.recoveryDisplay, isNull);
    expect(model.pairDisplay, isNull);
    await sub.cancel();
    await api.controller.close();
  });
}
