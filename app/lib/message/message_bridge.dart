/// Checked product entry points. FRB's low-level integer codecs narrow values,
/// so validate and own caller bytes before handing them to generated wrappers.
library;

import 'dart:typed_data';
import '../src/rust/api/chat.dart';
import '../src/rust/api/message.dart';
import '../src/rust/api/message.dart' as generated;

export '../src/rust/api/chat.dart' show MessageKey, DeliveryState;
export '../src/rust/api/message.dart'
    show
        BlockReason,
        ConversationCursor,
        ConversationPage,
        ConversationView,
        MessageCursor,
        MessagePage,
        MessageView;

Never _invalid() => throw const CoreError(
  code: ErrorCode.invalidMessage,
  detail: 'Invalid message argument',
);

Uint8List _bytes(List<int> value, int length) {
  if (value.length != length) _invalid();
  final owned = Uint8List(length);
  for (var i = 0; i < length; i++) {
    final byte = value[i];
    if (byte < 0 || byte > 255) _invalid();
    owned[i] = byte;
  }
  return owned;
}

void _limit(int limit) {
  if (limit < 1 || limit > 50) _invalid();
}

BigInt _integer(BigInt value) {
  if (value < BigInt.zero || value > BigInt.parse('9223372036854775807')) {
    _invalid();
  }
  return value;
}

MessageKey _key(MessageKey key) => MessageKey(
  conversation: _bytes(key.conversation, 32),
  sender: _bytes(key.sender, 32),
  installation: _bytes(key.installation, 32),
  id: _bytes(key.id, 16),
);

Future<ConversationView> openDm({required List<int> peer}) async =>
    generated.openDm(peer: _bytes(peer, 32));

Future<ConversationPage> listConversations({
  ConversationCursor? before,
  required int limit,
}) async {
  _limit(limit);
  final cursor = before == null
      ? null
      : ConversationCursor(
          changeRevision: _integer(before.changeRevision),
          id: _bytes(before.id, 32),
        );
  return generated.listConversations(before: cursor, limit: limit);
}

Future<MessagePage> loadMessages({
  required List<int> conversation,
  MessageCursor? before,
  required int limit,
}) async {
  _limit(limit);
  final cursor = before == null
      ? null
      : MessageCursor(
          lamport: _integer(before.lamport),
          installation: _bytes(before.installation, 32),
          id: _bytes(before.id, 16),
        );
  return generated.loadMessages(
    conversation: _bytes(conversation, 32),
    before: cursor,
    limit: limit,
  );
}

Future<MessageView> sendText({
  required List<int> peer,
  required List<int> id,
  required String body,
}) async =>
    generated.sendText(peer: _bytes(peer, 32), id: _bytes(id, 16), body: body);

Future<MessageView> cancelSend({required MessageKey key}) async =>
    generated.cancelSend(key: _key(key));

Future<MessageView> retrySend({required MessageKey key}) async =>
    generated.retrySend(key: _key(key));

Future<void> markRead({
  required List<int> conversation,
  required List<MessageKey> keys,
}) async {
  if (keys.length > 50) _invalid();
  return generated.markRead(
    conversation: _bytes(conversation, 32),
    keys: keys.map(_key).toList(growable: false),
  );
}
