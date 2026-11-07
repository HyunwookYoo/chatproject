import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';

import '../account/screens/contact_preview_screen.dart' show isolatedName;
import '../message/message_bridge.dart' show ConversationCursor;
import '../message/message_client.dart';
import '../src/rust/api/chat.dart' as rust;
import 'mailbox_platform.dart';

/// A conversation's notification id: its first 4 bytes as a signed
/// big-endian int32 (both platforms take a 32-bit int).
int notificationId(List<int> conversation) => ByteData.sublistView(
  Uint8List.fromList(conversation.take(4).toList()),
).getInt32(0);

/// The peer of the conversation whose notification id is [id], from the
/// conversation list; null when there is none.
Future<List<int>?> conversationPeer(MessageClient messages, int id) async {
  try {
    ConversationCursor? before;
    do {
      final page = await messages.listConversations(before: before, limit: 50);
      for (final conversation in page.items) {
        if (notificationId(conversation.id) == id) return conversation.peer;
      }
      before = page.next;
    } while (before != null);
  } catch (error) {
    debugPrint(
      'ChatProject conversation lookup failed: '
      '${error is rust.CoreError ? error.code.name : error.runtimeType}',
    );
  }
  return null;
}

/// One notification per conversation from `ChatEvent.notify` (M6 spec 4):
/// the contact's alias and the unread count, never message text.
class Notifier {
  Notifier({
    required Stream<rust.ChatEvent> events,
    required this.platform,
    required this.aliasOf,
    required this.findPeer,
    AppLifecycleState? Function()? lifecycle,
  }) : _lifecycle =
           lifecycle ?? (() => WidgetsBinding.instance.lifecycleState) {
    _subscription = events.listen((event) {
      if (event case rust.ChatEvent_Notify(:final items)) items.forEach(_item);
    });
  }

  final MailboxPlatform platform;
  final String? Function(List<int> peer) aliasOf;
  final Future<List<int>?> Function(int id) findPeer;
  final AppLifecycleState? Function() _lifecycle;
  late final StreamSubscription<rust.ChatEvent> _subscription;

  /// The peer of the conversation on screen, if any.
  List<int>? openPeer;

  /// Opens a conversation of a tapped notification.
  ValueChanged<List<int>>? onOpen;

  /// The notifications this run posted: id -> peer.
  final _shown = <int, List<int>>{};

  void _item(rust.NotifyItem item) {
    final id = notificationId(item.conversation);
    if (item.unread == BigInt.zero) {
      _shown.remove(id);
      unawaited(platform.cancelNotification(id));
      return;
    }
    // No new messages: only the count of a shown notification changes.
    if (item.newMessages == 0 && !_shown.containsKey(id)) return;
    if (_lifecycle() == AppLifecycleState.resumed &&
        listEquals(openPeer, item.peer)) {
      return;
    }
    _shown[id] = List.of(item.peer);
    final alias = aliasOf(item.peer);
    unawaited(
      platform.notify(
        id: id,
        title: alias == null ? '새 메시지' : isolatedName(alias),
        body: '새 메시지 ${item.unread}개',
      ),
    );
  }

  /// A tapped notification. The id comes from the platform and is untrusted:
  /// an id without a conversation is ignored.
  Future<void> open(int id) async {
    final peer = _shown[id] ?? await findPeer(id);
    if (peer != null) onOpen?.call(peer);
  }

  Future<void> dispose() => _subscription.cancel();
}
