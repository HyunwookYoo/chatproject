import 'dart:async';
import 'dart:typed_data';

import 'package:chat_app/message/message_client.dart';
import 'package:chat_app/message/message_bridge.dart';
import 'package:chat_app/src/rust/api/chat.dart' as rust;

Uint8List bytes(int value, [int length = 32]) =>
    Uint8List.fromList(List.filled(length, value));

MessageView message(
  int id, {
  int conversation = 1,
  int sender = 2,
  int installation = 3,
  BigInt? lamport,
  DeliveryState state = DeliveryState.queued,
  bool outgoing = true,
}) => MessageView(
  key: MessageKey(
    conversation: bytes(conversation),
    sender: bytes(sender),
    installation: bytes(installation),
    id: bytes(id, 16),
  ),
  lamport: lamport ?? BigInt.from(id),
  sentAtMs: BigInt.parse('9007199254740993'),
  body: 'fixture',
  outgoing: outgoing,
  state: state,
  cancelRequested: false,
);

ConversationView conversation(int id, {int revision = 1}) => ConversationView(
  id: bytes(id),
  peer: bytes(id + 1),
  last: message(1, conversation: id),
  unread: BigInt.zero,
  changeRevision: BigInt.from(revision),
);

class FakeMessageClient implements MessageClient {
  FakeMessageClient({StreamController<rust.ChatEvent>? bus})
    : controller = bus ?? StreamController.broadcast(sync: true);
  final StreamController<rust.ChatEvent> controller;
  BigInt revision = BigInt.one;
  List<MessageView> rows = [message(1)];
  List<ConversationView> conversations = [conversation(1)];
  Future<MessagePage> Function(MessageCursor?)? query;
  Future<ConversationPage> Function(ConversationCursor?)? listQuery;
  Future<ConversationView> Function(List<int>)? opening;
  Future<MessageView> Function(List<int>, String)? sending;
  Future<MessageView> Function(MessageKey)? cancelling;
  final List<List<int>> tokens = [];
  final List<int> limits = [];
  final List<List<MessageKey>> reads = [];
  final List<MessageKey> cancellations = [];
  final List<MessageKey> retries = [];
  int activeQueries = 0;
  int maxQueries = 0;
  int messageQueries = 0;
  int listQueries = 0;

  @override
  Stream<rust.ChatEvent> events() => controller.stream;
  @override
  Future<ConversationView> openDm({required List<int> peer}) async =>
      opening == null ? conversation(peer.first - 1) : await opening!(peer);
  @override
  Future<MessagePage> loadMessages({
    required List<int> conversation,
    MessageCursor? before,
    required int limit,
  }) async {
    limits.add(limit);
    messageQueries++;
    activeQueries++;
    if (activeQueries > maxQueries) maxQueries = activeQueries;
    try {
      if (query != null) return await query!(before);
      final eligible =
          rows
              .where((row) => before == null || row.lamport < before.lamport)
              .toList()
            ..sort((a, b) => b.lamport.compareTo(a.lamport));
      final page = eligible.take(limit).toList();
      return MessagePage(
        revision: revision,
        items: page,
        next: eligible.length <= limit
            ? null
            : MessageCursor(
                lamport: page.last.lamport,
                installation: page.last.key.installation,
                id: page.last.key.id,
              ),
      );
    } finally {
      activeQueries--;
    }
  }

  @override
  Future<ConversationPage> listConversations({
    ConversationCursor? before,
    required int limit,
  }) async {
    limits.add(limit);
    listQueries++;
    activeQueries++;
    if (activeQueries > maxQueries) maxQueries = activeQueries;
    try {
      if (listQuery != null) return await listQuery!(before);
      final eligible =
          conversations
              .where(
                (row) =>
                    before == null ||
                    row.changeRevision < before.changeRevision,
              )
              .toList()
            ..sort((a, b) => b.changeRevision.compareTo(a.changeRevision));
      final page = eligible.take(limit).toList();
      return ConversationPage(
        revision: revision,
        items: page,
        next: eligible.length <= limit
            ? null
            : ConversationCursor(
                changeRevision: page.last.changeRevision,
                id: page.last.id,
              ),
      );
    } finally {
      activeQueries--;
    }
  }

  @override
  Future<MessageView> sendText({
    required List<int> peer,
    required List<int> id,
    required String body,
  }) async {
    tokens.add(List.of(id));
    return sending == null ? message(2) : await sending!(id, body);
  }

  @override
  Future<MessageView> cancelSend({required MessageKey key}) async {
    cancellations.add(key);
    return cancelling == null
        ? message(1, state: DeliveryState.cancelled)
        : await cancelling!(key);
  }

  @override
  Future<MessageView> retrySend({required MessageKey key}) async {
    retries.add(key);
    return message(1);
  }

  @override
  Future<void> markRead({
    required List<int> conversation,
    required List<MessageKey> keys,
  }) async => reads.add(keys);

  void changed(int value) {
    revision = BigInt.from(value);
    controller.add(
      rust.ChatEvent.messagesChanged(
        revision: revision,
        conversation: bytes(1),
      ),
    );
  }
}

/// Models the native token lookup before enqueue, including cancelled rows.
class DurableMessageClient extends FakeMessageClient {
  DurableMessageClient() {
    rows = [];
    conversations = [conversation(3)];
  }
  bool loseSendReply = true;
  bool loseCancelReply = false;
  Object? cancelError;
  Future<void>? beforeCancel;
  final queued = <String>{};

  @override
  Future<MessageView> sendText({
    required List<int> peer,
    required List<int> id,
    required String body,
  }) async {
    tokens.add(List.of(id));
    final key = MessageKey(
      conversation: bytes(peer.first - 1),
      sender: bytes(2),
      installation: bytes(3),
      id: Uint8List.fromList(id),
    );
    for (final row in rows) {
      if (_identity(row.key) == _identity(key)) {
        if (row.body != body) {
          throw const rust.CoreError(
            code: rust.ErrorCode.messageConflict,
            detail: 'fixture',
          );
        }
        return row;
      }
    }
    final row = MessageView(
      key: key,
      lamport: BigInt.from(rows.length + 1),
      sentAtMs: BigInt.one,
      body: body,
      outgoing: true,
      state: DeliveryState.queued,
      cancelRequested: false,
    );
    rows.add(row);
    queued.add(_identity(key));
    revision += BigInt.one;
    if (loseSendReply) {
      loseSendReply = false;
      throw StateError('fixture response lost after commit');
    }
    return row;
  }

  @override
  Future<MessageView> cancelSend({required MessageKey key}) async {
    cancellations.add(key);
    await beforeCancel;
    if (cancelError != null) throw cancelError!;
    final index = rows.indexWhere(
      (row) => _identity(row.key) == _identity(key),
    );
    if (index < 0) {
      throw const rust.CoreError(
        code: rust.ErrorCode.messageNotFound,
        detail: 'fixture',
      );
    }
    final row = rows[index];
    final cancelled = MessageView(
      key: row.key,
      lamport: row.lamport,
      sentAtMs: row.sentAtMs,
      body: row.body,
      outgoing: row.outgoing,
      state: DeliveryState.cancelled,
      cancelRequested: true,
    );
    rows[index] = cancelled;
    queued.remove(_identity(key));
    revision += BigInt.one;
    if (loseCancelReply) throw StateError('fixture cancellation reply lost');
    return cancelled;
  }
}

String _identity(MessageKey key) =>
    '${key.conversation.join(',')}/${key.sender.join(',')}/${key.installation.join(',')}/${key.id.join(',')}';
