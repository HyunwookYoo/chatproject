import 'dart:async';
import 'package:flutter/material.dart';
import '../message_model.dart';
import 'message_presentation.dart';

/// Borrows a list-only projection. The owner disposes it, not this screen.
class ConversationListScreen extends StatefulWidget {
  const ConversationListScreen({
    super.key,
    required this.model,
    required this.onOpen,
  });
  final MessageModel model;
  final ValueChanged<List<int>> onOpen;
  @override
  State<ConversationListScreen> createState() => _ConversationListScreenState();
}

class _ConversationListScreenState extends State<ConversationListScreen> {
  final _scroll = ScrollController();

  Future<void> _latest() async {
    final model = widget.model;
    await model.loadLatest();
    if (mounted && widget.model == model && _scroll.hasClients) {
      _scroll.jumpTo(0);
    }
  }

  @override
  void dispose() {
    _scroll.dispose();
    super.dispose();
  }

  @override
  void initState() {
    super.initState();
    unawaited(widget.model.reload());
  }

  @override
  void didUpdateWidget(ConversationListScreen oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.model != widget.model) unawaited(widget.model.reload());
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: widget.model,
    builder: (context, _) {
      final model = widget.model;
      if (!model.ready) return const Center(child: Text('계정을 열면 대화를 볼 수 있어요.'));
      return Column(
        children: [
          if (model.hasLatest)
            TextButton(
              onPressed: model.busy ? null : _latest,
              child: const Text('최신 대화로 이동'),
            ),
          Expanded(
            child: ListView(
              controller: _scroll,
              children: [
                if (model.error case final String error)
                  Padding(
                    padding: const EdgeInsets.all(16),
                    child: Column(
                      children: [
                        Semantics(liveRegion: true, child: Text(error)),
                        TextButton(
                          onPressed: model.busy ? null : model.reload,
                          child: const Text('다시 불러오기'),
                        ),
                      ],
                    ),
                  ),
                if (model.conversations.isEmpty)
                  Padding(
                    padding: const EdgeInsets.symmetric(vertical: 32),
                    child: Column(
                      children: [
                        Text(
                          model.busy ? '대화를 불러오는 중' : '아직 대화가 없어요',
                          style: Theme.of(context).textTheme.titleLarge,
                        ),
                        const SizedBox(height: 12),
                        const Text(
                          '연락처에서 상대를 선택하고 대화를 시작해 보세요.',
                          textAlign: TextAlign.center,
                        ),
                      ],
                    ),
                  ),
                for (final conversation in model.conversations)
                  ListTile(
                    key: ValueKey(conversation.id.join(',')),
                    contentPadding: const EdgeInsets.symmetric(
                      vertical: 8,
                      horizontal: 4,
                    ),
                    title: Text(peerLabel(model, conversation.peer)),
                    subtitle: Column(
                      crossAxisAlignment: CrossAxisAlignment.start,
                      children: [
                        if (conversation.last case final last?) ...[
                          Text(
                            last.body,
                            maxLines: 2,
                            overflow: TextOverflow.ellipsis,
                          ),
                          Text(messageTime(last.sentAtMs)),
                          if (last.outgoing) Text(deliveryLabel(last.state)),
                        ],
                        if (conversation.unread > BigInt.zero)
                          Text('미확인 ${conversation.unread}개'),
                      ],
                    ),
                    onTap: () => widget.onOpen(List.of(conversation.peer)),
                  ),
                if (model.hasOlder)
                  TextButton(
                    onPressed: model.busy ? null : model.loadOlder,
                    child: const Text('이전 대화 더 보기'),
                  ),
              ],
            ),
          ),
        ],
      );
    },
  );
}
