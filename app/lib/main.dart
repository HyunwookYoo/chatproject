import 'dart:async';

import 'package:flutter/foundation.dart' show listEquals;
import 'package:flutter/material.dart';

import 'bootstrap.dart';
import 'account/account_model.dart';
import 'account/account_routes.dart';
import 'account/invite_links.dart';
import 'core_client.dart';
import 'home/home_screen.dart';
import 'mailbox/mailbox_client.dart';
import 'mailbox/mailbox_model.dart';
import 'mailbox/mailbox_platform.dart';
import 'mailbox/notifier.dart';
import 'message/message_client.dart';
import 'message/message_model.dart';
import 'message/screens/conversation_screen.dart';
import 'network/network_client.dart';
import 'push/push_client.dart';
import 'push/push_model.dart';
import 'push/push_platform.dart';
import 'src/rust/frb_generated.dart';
import 'theme/app_theme.dart';

Future<void> main(List<String> arguments) async {
  WidgetsFlutterBinding.ensureInitialized();
  for (final argument in arguments) {
    if (InviteLinks.isPublicInvite(argument)) InviteLinks.initial = argument;
  }
  runApp(
    await bootstrap(
      arguments.where((value) => !value.startsWith('chatproject:')).toList(),
      initializeRust: RustLib.init,
      createClient: (dataDir) => FrbCoreClient(dataDir: dataDir),
    ),
  );
}

class ChatApp extends StatefulWidget {
  const ChatApp({
    super.key,
    required this.core,
    this.network,
    this.account,
    this.messages,
    this.mailbox,
    this.mailboxPlatform = const ChannelMailboxPlatform(),
    this.push,
    this.pushHost = const ChannelPushHost(),
    this.pushUrl,
  });

  final CoreClient core;
  final AccountModel? account;
  final MessageClient? messages;
  final MailboxClient? mailbox;
  final MailboxPlatform mailboxPlatform;
  final NetworkClient? network;
  final PushClient? push;
  final PushHost pushHost;

  /// Debug `--push-url`.
  final String? pushUrl;

  @override
  State<ChatApp> createState() => _ChatAppState();
}

class _ChatAppState extends State<ChatApp> {
  bool _roomOpen = false;
  final _navigator = GlobalKey<NavigatorState>();
  MailboxModel? _mailbox;
  Notifier? _notifier;
  PushModel? _push;

  @override
  void initState() {
    super.initState();
    final account = widget.account;
    if (account == null) return;
    if (widget.push case final PushClient push) {
      _push = PushModel(push, widget.pushHost, proxyUrl: widget.pushUrl)
        ..onOpen = (conversation) =>
            unawaited(_openNotification(notificationId(conversation)));
    }
    final client = widget.mailbox;
    if (client == null) return;
    final platform = widget.mailboxPlatform;
    final mailbox = _mailbox = MailboxModel(client, platform);
    final messages = widget.messages;
    // Android's notifications are Kotlin's, with or without Flutter (M7 spec
    // 5.5); posting from Dart too would alert twice.
    if (messages != null && !MailboxModel.android) {
      _notifier =
          Notifier(
              events: messages.events(),
              platform: platform,
              aliasOf: (peer) => account.snapshot?.contacts
                  .where((c) => listEquals(c.peerId, peer))
                  .firstOrNull
                  ?.alias,
              findPeer: (id) => conversationPeer(messages, id),
            )
            ..onOpen = (peer) {
              final context = _navigator.currentContext;
              if (context != null) unawaited(_openConversation(context, peer));
            };
    }
    platform.listen(
      onExitRequested: mailbox.exit,
      onOpenConversation: (id) => unawaited(_openNotification(id)),
    );
  }

  @override
  void dispose() {
    unawaited(_notifier?.dispose());
    _mailbox?.dispose();
    _push?.dispose();
    super.dispose();
  }

  /// A tapped notification's conversation (its notification id, untrusted).
  Future<void> _openNotification(int id) async {
    final notifier = _notifier;
    if (notifier != null) return notifier.open(id);
    final messages = widget.messages;
    if (messages == null) return;
    final peer = await conversationPeer(messages, id);
    final context = _navigator.currentContext;
    if (peer != null && context != null && context.mounted) {
      await _openConversation(context, peer);
    }
  }

  Future<void> _openConversation(BuildContext context, List<int> peer) async {
    final account = widget.account;
    final messages = widget.messages;
    if (_roomOpen || account == null || messages == null) return;
    _roomOpen = true;
    _notifier?.openPeer = List.of(peer);
    try {
      await Navigator.of(context).push(
        MaterialPageRoute<void>(
          builder: (_) => _ConversationRoute(
            account: account,
            messages: messages,
            peer: List.of(peer),
            onShown: _push?.conversationShown,
          ),
        ),
      );
    } finally {
      _roomOpen = false;
      _notifier?.openPeer = null;
      _push?.conversationShown(null);
    }
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      navigatorKey: _navigator,
      title: 'ChatProject',
      theme: buildAppTheme(),
      home: Builder(
        builder: (context) {
          final home = HomeScreen(
            core: widget.core,
            network: widget.network,
            account: widget.account,
            messages: widget.messages,
            onOpenConversation: (peer) => _openConversation(context, peer),
          );
          return widget.account == null
              ? home
              : AccountRoutes(
                  model: widget.account!,
                  mailbox: _mailbox,
                  push: _push,
                  onOpenConversation: widget.messages == null
                      ? null
                      : (peer) => _openConversation(context, peer),
                  child: home,
                );
        },
      ),
    );
  }
}

/// One active room owns a disposable projection, borrowing the opened account
/// and the core's cached message client/event broadcast. No core/account close.
class _ConversationRoute extends StatefulWidget {
  const _ConversationRoute({
    required this.account,
    required this.messages,
    required this.peer,
    this.onShown,
  });
  final AccountModel account;
  final MessageClient messages;
  final List<int> peer;

  /// The id of the conversation on screen, once it is known.
  final ValueChanged<List<int>?>? onShown;
  @override
  State<_ConversationRoute> createState() => _ConversationRouteState();
}

class _ConversationRouteState extends State<_ConversationRoute> {
  late final _model = MessageModel(widget.messages, account: widget.account);
  List<int>? _shown;

  @override
  void initState() {
    super.initState();
    _model.addListener(_conversationChanged);
  }

  void _conversationChanged() {
    final id = _model.currentConversation?.id;
    if (id == null || listEquals(id, _shown)) return;
    _shown = List.of(id);
    widget.onShown?.call(_shown);
  }

  @override
  Widget build(BuildContext context) =>
      ConversationScreen(model: _model, peer: widget.peer);
  @override
  void dispose() {
    _model.removeListener(_conversationChanged);
    _model.dispose();
    super.dispose();
  }
}
