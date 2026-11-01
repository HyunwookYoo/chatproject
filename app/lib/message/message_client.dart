import '../src/rust/api/chat.dart' as rust;
import '../core_client.dart';
import 'message_bridge.dart';
import 'message_bridge.dart' as bridge;

abstract interface class MessageClient {
  Stream<rust.ChatEvent> events();
  Future<ConversationView> openDm({required List<int> peer});
  Future<ConversationPage> listConversations({
    ConversationCursor? before,
    required int limit,
  });
  Future<MessagePage> loadMessages({
    required List<int> conversation,
    MessageCursor? before,
    required int limit,
  });
  Future<MessageView> sendText({
    required List<int> peer,
    required List<int> id,
    required String body,
  });
  Future<MessageView> cancelSend({required MessageKey key});
  Future<MessageView> retrySend({required MessageKey key});
  Future<void> markRead({
    required List<int> conversation,
    required List<MessageKey> keys,
  });
}

/// All product calls pass the checked facade; events share the core's one bus.
class FrbMessageClient implements MessageClient {
  FrbMessageClient(this.core);
  final CoreClient core;
  @override
  Stream<rust.ChatEvent> events() => core.events();
  @override
  Future<ConversationView> openDm({required List<int> peer}) =>
      bridge.openDm(peer: peer);
  @override
  Future<ConversationPage> listConversations({
    ConversationCursor? before,
    required int limit,
  }) => bridge.listConversations(before: before, limit: limit);
  @override
  Future<MessagePage> loadMessages({
    required List<int> conversation,
    MessageCursor? before,
    required int limit,
  }) => bridge.loadMessages(
    conversation: conversation,
    before: before,
    limit: limit,
  );
  @override
  Future<MessageView> sendText({
    required List<int> peer,
    required List<int> id,
    required String body,
  }) => bridge.sendText(peer: peer, id: id, body: body);
  @override
  Future<MessageView> cancelSend({required MessageKey key}) =>
      bridge.cancelSend(key: key);
  @override
  Future<MessageView> retrySend({required MessageKey key}) =>
      bridge.retrySend(key: key);
  @override
  Future<void> markRead({
    required List<int> conversation,
    required List<MessageKey> keys,
  }) => bridge.markRead(conversation: conversation, keys: keys);
}
