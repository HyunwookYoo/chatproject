import 'dart:async';
import 'package:flutter/foundation.dart';
import '../src/rust/api/account.dart' as account;
import '../src/rust/api/chat.dart' as rust;
import 'account_client.dart';
import 'error_messages.dart';
export 'error_messages.dart' show accountErrorMessage;

/// Public projection ordering and route-scoped ephemeral response ownership.
/// Dart strings and generated transit buffers do not support guaranteed RAM erasure.
class AccountModel extends ChangeNotifier {
  AccountModel(this.client);
  final AccountClient client;
  StreamSubscription<rust.ChatEvent>? _subscription;
  int _generation = 0;
  int _secretGeneration = 0;
  bool _closed = true;
  BigInt? _revision;
  account.AccountView? snapshot;
  account.RecoveryDisplay? recoveryDisplay;
  account.PairDisplay? pairDisplay;
  rust.ErrorCode? errorCode;
  bool busy = false;
  String? get error =>
      errorCode == null ? null : accountErrorMessage(errorCode!);

  Future<void> open() async {
    final generation = ++_generation;
    _closed = false;
    final previous = _subscription;
    if (previous != null) await previous.cancel();
    if (!_current(generation)) return;
    _subscription = client.events().listen(
      (event) {
        if (!_current(generation)) return;
        if (event case rust.ChatEvent_AccountChanged(
          :final revision,
          :final view,
        )) {
          if (revision == view.revision) applySnapshot(view);
        }
      },
      onError: (Object error) {
        if (_current(generation)) _failure(error);
      },
      onDone: () {
        if (_current(generation)) _failure(null);
      },
    );
    try {
      await client.openAccount();
      if (!_current(generation)) return;
      final next = await client.snapshot();
      if (_current(generation)) applySnapshot(next);
    } catch (error) {
      if (_current(generation)) _failure(error, opening: true);
    }
  }

  bool _current(int generation) => !_closed && generation == _generation;
  void applySnapshot(account.AccountView next) => _applySnapshot(next);
  void _applySnapshot(account.AccountView next, {bool retry = false}) {
    if (_closed ||
        (_revision != null &&
            (next.revision < _revision! ||
                (next.revision == _revision! && !retry)))) {
      return;
    }
    _revision = next.revision;
    snapshot = next;
    errorCode = next.lockError;
    if (next.phase == account.AccountPhase.locked) clearSecrets();
    notifyListeners();
  }

  void clearSecrets() {
    ++_secretGeneration;
    recoveryDisplay = null;
    pairDisplay = null;
  }

  void _failure(Object? error, {bool opening = false}) {
    errorCode = error is rust.CoreError
        ? error.code
        : rust.ErrorCode.notAvailable;
    if (opening || _storageError(errorCode!)) {
      clearSecrets();
      snapshot = lockedAccountView(
        _revision ?? BigInt.zero,
        errorCode!,
        previous: snapshot,
      );
    }
    notifyListeners();
  }

  Future<T?> _result<T>(
    Future<T> Function() operation, {
    bool allowLocked = false,
  }) async {
    if (_closed ||
        busy ||
        (!allowLocked && snapshot?.phase == account.AccountPhase.locked)) {
      return null;
    }
    final generation = _generation;
    busy = true;
    errorCode = null;
    notifyListeners();
    try {
      final result = await operation();
      if (!_current(generation)) return null;
      return result;
    } catch (error) {
      if (_current(generation)) _failure(error);
      return null;
    } finally {
      if (_current(generation)) {
        busy = false;
        notifyListeners();
      }
    }
  }

  Future<bool> _command(Future<void> Function() operation) async =>
      await _result(() async {
        await operation();
        return true;
      }) ??
      false;
  Future<account.RecoveryDisplay?> beginCreate() async {
    final generation = _generation;
    final secretGeneration = _secretGeneration;
    final reply = await _result(client.beginCreate);
    if (!_current(generation) || reply == null) return null;
    if (secretGeneration != _secretGeneration) {
      // Cancel only the abandoned reply's exact flow. Do not clear secrets or
      // publish an error into a replacement route if native cleanup fails.
      try {
        await client.cancelCreate(reply.flow);
      } catch (_) {
        // The next coherent projection remains authoritative; no success claim.
      }
      return null;
    }
    recoveryDisplay = reply;
    pairDisplay = null;
    notifyListeners();
    return reply;
  }

  Future<account.PairDisplay?> pairBegin(
    account.PairDirection direction, {
    Uint8List? resume,
  }) async {
    final generation = _generation;
    final secretGeneration = _secretGeneration;
    final reply = await _result(() => client.pairBegin(direction, resume));
    if (!_current(generation) || reply == null) return null;
    if (secretGeneration != _secretGeneration) {
      await cancelAbandonedPair(reply.flow);
      return null;
    }
    pairDisplay = reply;
    recoveryDisplay = null;
    notifyListeners();
    return reply;
  }

  Future<bool> confirmCreate(List<int> flow, List<String> answers) {
    clearSecrets();
    return _command(() => client.confirmCreate(flow, answers));
  }

  Future<bool> cancelCreate(List<int> flow) {
    clearSecrets();
    return _command(() => client.cancelCreate(flow));
  }

  /// Only abandoned confirmation cleanup may wait past the normal busy guard.
  /// Never clears or reports errors into a replacement route's state.
  Future<bool> cancelAbandonedCreate(
    List<int> flow, {
    required Future<bool> afterConfirmation,
  }) async {
    final generation = _generation;
    final capturedFlow = List<int>.of(flow);
    if (await afterConfirmation) return true; // Durable confirmation won.
    if (!_current(generation)) return false;
    try {
      await client.cancelCreate(capturedFlow);
      return true;
    } catch (_) {
      return false;
    }
  }

  Future<bool> recover(String words, bool confirmReplacement) {
    clearSecrets();
    return _command(() => client.recover(words, confirmReplacement));
  }

  Future<account.CandidateInfo?> prepareCandidate() =>
      _result(client.prepareCandidate);
  Future<bool> discardPendingImport() async =>
      await _result(() async {
        await client.discardPendingImport();
        return true;
      }, allowLocked: true) ??
      false;
  Future<String?> createInvite(String? name) =>
      _result(() => client.createInvite(name));
  Future<account.ContactView?> acceptInvite(String input, String alias) =>
      _result(() => client.acceptInvite(input, alias));
  Future<account.InvitePreview?> parseInvite(String input, BigInt now) =>
      _result(() => client.parseInvite(input, now));
  Future<account.SafetyDisplay?> safetyDisplay(
    List<int> local,
    List<int> peer,
  ) => _result(() => client.safetyDisplay(local, peer));
  Future<bool> confirmSafety(List<int> peer, List<int> digest) =>
      _command(() => client.confirmSafety(peer, digest));
  Future<bool> connectAccount(List<int> peer) =>
      _command(() => client.connectAccount(peer));
  Future<bool> refreshRecord(List<int> peer) =>
      _command(() => client.refreshRecord(peer));
  Future<bool> revokeDevice(List<int> target) =>
      _command(() => client.revokeDevice(target));
  Future<bool> confirmRecordConflict(List<int> baseHash) =>
      _command(() => client.confirmRecordConflict(baseHash));
  Future<Uint8List?> pairScan(String qr, {List<int>? expectedResume}) async {
    final generation = _generation;
    final secretGeneration = _secretGeneration;
    final flow = await _result(() async {
      final scanned = await client.pairScan(qr);
      if (expectedResume == null) return scanned;
      try {
        final next = await client.snapshot();
        if (_current(generation) && secretGeneration == _secretGeneration) {
          applySnapshot(next);
          final pair = snapshot?.pair;
          if (pair != null &&
              listEquals(pair.flow, scanned) &&
              listEquals(pair.transaction, expectedResume)) {
            return scanned;
          }
        }
      } catch (_) {
        // No approval on missing coherent metadata; no error in a newer route.
      }
      await cancelAbandonedPair(scanned);
      return null;
    });
    if (!_current(generation) || flow == null) return null;
    if (secretGeneration != _secretGeneration) {
      await cancelAbandonedPair(flow);
      return null;
    }
    return flow;
  }

  /// Exact abandoned pair only: cancellation cannot undo durable disclosure.
  /// No secret clearing or error publication into a replacement route.
  Future<bool> cancelAbandonedPair(
    List<int> flow, {
    Future<Object?>? afterOperation,
  }) async {
    final generation = _generation;
    final captured = List<int>.of(flow);
    if (afterOperation != null) await afterOperation;
    if (!_current(generation)) return false;
    try {
      await client.pairCancel(captured);
      return true;
    } catch (_) {
      return false;
    }
  }

  Future<bool> pairConfirmCode(List<int> flow, bool matches) =>
      _command(() => client.pairConfirmCode(flow, matches));
  Future<bool> pairApprove(List<int> flow) =>
      _command(() => client.pairApprove(flow));
  Future<bool> pairCancel(List<int> flow) {
    clearSecrets();
    return _command(() => client.pairCancel(flow));
  }

  Future<void> retry() async {
    if (_closed || busy) return;
    final generation = _generation;
    busy = true;
    notifyListeners();
    try {
      final next = await client.retryOpen();
      if (_current(generation)) _applySnapshot(next, retry: true);
    } catch (error) {
      if (_current(generation)) _failure(error, opening: true);
    } finally {
      if (_current(generation)) {
        busy = false;
        notifyListeners();
      }
    }
  }

  Future<void> close() async {
    _closed = true;
    ++_generation;
    busy = false;
    clearSecrets();
    snapshot = null;
    _revision = null;
    errorCode = null;
    final subscription = _subscription;
    _subscription = null;
    await subscription?.cancel();
  }

  @override
  void dispose() {
    unawaited(close());
    super.dispose();
  }
}

bool _storageError(rust.ErrorCode code) => switch (code) {
  rust.ErrorCode.storageLocked ||
  rust.ErrorCode.storageMissing ||
  rust.ErrorCode.storageCorrupt ||
  rust.ErrorCode.writerBusy => true,
  _ => false,
};
account.AccountView lockedAccountView(
  BigInt revision,
  rust.ErrorCode code, {
  account.AccountView? previous,
}) => account.AccountView(
  revision: revision,
  phase: account.AccountPhase.locked,
  lockError: code,
  devices: [],
  contacts: [],
  publishIntents: [],
  disclosurePossible: previous?.disclosurePossible ?? false,
  pendingPairTransactions: previous?.pendingPairTransactions ?? [],
  pairResumes: previous?.pairResumes ?? [],
  pendingImportRequiresRemoteRevoke:
      previous?.pendingImportRequiresRemoteRevoke ?? false,
  publication: account.PublicationView(
    state: account.PublicationState.localBlocked,
    observedSeq: BigInt.zero,
    allocatedSeq: BigInt.zero,
    tombstones: [],
    pendingCount: 0,
    pendingNotifications: [],
  ),
);
