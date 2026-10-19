import 'dart:async';
import 'dart:typed_data';
import 'package:chat_app/account/account_client.dart';
import 'package:chat_app/account/account_model.dart';
import 'package:chat_app/src/rust/api/account.dart' as account;
import 'package:chat_app/src/rust/api/chat.dart' as rust;
import 'package:flutter_test/flutter_test.dart';

account.AccountView view(
  BigInt revision, {
  account.AccountPhase phase = account.AccountPhase.unconfigured,
}) => account.AccountView(
  revision: revision,
  phase: phase,
  devices: [],
  contacts: [],
  remoteRecords: [],
  publishIntents: [],
  publication: account.PublicationView(
    state: account.PublicationState.localBlocked,
    observedSeq: BigInt.zero,
    allocatedSeq: BigInt.zero,
    tombstones: [],
    pendingCount: 0,
    pendingNotifications: [],
  ),
  disclosurePossible: false,
  pendingPairTransactions: [],
  pairResumes: [],
  pendingImportRequiresRemoteRevoke: false,
);

class FakeAccountClient implements AccountClient {
  final controller = StreamController<rust.ChatEvent>.broadcast(sync: true);
  Future<account.AccountView>? delayed;
  final secret = Completer<account.RecoveryDisplay>();
  int begins = 0;
  Object? openError;
  Object? commandError;
  @override
  Future<account.CandidateInfo> prepareCandidate() async {
    throw commandError!;
  }

  @override
  Future<account.AccountView> retryOpen() => snapshot();
  @override
  Stream<rust.ChatEvent> events() => controller.stream;
  @override
  Future<void> openAccount() async {
    if (openError != null) throw openError!;
  }

  @override
  Future<account.AccountView> snapshot() =>
      delayed ?? Future.value(view(BigInt.one));
  @override
  Future<account.RecoveryDisplay> beginCreate() {
    begins++;
    return secret.future;
  }

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

void main() {
  test(
    'clearing a flow invalidates its pending secret response and suppresses duplicate taps',
    () async {
      final client = FakeAccountClient();
      final model = AccountModel(client);
      await model.open();
      final pending = model.beginCreate();
      expect(await model.beginCreate(), isNull);
      expect(client.begins, 1);
      model.clearSecrets();
      client.secret.complete(
        account.RecoveryDisplay(
          flow: Uint8List(16),
          words: ['private fixture'],
          positions: Uint8List.fromList([1, 2, 3]),
        ),
      );
      expect(await pending, isNull);
      expect(model.recoveryDisplay, isNull);
      await model.close();
      await client.controller.close();
    },
  );
  test(
    'explicit retry can restore an unchanged authoritative revision after a local locked error',
    () async {
      final client = FakeAccountClient()
        ..commandError = const rust.CoreError(
          code: rust.ErrorCode.storageLocked,
          detail: 'private status',
        );
      final model = AccountModel(client);
      await model.open();
      await model.prepareCandidate();
      expect(model.snapshot!.phase, account.AccountPhase.locked);
      await model.retry();
      expect(model.snapshot!.phase, account.AccountPhase.unconfigured);
      expect(model.errorCode, isNull);
      await model.close();
      await client.controller.close();
    },
  );
  test('subscribe_before_snapshot_keeps_newer_event', () async {
    final client = FakeAccountClient();
    final delayed = Completer<account.AccountView>();
    client.delayed = delayed.future;
    final model = AccountModel(client);
    final opening = model.open();
    expect(client.controller.hasListener, isTrue);
    client.controller.add(
      rust.ChatEvent.accountChanged(
        revision: BigInt.from(9),
        view: view(BigInt.from(9), phase: account.AccountPhase.active),
      ),
    );
    delayed.complete(view(BigInt.from(8)));
    await opening;
    expect(model.snapshot!.revision, BigInt.from(9));
    expect(model.snapshot!.phase, account.AccountPhase.active);
    await model.close();
    await client.controller.close();
  });
  test('duplicate_revision_no_rollback', () async {
    final client = FakeAccountClient();
    final model = AccountModel(client);
    await model.open();
    final revision = (BigInt.one << 63) + BigInt.from(17);
    model.applySnapshot(view(revision, phase: account.AccountPhase.active));
    model.applySnapshot(view(revision));
    model.applySnapshot(view(revision - BigInt.one));
    expect(model.snapshot!.revision, revision);
    expect(model.snapshot!.phase, account.AccountPhase.active);
    await model.close();
    await client.controller.close();
  });
  test('closed_flow_ignores_late_secret_reply', () async {
    final client = FakeAccountClient();
    final model = AccountModel(client);
    await model.open();
    final pending = model.beginCreate();
    await model.close();
    client.secret.complete(
      account.RecoveryDisplay(
        flow: Uint8List(16),
        words: ['fixture private words'],
        positions: Uint8List.fromList([1, 2, 3]),
      ),
    );
    expect(await pending, isNull);
    expect(model.recoveryDisplay, isNull);
    expect(client.controller.hasListener, isFalse);
    await client.controller.close();
  });
}
