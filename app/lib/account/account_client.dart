import 'dart:typed_data';
import '../core_client.dart';
import '../src/rust/api/account.dart' as account;
import '../src/rust/api/chat.dart' as rust;

abstract interface class AccountClient {
  Stream<rust.ChatEvent> events();
  Future<void> openAccount();
  Future<account.AccountView> snapshot();
  Future<account.AccountView> retryOpen();
  Future<account.CandidateInfo> prepareCandidate();
  Future<void> discardPendingImport();
  Future<account.RecoveryDisplay> beginCreate();
  Future<void> confirmCreate(List<int> flow, List<String> answers);
  Future<void> cancelCreate(List<int> flow);
  Future<void> recover(String words, bool confirmReplacement);
  Future<String> createInvite(String? name);
  Future<account.ContactView> acceptInvite(String input, String alias);
  Future<List<account.ContactView>> contacts();
  Future<void> confirmSafety(List<int> peer, List<int> digest);
  Future<void> connectAccount(List<int> peer);
  Future<void> refreshRecord(List<int> peer);
  Future<void> revokeDevice(List<int> target);
  Future<void> confirmRecordConflict(List<int> baseHash);
  Future<void> confirmRemoteRecord(List<int> peer, List<int> hash);
  Future<account.PairDisplay> pairBegin(
    account.PairDirection direction,
    Uint8List? resume,
  );
  Future<Uint8List> pairScan(String qr);
  Future<void> pairConfirmCode(List<int> flow, bool matches);
  Future<void> pairApprove(List<int> flow);
  Future<void> pairCancel(List<int> flow);
  Future<account.PairView?> pairView();
  Future<account.InvitePreview> parseInvite(String input, BigInt now);
  Future<account.SafetyDisplay> safetyDisplay(List<int> local, List<int> peer);
}

/// Uses the same broadcast stream as network diagnostics, never a second Rust subscription.
class FrbAccountClient implements AccountClient {
  FrbAccountClient(this.core);
  final CoreClient core;
  @override
  Stream<rust.ChatEvent> events() => core.events();
  @override
  Future<void> openAccount() => account.openAccount();
  @override
  Future<account.AccountView> snapshot() => account.snapshot();
  @override
  Future<account.AccountView> retryOpen() => account.retryOpen();
  @override
  Future<account.CandidateInfo> prepareCandidate() =>
      account.prepareCandidate();
  @override
  Future<void> discardPendingImport() => account.discardPendingImport();
  @override
  Future<account.RecoveryDisplay> beginCreate() => account.beginCreate();
  @override
  Future<void> confirmCreate(List<int> flow, List<String> answers) =>
      account.confirmCreate(flow: flow, answers: answers);
  @override
  Future<void> cancelCreate(List<int> flow) => account.cancelCreate(flow: flow);
  @override
  Future<void> recover(String words, bool confirmReplacement) =>
      account.recover(words: words, confirmReplacement: confirmReplacement);
  @override
  Future<String> createInvite(String? name) => account.createInvite(name: name);
  @override
  Future<account.ContactView> acceptInvite(String input, String alias) =>
      account.acceptInvite(input: input, alias: alias);
  @override
  Future<List<account.ContactView>> contacts() => account.contacts();
  @override
  Future<void> confirmSafety(List<int> peer, List<int> digest) =>
      account.confirmSafety(peer: peer, digest: digest);
  @override
  Future<void> connectAccount(List<int> peer) =>
      account.connectAccount(peer: peer);
  @override
  Future<void> refreshRecord(List<int> peer) =>
      account.refreshRecord(peer: peer);
  @override
  Future<void> revokeDevice(List<int> target) =>
      account.revokeDevice(target: target);
  @override
  Future<void> confirmRecordConflict(List<int> baseHash) =>
      account.confirmRecordConflict(baseHash: baseHash);
  @override
  Future<void> confirmRemoteRecord(List<int> peer, List<int> hash) =>
      account.confirmRemoteRecord(peer: peer, hash: hash);
  @override
  Future<account.PairDisplay> pairBegin(
    account.PairDirection direction,
    Uint8List? resume,
  ) => account.pairBegin(direction: direction, resume: resume);
  @override
  Future<Uint8List> pairScan(String qr) => account.pairScan(qr: qr);
  @override
  Future<void> pairConfirmCode(List<int> flow, bool matches) =>
      account.pairConfirmCode(flow: flow, matches: matches);
  @override
  Future<void> pairApprove(List<int> flow) => account.pairApprove(flow: flow);
  @override
  Future<void> pairCancel(List<int> flow) => account.pairCancel(flow: flow);
  @override
  Future<account.PairView?> pairView() => account.pairView();
  @override
  Future<account.InvitePreview> parseInvite(String input, BigInt now) =>
      account.parseInvite(input: input, now: now);
  @override
  Future<account.SafetyDisplay> safetyDisplay(
    List<int> local,
    List<int> peer,
  ) => account.safetyDisplay(local: local, peer: peer);
}
