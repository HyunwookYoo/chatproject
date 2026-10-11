import 'dart:async';

import 'package:flutter/material.dart';

import '../core_client.dart';
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
  const HomeScreen({super.key, required this.core, this.probe});

  final CoreClient core;

  /// M1b device probe (iOS only). Null hides the probe button.
  final ProbeHost? probe;

  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> {
  StreamSubscription<ChatEvent>? _events;
  ChatEvent_ConnectionState? _connection;
  String? _startError;

  @override
  void initState() {
    super.initState();
    // Subscribe before starting: the core replays the latest connection state anyway.
    _events = widget.core.events().listen((event) {
      if (event is ChatEvent_ConnectionState) {
        setState(() => _connection = event);
      }
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
  void dispose() {
    _events?.cancel();
    super.dispose();
  }

  String get _status {
    final error = _startError;
    if (error != null) return '코어를 시작하지 못했어요 · $error';
    final connection = _connection;
    if (connection == null) return '코어 시작 중';
    return connectionLabel(connection);
  }

  @override
  Widget build(BuildContext context) {
    return Scaffold(
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
              const Expanded(
                child: Center(
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      Text(
                        '아직 대화가 없어요',
                        style: TextStyle(fontSize: 22, fontWeight: FontWeight.w600, color: AppColors.text),
                      ),
                      SizedBox(height: 12),
                      Text(
                        '만나서 서로 QR을 찍으면 첫 연락처가 생겨요.',
                        textAlign: TextAlign.center,
                        style: TextStyle(fontSize: 15, color: AppColors.textSecondary),
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
