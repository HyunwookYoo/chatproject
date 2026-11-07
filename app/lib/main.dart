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
import 'probe/probe_host.dart';
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
    this.probe,
    this.network,
    this.account,
    this.messages,
    this.mailbox,
    this.mailboxPlatform = const ChannelMailboxPlatform(),
  });

  final CoreClient core;
  final AccountModel? account;
  final MessageClient? messages;
  final MailboxClient? mailbox;
  final MailboxPlatform mailboxPlatform;
  final ProbeHost? probe;
  final NetworkClient? network;

  @override
  State<ChatApp> createState() => _ChatAppState();
}

class _ChatAppState extends State<ChatApp> {
  bool _roomOpen = false;
  final _navigator = GlobalKey<NavigatorState>();
  MailboxModel? _mailbox;
  Notifier? _notifier;

  @override
  void initState() {
    super.initState();
    final client = widget.mailbox;
    final account = widget.account;
    if (client == null || account == null) return;
    final platform = widget.mailboxPlatform;
    final mailbox = _mailbox = MailboxModel(client, platform);
    final messages = widget.messages;
    if (messages != null) {
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
      onOpenConversation: (id) => unawaited(_notifier?.open(id)),
    );
  }

  @override
  void dispose() {
    unawaited(_notifier?.dispose());
    _mailbox?.dispose();
    super.dispose();
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
          ),
        ),
      );
    } finally {
      _roomOpen = false;
      _notifier?.openPeer = null;
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
            probe: widget.probe,
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
  });
  final AccountModel account;
  final MessageClient messages;
  final List<int> peer;
  @override
  State<_ConversationRoute> createState() => _ConversationRouteState();
}

class _ConversationRouteState extends State<_ConversationRoute> {
  late final _model = MessageModel(widget.messages, account: widget.account);
  @override
  Widget build(BuildContext context) =>
      ConversationScreen(model: _model, peer: widget.peer);
  @override
  void dispose() {
    _model.dispose();
    super.dispose();
  }
}
