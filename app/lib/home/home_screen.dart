import 'dart:async';

import 'package:flutter/material.dart';

import '../core_client.dart';
import '../account/account_model.dart';
import '../message/message_client.dart';
import '../message/message_model.dart';
import '../message/screens/conversation_list_screen.dart';
import '../network/network_client.dart';
import '../network/network_screen.dart';
import '../probe/probe_host.dart';
import '../probe/probe_screen.dart';
import '../src/rust/api/chat.dart';
import '../theme/tokens.dart';

/// Short label for the connection status line.
String connectionLabel(ChatEvent_ConnectionState state) {
  if (state.directPeers > 0) return '직접 연결 ${state.directPeers}';
  if (state.mailboxOk) return '메일박스 연결됨';
  if (state.queueOk ?? false) return '서버 큐 사용 중';
  return '연결 없음';
}

class HomeScreen extends StatefulWidget {
  const HomeScreen({
    super.key,
    required this.core,
    this.probe,
    this.network,
    this.account,
    this.messages,
    this.onOpenConversation,
  });

  final CoreClient core;
  final NetworkClient? network;
  final AccountModel? account;
  final MessageClient? messages;
  final ValueChanged<List<int>>? onOpenConversation;

  /// M1b device probe (iOS only). Null hides the probe button.
  final ProbeHost? probe;

  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> {
  StreamSubscription<ChatEvent>? _events;
  ChatEvent_ConnectionState? _connection;
  String? _startError;
  String? _streamError;
  MessageModel? _messages;

  @override
  void initState() {
    super.initState();
    _createMessages();
    // Subscribe before starting: the core replays the latest connection state anyway.
    _events = widget.core.events().listen((event) {
      if (!mounted) return;
      if (event is ChatEvent_ConnectionState) {
        setState(() => _connection = event);
      }
    }, onError: (Object error, StackTrace stack) {
      if (!mounted) return;
      debugPrint('ChatProject Home event stream failed: $error');
      debugPrintStack(label: 'ChatProject Home event stream failed', stackTrace: stack);
      setState(() => _streamError = '$error');
    }, onDone: () {
      if (!mounted) return;
      debugPrint('ChatProject Home event stream ended');
      setState(() => _streamError = '이벤트 스트림이 종료됐어요');
    });
    widget.core.start().then(
      (_) {},
      onError: (Object error) {
        if (!mounted) return;
        setState(() => _startError = error is CoreError ? error.code.name : '$error');
      },
    );
  }

  @override
  void didUpdateWidget(HomeScreen oldWidget) {
    super.didUpdateWidget(oldWidget);
    if (oldWidget.account != widget.account ||
        oldWidget.messages != widget.messages) {
      _messages?.dispose();
      _createMessages();
    }
  }

  void _createMessages() {
    final account = widget.account;
    final messages = widget.messages;
    _messages = account == null || messages == null
        ? null
        : MessageModel(messages, account: account);
  }

  @override
  void dispose() {
    _messages?.dispose();
    _events?.cancel();
    super.dispose();
  }

  String get _status {
    if (_streamError != null) return '이벤트를 받지 못했어요 · $_streamError';
    final error = _startError;
    if (error != null) return '코어를 시작하지 못했어요 · $error';
    final connection = _connection;
    if (connection == null) return '코어 시작 중';
    return connectionLabel(connection);
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: ModalRoute.of(context)?.canPop == true
          ? AppBar(title: const Text('대화'))
          : null,
      body: SafeArea(
        child: Padding(
          padding: const EdgeInsets.fromLTRB(22, 16, 22, 16),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              Row(
                children: [
                  const Text(
                    '대화',
                    style: TextStyle(fontSize: 30, fontWeight: FontWeight.w700, color: AppColors.text),
                  ),
                  const Spacer(),
                  if (widget.network != null)
                    IconButton(
                      tooltip: '네트워크 측정',
                      icon: const Icon(Icons.hub_outlined, color: AppColors.textSecondary),
                      onPressed: () => Navigator.of(context).push(
                        MaterialPageRoute<void>(builder: (_) => NetworkScreen(client: widget.network!)),
                      ),
                    ),
                  if (widget.probe != null)
                    IconButton(
                      tooltip: '알림 측정',
                      icon: const Icon(Icons.science_outlined, color: AppColors.textSecondary),
                      onPressed: () => Navigator.of(context).push(
                        MaterialPageRoute<void>(builder: (_) => ProbeScreen(host: widget.probe!)),
                      ),
                    ),
                ],
              ),
              const SizedBox(height: 16),
              Container(
                width: double.infinity,
                padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 14),
                decoration: const BoxDecoration(
                  color: AppColors.surface,
                  borderRadius: BorderRadius.all(Radius.circular(16)),
                ),
                child: Text(_status, style: const TextStyle(fontSize: 14, color: AppColors.textSecondary)),
              ),
              Expanded(
                child: _messages != null && widget.onOpenConversation != null
                    ? ConversationListScreen(
                        model: _messages!,
                        onOpen: widget.onOpenConversation!,
                      )
                    : const Center(
                        child: Column(
                          mainAxisSize: MainAxisSize.min,
                          children: [
                            Text(
                              '아직 대화가 없어요',
                              style: TextStyle(
                                fontSize: 22,
                                fontWeight: FontWeight.w600,
                                color: AppColors.text,
                              ),
                            ),
                            SizedBox(height: 12),
                            Text(
                              '초대를 확인하고 연락처를 추가해 보세요.',
                              textAlign: TextAlign.center,
                              style: TextStyle(
                                fontSize: 15,
                                color: AppColors.textSecondary,
                              ),
                            ),
                          ],
                        ),
                      ),
              ),
              Text('코어 ${widget.core.version}', style: const TextStyle(fontSize: 12, color: AppColors.textMuted)),
            ],
          ),
        ),
      ),
    );
  }
}
