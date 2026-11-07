import 'dart:async';
import 'dart:typed_data';

import 'package:chat_app/account/account_client.dart';
import 'package:chat_app/mailbox/mailbox_client.dart';
import 'package:chat_app/mailbox/mailbox_platform.dart';
import 'package:chat_app/src/rust/api/account.dart' as a;
import 'package:chat_app/src/rust/api/chat.dart' as r;

/// The core's mailbox calls, recorded.
class FakeMailboxClient implements MailboxClient {
  bool enabled = false;
  bool firstLinkShown = false;
  r.ErrorCode? failure;
  Completer<void>? backgroundWait;
  final sets = <bool>[];
  int states = 0, marks = 0, foregrounds = 0, backgrounds = 0;

  void _check() {
    if (failure != null) {
      throw r.CoreError(code: failure!, detail: 'fixture failure');
    }
  }

  @override
  Future<MailboxStateView> mailboxState() async {
    states++;
    _check();
    return MailboxStateView(
      enabled: enabled,
      firstLinkShown: firstLinkShown,
      uptime7DPermille: 0,
    );
  }

  @override
  Future<void> setMailbox(bool enabled) async {
    sets.add(enabled);
    _check();
    this.enabled = enabled;
  }

  @override
  Future<void> markFirstLinkShown() async {
    marks++;
    _check();
    firstLinkShown = true;
  }

  @override
  Future<void> mailboxForeground() async {
    foregrounds++;
    _check();
  }

  @override
  Future<int> mailboxBackground() async {
    backgrounds++;
    await backgroundWait?.future;
    _check();
    return 0;
  }
}

/// The `chat_app/mailbox` channel, recorded in call order.
class FakeMailboxPlatform implements MailboxPlatform {
  /// What `setMailbox(true)` answers (Android: false unless the app is on screen).
  bool mailboxResult = true;
  bool autostartValue = false;
  bool permissionResult = true;
  final calls = <String>[];
  final notifications = <({int id, String title, String body, bool update})>[];
  final cancelled = <int>[];
  Future<void> Function()? onExitRequested;
  void Function(int)? onOpenConversation;

  @override
  Future<bool> setMailbox(bool on) async {
    calls.add('setMailbox:$on');
    return on ? mailboxResult : true;
  }

  @override
  Future<bool> setAutostart(bool on) async {
    calls.add('setAutostart:$on');
    autostartValue = on;
    return true;
  }

  @override
  Future<bool> autostart() async => autostartValue;

  @override
  Future<void> notify({
    required int id,
    required String title,
    required String body,
    bool update = false,
  }) async =>
      notifications.add((id: id, title: title, body: body, update: update));

  @override
  Future<void> cancelNotification(int id) async => cancelled.add(id);

  @override
  Future<void> requestBatteryExemption() async =>
      calls.add('requestBatteryExemption');

  @override
  Future<bool> requestNotificationPermission() async {
    calls.add('requestNotificationPermission');
    return permissionResult;
  }

  @override
  Future<void> quit() async => calls.add('quit');

  @override
  void listen({
    required Future<void> Function() onExitRequested,
    required void Function(int id) onOpenConversation,
  }) {
    this.onExitRequested = onExitRequested;
    this.onOpenConversation = onOpenConversation;
  }
}

/// An account that is Active with the given contacts; nothing else is called.
class ActiveAccountClient implements AccountClient {
  ActiveAccountClient([this.saved = const []]);
  final List<a.ContactView> saved;
  final controller = StreamController<r.ChatEvent>.broadcast(sync: true);

  a.AccountView view() => a.AccountView(
    revision: BigInt.one,
    phase: a.AccountPhase.active,
    publicPeerId: Uint8List(32),
    devices: [],
    contacts: saved,
    remoteRecords: [],
    publishIntents: [],
    publication: a.PublicationView(
      canRepublishAccepted: false,
      state: a.PublicationState.observed,
      observedSeq: BigInt.one,
      allocatedSeq: BigInt.one,
      tombstones: [],
      pendingCount: 0,
      pendingNotifications: [],
    ),
    disclosurePossible: false,
    pendingPairTransactions: [],
    pairResumes: [],
    pendingImportRequiresRemoteRevoke: false,
  );

  @override
  Stream<r.ChatEvent> events() => controller.stream;
  @override
  Future<void> openAccount() async {}
  @override
  Future<a.AccountView> snapshot() async => view();
  @override
  dynamic noSuchMethod(Invocation invocation) =>
      throw UnsupportedError('not used by mailbox tests');
}
