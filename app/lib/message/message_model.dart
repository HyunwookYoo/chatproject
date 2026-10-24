import 'dart:async';
import 'dart:math';

import 'package:flutter/foundation.dart';

import '../account/account_model.dart';
import '../src/rust/api/account.dart' as a;
import '../src/rust/api/chat.dart' as rust;
import 'error_messages.dart';
import 'message_bridge.dart';
import 'message_client.dart';

/// Bounded, disposable projection. The existing AccountModel is borrowed, never
/// opened or disposed here; its coherent snapshot also serves late listeners.
class MessageModel extends ChangeNotifier {
  MessageModel(this.client, {required this.account}) {
    _subscription = client.events().listen(
      _event,
      onError: (Object failure) => _failure(failure),
      onDone: () => _failure(null),
    );
    account.addListener(_accountChanged);
    _accountChanged();
  }

  final MessageClient client;
  final AccountModel account;
  late final StreamSubscription<rust.ChatEvent> _subscription;
  bool _disposed = false;
  bool _ready = false;
  String? _identity;
  int _generation = 0;
  BigInt _seenRevision = BigInt.zero;
  bool _dirty = false;
  Future<void>? _refresh;
  Future<bool>? _send;
  ({String body, Uint8List id})? _pendingSend;
  Uint8List? _peer;
  int _messagePages = 1;
  int _conversationPages = 1;
  MessageCursor? _messageNext;
  ConversationCursor? _conversationNext;
  bool _opening = false;
  bool _commandBusy = false;
  List<MessageView> _messages = [];
  List<ConversationView> _conversations = [];

  List<MessageView> get messages => List.unmodifiable(_messages);
  List<ConversationView> get conversations => List.unmodifiable(_conversations);
  BigInt? revision;
  ConversationView? currentConversation;
  MessageKey? scrollAnchor;
  bool saving = false;
  bool get ready => _ready && !_disposed;
  bool get busy => _opening || _refresh != null || saving || _commandBusy;
  bool get hasOlder => currentConversation == null
      ? _conversationNext != null && _conversationPages < 3
      : _messageNext != null && _messagePages < 3;
  rust.ErrorCode? errorCode;
  String? get error =>
      errorCode == null ? null : messageErrorMessage(errorCode!);

  bool _current(int generation) => ready && generation == _generation;

  void _accountChanged() {
    if (_disposed) return;
    final view = account.snapshot;
    final allowed = switch (view?.phase) {
      a.AccountPhase.localReady ||
      a.AccountPhase.publishPending ||
      a.AccountPhase.active ||
      a.AccountPhase.conflict => true,
      _ => false,
    };
    final identity = view == null
        ? null
        : '${_bytes(view.publicPeerId ?? [])}/${_bytes(view.installation ?? [])}';
    if (allowed != _ready || identity != _identity) {
      ++_generation;
      _ready = allowed;
      _identity = identity;
      _messages = [];
      _conversations = [];
      currentConversation = null;
      scrollAnchor = null;
      _peer = null;
      _pendingSend = null;
      _send = null;
      saving = false;
      _opening = false;
      _commandBusy = false;
      _messagePages = _conversationPages = 1;
      _messageNext = null;
      _conversationNext = null;
      revision = null;
      _seenRevision = BigInt.zero;
      _dirty = false;
      errorCode = null;
    }
    notifyListeners();
  }

  void _event(rust.ChatEvent event) {
    if (!ready) return;
    final next = switch (event) {
      rust.ChatEvent_MessagesChanged(:final revision) => revision,
      rust.ChatEvent_MessageStateChanged(:final revision) => revision,
      _ => null,
    };
    if (next == null ||
        next <= _seenRevision ||
        (revision != null && next <= revision!)) {
      return;
    }
    _seenRevision = next;
    unawaited(reload());
  }

  Future<void> openConversation(List<int> peer) async {
    if (!ready) return;
    final generation = ++_generation;
    _peer = Uint8List.fromList(peer);
    _pendingSend = null;
    _send = null;
    saving = false;
    _commandBusy = false;
    _opening = true;
    _messages = [];
    currentConversation = null;
    scrollAnchor = null;
    _messagePages = 1;
    _messageNext = null;
    errorCode = null;
    notifyListeners();
    try {
      final view = await client.openDm(peer: List.of(peer));
      if (!_current(generation)) return;
      // open_dm has no global revision envelope: use only selection identity.
      // Last/unread come from the revision-checked conversation pages.
      currentConversation = ConversationView(
        id: Uint8List.fromList(view.id),
        peer: Uint8List.fromList(view.peer),
        unread: BigInt.zero,
        changeRevision: view.changeRevision,
      );
      await reload();
    } catch (failure) {
      if (_current(generation)) _failure(failure);
    } finally {
      if (_current(generation)) {
        _opening = false;
        notifyListeners();
      }
    }
  }

  Future<void> loadOlder() async {
    if (!ready || !hasOlder) return;
    if (currentConversation == null) {
      ++_conversationPages;
    } else {
      ++_messagePages;
    }
    await reload();
  }

  /// One refresh in flight, one dirty bit. Each pass restarts cursors and reads
  /// at most three pages per window. Pages are separate SQL requests: publish
  /// matching revisions, resnapshot after churn, and converge once events settle.
  Future<void> reload() {
    if (!ready) return Future.value();
    _dirty = true;
    if (_refresh != null) return _refresh!;
    // Install the guard before a client can emit a synchronous query event.
    final refresh = _refresh = Future<void>.microtask(_drain);
    notifyListeners();
    return refresh;
  }

  bool _pageCurrent(BigInt next, BigInt? passRevision, int generation) =>
      _current(generation) &&
      next >= _seenRevision &&
      (revision == null || next >= revision!) &&
      (passRevision == null || next == passRevision);

  Future<void> _drain() async {
    var generation = _generation;
    try {
      refresh:
      while (_dirty && ready) {
        _dirty = false;
        generation = _generation;
        final selected = currentConversation;
        final conversations = <String, ConversationView>{};
        final messages = <String, MessageView>{};
        BigInt? passRevision;
        ConversationCursor? conversationCursor;
        MessageCursor? messageCursor;
        var stale = false;
        for (var i = 0; i < _conversationPages; i++) {
          final ConversationPage page;
          try {
            page = await client.listConversations(
              before: conversationCursor,
              limit: 50,
            );
          } catch (failure) {
            if (!_current(generation)) continue refresh;
            // The fixed cursor detects moved/missing anchors. Restart the
            // loaded window; never reuse a cursor from the invalidated pass.
            if (conversationCursor != null &&
                failure is rust.CoreError &&
                failure.code == rust.ErrorCode.invalidMessage) {
              _dirty = true;
              continue refresh;
            }
            rethrow;
          }
          if (!_current(generation)) {
            stale = true;
            break;
          }
          if (!_pageCurrent(page.revision, passRevision, generation)) {
            stale = true;
            _dirty = true;
            break;
          }
          passRevision = page.revision;
          for (final row in page.items) {
            conversations[_bytes(row.id)] = row;
          }
          conversationCursor = page.next;
          if (conversationCursor == null) break;
        }
        if (stale) continue;
        if (selected != null) {
          for (var i = 0; i < _messagePages; i++) {
            final MessagePage page;
            try {
              page = await client.loadMessages(
                conversation: selected.id,
                before: messageCursor,
                limit: 50,
              );
            } catch (_) {
              if (!_current(generation)) continue refresh;
              rethrow;
            }
            if (!_current(generation)) {
              stale = true;
              break;
            }
            if (!_pageCurrent(page.revision, passRevision, generation)) {
              stale = true;
              _dirty = true;
              break;
            }
            for (final row in page.items) {
              messages[_key(row.key)] = row;
            }
            messageCursor = page.next;
            if (messageCursor == null) break;
          }
        }
        if (stale || !_current(generation)) continue;
        _conversations = conversations.values.take(150).toList();
        _messages = messages.values.take(150).toList();
        _conversationNext = conversationCursor;
        _messageNext = messageCursor;
        if (selected != null) {
          currentConversation = conversations[_bytes(selected.id)] ?? selected;
        }
        if (scrollAnchor != null &&
            !messages.containsKey(_key(scrollAnchor!))) {
          scrollAnchor = null;
        }
        revision = passRevision;
        errorCode = null;
        notifyListeners();
      }
    } catch (failure) {
      if (_current(generation)) _failure(failure);
    } finally {
      _refresh = null;
      if (!_disposed) notifyListeners();
    }
  }

  /// True authorizes the caller to clear its composer only after durable commit.
  /// A failed/lost reply retains the same action token for the same body.
  Future<bool> send(String body) {
    if (!ready || _peer == null || currentConversation == null) {
      return Future.value(false);
    }
    if (_send != null) {
      return _pendingSend?.body == body ? _send! : Future.value(false);
    }
    final random = Random.secure();
    final action = _pendingSend?.body == body
        ? _pendingSend!
        : (
            body: body,
            id: Uint8List.fromList(
              List.generate(16, (_) => random.nextInt(256)),
            ),
          );
    _pendingSend = action;
    saving = true;
    errorCode = null;
    final generation = _generation;
    final peer = Uint8List.fromList(_peer!);
    // Install the guard before calling the client or notifying any listeners.
    final result = Completer<bool>();
    _send = result.future;
    unawaited(_save(peer, action.id, body, generation).then(result.complete));
    notifyListeners();
    return result.future;
  }

  Future<bool> _save(
    List<int> peer,
    List<int> id,
    String body,
    int generation,
  ) async {
    try {
      await client.sendText(peer: peer, id: id, body: body);
      if (!_current(generation)) return false;
      _pendingSend = null;
      unawaited(reload());
      return true;
    } catch (failure) {
      if (_current(generation)) _failure(failure);
      return false;
    } finally {
      if (_current(generation)) {
        saving = false;
        _send = null;
        notifyListeners();
      }
    }
  }

  Future<bool> _command(Future<Object?> Function() operation) async {
    if (!ready || _commandBusy) return false;
    final generation = _generation;
    _commandBusy = true;
    errorCode = null;
    notifyListeners();
    try {
      await operation();
      if (!_current(generation)) return false;
      unawaited(reload());
      return true;
    } catch (failure) {
      if (_current(generation)) _failure(failure);
      return false;
    } finally {
      if (_current(generation)) {
        _commandBusy = false;
        notifyListeners();
      }
    }
  }

  Future<bool> cancel(MessageKey key) =>
      _command(() => client.cancelSend(key: key));
  Future<bool> retry(MessageKey key) =>
      _command(() => client.retrySend(key: key));

  Future<void> markVisibleRead(List<MessageKey> keys) async {
    if (!ready || currentConversation == null) return;
    final generation = _generation;
    final conversation = Uint8List.fromList(currentConversation!.id);
    final incoming = {
      for (final row in _messages.where((row) => !row.outgoing))
        _key(row.key): row.key,
    };
    final visible = <String, MessageKey>{};
    for (final key in keys) {
      final row = incoming[_key(key)];
      if (row != null) visible[_key(row)] = row;
    }
    final selected = visible.values.toList();
    try {
      for (var i = 0; i < selected.length; i += 50) {
        if (!_current(generation)) return;
        await client.markRead(
          conversation: conversation,
          keys: selected.skip(i).take(50).toList(),
        );
        if (!_current(generation)) return;
      }
    } catch (failure) {
      if (_current(generation)) _failure(failure);
    }
  }

  void _failure(Object? failure) {
    if (_disposed) return;
    errorCode = failure is rust.CoreError
        ? failure.code
        : rust.ErrorCode.notAvailable;
    notifyListeners();
  }

  @override
  void dispose() {
    if (_disposed) return;
    _disposed = true;
    ++_generation;
    account.removeListener(_accountChanged);
    unawaited(_subscription.cancel());
    _messages = [];
    _conversations = [];
    currentConversation = null;
    scrollAnchor = null;
    _pendingSend = null;
    super.dispose();
  }
}

// Fixed field separators preserve every identifier's bytes, unlike generated ==.
String _bytes(List<int> bytes) => bytes.join(',');
String _key(MessageKey key) =>
    '${_bytes(key.conversation)}/${_bytes(key.sender)}/${_bytes(key.installation)}/${_bytes(key.id)}';
