import 'dart:async';
import 'dart:ui' show ViewFocusEvent, ViewFocusState;

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../../src/rust/api/chat.dart' as rust;
import '../../theme/tokens.dart';
import '../message_bridge.dart';
import '../message_model.dart';
import 'message_presentation.dart';

/// Borrows one room projection; owns only its visible rows, listeners and input.
class ConversationScreen extends StatefulWidget {
  const ConversationScreen({
    super.key,
    required this.model,
    required this.peer,
  });
  final MessageModel model;
  final List<int> peer;
  @override
  State<ConversationScreen> createState() => _ConversationScreenState();
}

class _ConversationScreenState extends State<ConversationScreen>
    with WidgetsBindingObserver {
  final _composer = TextEditingController();
  final _scroll = ScrollController();
  final _viewport = GlobalKey();
  final _rows = <String, GlobalKey>{};
  final _read = <String>{};
  bool _framePending = false;
  bool _reading = false;
  bool _readFailed = false;
  bool _focused = true;
  int _generation = 0;
  int _edits = 0;
  bool _selected = false;
  AppLifecycleState? _lifecycle;
  ModalRoute<void>? _route;

  MessageModel get model => widget.model;

  @override
  void initState() {
    super.initState();
    _lifecycle = WidgetsBinding.instance.lifecycleState;
    WidgetsBinding.instance.addObserver(this);
    model.addListener(_changed);
    _composer.addListener(_edited);
    _scroll.addListener(_scheduleRead);
    unawaited(model.openConversation(widget.peer));
  }

  void _edited() => _edits++;

  @override
  void didUpdateWidget(ConversationScreen oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.model != model || !listEquals(oldWidget.peer, widget.peer)) {
      oldWidget.model.removeListener(_changed);
      model.addListener(_changed);
      _clearPrivate();
      unawaited(model.openConversation(widget.peer));
    }
  }

  void _clearPrivate() {
    ++_generation;
    _composer.clear();
    _rows.clear();
    _read.clear();
    _readFailed = false;
    _selected = false;
  }

  void _changed() {
    if (!mounted) return;
    if (!model.ready || (_selected && model.currentConversation == null)) {
      _clearPrivate();
    }
    _selected = model.currentConversation != null;
    final keys = model.messages.map((row) => messageIdentity(row.key)).toSet();
    _rows.removeWhere((key, _) => !keys.contains(key));
    _read.retainAll(keys);
    setState(() {});
    _scheduleRead();
  }

  @override
  void didChangeDependencies() {
    super.didChangeDependencies();
    final route = ModalRoute.of<void>(context);
    if (_route != route) {
      _route?.animation?.removeStatusListener(_routeChanged);
      _route?.secondaryAnimation?.removeStatusListener(_routeChanged);
      _route = route;
      route?.animation?.addStatusListener(_routeChanged);
      route?.secondaryAnimation?.addStatusListener(_routeChanged);
    }
    _scheduleRead();
  }

  void _routeChanged(AnimationStatus status) => _scheduleRead();

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    _lifecycle = state;
    if (state == AppLifecycleState.resumed) _scheduleRead();
  }

  @override
  void didChangeViewFocus(ViewFocusEvent event) {
    if (event.viewId != View.of(context).viewId) return;
    _focused = event.state == ViewFocusState.focused;
    if (_focused) _scheduleRead();
  }

  bool get _eligible =>
      mounted &&
      model.ready &&
      _lifecycle == AppLifecycleState.resumed &&
      _focused &&
      ModalRoute.of(context)?.isCurrent == true &&
      _route?.animation?.status == AnimationStatus.completed &&
      _route?.secondaryAnimation?.status == AnimationStatus.dismissed &&
      listEquals(model.currentConversation?.peer, widget.peer);

  void _scheduleRead() {
    if (_framePending || !mounted) return;
    _framePending = true;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      _framePending = false;
      if (_eligible && !_reading && !_readFailed) unawaited(_markRendered());
    });
    WidgetsBinding.instance.scheduleFrame();
  }

  Future<void> _markRendered() async {
    final viewport = _viewport.currentContext?.findRenderObject();
    if (!_eligible || viewport is! RenderBox || !viewport.hasSize) return;
    final bounds = viewport.localToGlobal(Offset.zero) & viewport.size;
    final visible = <MessageKey>[];
    for (final row in model.messages.where((row) => !row.outgoing)) {
      final id = messageIdentity(row.key);
      if (_read.contains(id)) continue;
      final box = _rows[id]?.currentContext?.findRenderObject();
      if (box is RenderBox &&
          box.attached &&
          box.hasSize &&
          bounds.overlaps(box.localToGlobal(Offset.zero) & box.size)) {
        visible.add(row.key);
        // One native batch per frame/check. Recheck route, focus and geometry
        // after its await instead of allowing model batching to outlive focus.
        if (visible.length == 50) break;
      }
    }
    if (visible.isEmpty || !_eligible) return;
    final generation = _generation;
    // Record before await: read events may immediately refresh the model.
    _read.addAll(visible.map(messageIdentity));
    _reading = true;
    final completed = await model.markVisibleRead(visible);
    _reading = false;
    if (!mounted) return;
    if (generation != _generation) {
      _scheduleRead();
      return;
    }
    if (!completed) setState(() => _readFailed = true);
    _scheduleRead();
  }

  Future<void> _retryRead() async {
    final generation = _generation;
    await model.reload();
    if (!mounted || generation != _generation || model.errorCode != null) {
      return;
    }
    setState(() {
      _read.clear();
      _readFailed = false;
    });
    _scheduleRead();
  }

  Future<void> _latest() async {
    final generation = _generation;
    await model.loadLatest();
    if (!mounted || generation != _generation) return;
    if (_scroll.hasClients) _scroll.jumpTo(0);
    _scheduleRead();
  }

  Future<void> _send({String? resend}) async {
    if (!model.ready ||
        model.saving ||
        ModalRoute.of(context)?.isCurrent != true) {
      return;
    }
    final body = resend ?? _composer.text;
    if (body.trim().isEmpty) return;
    final generation = _generation;
    final edits = _edits;
    final sent = await model.send(body);
    if (sent &&
        mounted &&
        generation == _generation &&
        ModalRoute.of(context)?.isCurrent == true &&
        resend == null &&
        edits == _edits &&
        _composer.text == body) {
      _composer.clear();
    }
  }

  Future<void> _cancel(MessageKey key) async {
    final generation = _generation;
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('전송을 취소할까요?'),
        content: const Text('이미 전송 중인 메시지는 상대에게 도착했을 수 있어요.'),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('돌아가기'),
          ),
          TextButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('전송 중단'),
          ),
        ],
      ),
    );
    if (confirmed == true &&
        mounted &&
        generation == _generation &&
        model.ready) {
      await model.cancel(key);
    }
    _scheduleRead();
  }

  Widget _message(MessageView row) {
    final id = messageIdentity(row.key);
    final canCancel =
        !row.cancelRequested &&
        [
          DeliveryState.queued,
          DeliveryState.sentDirect,
          DeliveryState.blocked,
        ].contains(row.state);
    return Padding(
      key: _rows.putIfAbsent(id, GlobalKey.new),
      padding: const EdgeInsets.symmetric(vertical: 6, horizontal: 16),
      child: Align(
        alignment: row.outgoing ? Alignment.centerRight : Alignment.centerLeft,
        child: Container(
          constraints: const BoxConstraints(maxWidth: 560),
          padding: const EdgeInsets.all(14),
          decoration: BoxDecoration(
            color: AppColors.surface,
            borderRadius: BorderRadius.circular(16),
          ),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            mainAxisSize: MainAxisSize.min,
            children: [
              Text(
                row.outgoing ? '나' : peerLabel(model, widget.peer),
                style: const TextStyle(color: AppColors.textSecondary),
              ),
              const SizedBox(height: 4),
              Text(row.body),
              const SizedBox(height: 8),
              Text(
                messageTime(row.sentAtMs),
                style: const TextStyle(color: AppColors.textSecondary),
              ),
              if (row.outgoing) ...[
                Text(
                  deliveryLabel(row.state),
                  style: const TextStyle(color: AppColors.accent),
                ),
                if (row.cancelRequested && row.state != DeliveryState.cancelled)
                  const Text('재전송 중단됨 · 이미 상대에게 도착했을 수 있어요.'),
                if (row.state == DeliveryState.blocked)
                  Text(blockLabel(row.block)),
                if (row.state == DeliveryState.failed)
                  const Text('자동으로 다시 보내지 않아요. 메시지 내용과 상대 상태를 확인해 주세요.'),
                Wrap(
                  spacing: 8,
                  children: [
                    if (canCancel)
                      TextButton(
                        onPressed: model.busy ? null : () => _cancel(row.key),
                        child: const Text('전송 취소'),
                      ),
                    if (row.state == DeliveryState.blocked &&
                        !row.cancelRequested)
                      TextButton(
                        onPressed: model.busy
                            ? null
                            : () => model.retry(row.key),
                        child: const Text('다시 시도'),
                      ),
                    if (row.state == DeliveryState.cancelled)
                      TextButton(
                        onPressed: model.busy
                            ? null
                            : () => _send(resend: row.body),
                        child: const Text('새 메시지로 보내기'),
                      ),
                  ],
                ),
              ],
            ],
          ),
        ),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    _scheduleRead();
    return Scaffold(
      appBar: AppBar(title: Text(peerLabel(model, widget.peer))),
      body: SafeArea(
        child: Center(
          child: ConstrainedBox(
            constraints: const BoxConstraints(maxWidth: 760),
            child: !model.ready
                ? const Text('계정을 열면 대화를 볼 수 있어요.')
                : Column(
                    children: [
                      if (model.hasLatest)
                        TextButton(
                          onPressed: model.busy ? null : _latest,
                          child: const Text('최신 메시지로 이동'),
                        ),
                      Expanded(
                        child: SizedBox(
                          key: _viewport,
                          child:
                              NotificationListener<ScrollMetricsNotification>(
                                onNotification: (_) {
                                  _scheduleRead();
                                  return false;
                                },
                                child: ListView.builder(
                                  key: const ValueKey('message-viewport'),
                                  controller: _scroll,
                                  reverse: true,
                                  itemCount: model.messages.length + 1,
                                  itemBuilder: (context, index) {
                                    if (index < model.messages.length) {
                                      return _message(model.messages[index]);
                                    }
                                    return Padding(
                                      padding: const EdgeInsets.all(16),
                                      child: Column(
                                        children: [
                                          if (model.messages.isEmpty)
                                            Text(
                                              model.busy
                                                  ? '대화를 불러오는 중'
                                                  : '첫 메시지를 보내 보세요.',
                                            ),
                                          if (model.hasOlder)
                                            TextButton(
                                              onPressed: model.busy
                                                  ? null
                                                  : model.loadOlder,
                                              child: const Text('이전 메시지 더 보기'),
                                            ),
                                        ],
                                      ),
                                    );
                                  },
                                ),
                              ),
                        ),
                      ),
                      if (model.error case final String error)
                        ConstrainedBox(
                          constraints: const BoxConstraints(maxHeight: 110),
                          child: SingleChildScrollView(
                            padding: const EdgeInsets.symmetric(horizontal: 16),
                            child: Semantics(
                              liveRegion: true,
                              child: Text(
                                model.errorCode == rust.ErrorCode.storageFull
                                    ? '$error 기존 메시지는 지우지 않아요. 대기 메시지 한도도 확인해 주세요.'
                                    : error,
                              ),
                            ),
                          ),
                        ),
                      if (_readFailed)
                        TextButton(
                          onPressed: model.busy ? null : _retryRead,
                          child: const Text('읽음 반영 다시 시도'),
                        ),
                      if (model.saving)
                        Semantics(liveRegion: true, child: const Text('저장 중')),
                      Padding(
                        padding: const EdgeInsets.all(12),
                        child: Shortcuts(
                          shortcuts: const {
                            SingleActivator(
                              LogicalKeyboardKey.enter,
                              control: true,
                            ): _SendIntent(),
                            SingleActivator(
                              LogicalKeyboardKey.enter,
                              meta: true,
                            ): _SendIntent(),
                          },
                          child: Actions(
                            actions: {
                              _SendIntent: CallbackAction<_SendIntent>(
                                onInvoke: (_) {
                                  unawaited(_send());
                                  return null;
                                },
                              ),
                            },
                            child: Column(
                              children: [
                                TextField(
                                  controller: _composer,
                                  minLines: 1,
                                  maxLines: 4,
                                  keyboardType: TextInputType.multiline,
                                  textInputAction: TextInputAction.newline,
                                  decoration: const InputDecoration(
                                    labelText: '메시지 입력',
                                    border: OutlineInputBorder(),
                                  ),
                                ),
                                Align(
                                  alignment: Alignment.centerRight,
                                  child: FilledButton(
                                    onPressed:
                                        model.saving ||
                                            model.currentConversation == null
                                        ? null
                                        : _send,
                                    child: const Text('보내기'),
                                  ),
                                ),
                              ],
                            ),
                          ),
                        ),
                      ),
                    ],
                  ),
          ),
        ),
      ),
    );
  }

  @override
  void dispose() {
    ++_generation;
    WidgetsBinding.instance.removeObserver(this);
    _route?.animation?.removeStatusListener(_routeChanged);
    _route?.secondaryAnimation?.removeStatusListener(_routeChanged);
    model.removeListener(_changed);
    _composer.dispose();
    _scroll.dispose();
    super.dispose();
  }
}

class _SendIntent extends Intent {
  const _SendIntent();
}
