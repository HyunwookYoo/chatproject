import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../src/rust/api/chat.dart' as rust;
import '../theme/tokens.dart';
import 'network_client.dart';
import 'network_model.dart';

class NetworkScreen extends StatefulWidget {
  const NetworkScreen({super.key, required this.client});

  final NetworkClient client;

  @override
  State<NetworkScreen> createState() => _NetworkScreenState();
}

class _NetworkScreenState extends State<NetworkScreen> {
  late final NetworkModel _model;
  final _target = TextEditingController();
  final _text = TextEditingController();
  String? _selectedPeerId;

  @override
  void initState() {
    super.initState();
    _model = NetworkModel(widget.client)..addListener(_changed);
    unawaited(_model.open());
  }

  void _changed() {
    if (mounted) setState(() {});
  }

  @override
  void dispose() {
    _model.removeListener(_changed);
    _model.dispose();
    _target.dispose();
    _text.dispose();
    super.dispose();
  }

  rust.PeerInfo? get _selectedPeer {
    for (final peer in _model.snapshot?.peers ?? <rust.PeerInfo>[]) {
      if (peer.peerId == _selectedPeerId) return peer;
    }
    return null;
  }

  Future<void> _copy(String value) async {
    await Clipboard.setData(ClipboardData(text: value));
    if (!mounted) return;
    ScaffoldMessenger.of(
      context,
    ).showSnackBar(const SnackBar(content: Text('복사했어요')));
  }

  Widget _identity(String label, String? value) => Column(
    crossAxisAlignment: CrossAxisAlignment.start,
    children: [
      Row(
        children: [
          Text(label, style: Theme.of(context).textTheme.titleSmall),
          const Spacer(),
          IconButton(
            tooltip: '$label 복사',
            onPressed: value == null || _model.busy ? null : () => _copy(value),
            icon: const Icon(Icons.copy_outlined, size: 18),
          ),
        ],
      ),
      SelectableText(
        value ?? '네트워크를 시작하면 표시돼요',
        style: const TextStyle(color: AppColors.textSecondary),
      ),
    ],
  );

  Widget _panel(String title, List<Widget> children) => SizedBox(
    width: double.infinity,
    child: Material(
      color: AppColors.surface,
      borderRadius: const BorderRadius.all(Radius.circular(16)),
      child: Padding(
        padding: const EdgeInsets.all(16),
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            Text(title, style: Theme.of(context).textTheme.titleLarge),
            const SizedBox(height: 16),
            ...children,
          ],
        ),
      ),
    ),
  );

  Widget _connections() {
    final info = _model.snapshot;
    final online = info?.state == rust.NetworkState.online;
    final selected = _selectedPeer;
    return _panel('연결', [
      Text(switch (info?.state) {
        rust.NetworkState.stopped => '네트워크 중지됨',
        rust.NetworkState.starting => '네트워크 시작 중',
        rust.NetworkState.online => '네트워크 준비됨 · 상대 연결은 별도예요',
        rust.NetworkState.failed => '네트워크를 시작하지 못했어요',
        null => '상태 확인 중',
      }, style: const TextStyle(color: AppColors.textSecondary)),
      const SizedBox(height: 12),
      Wrap(
        spacing: 8,
        runSpacing: 8,
        children: [
          FilledButton(
            onPressed: _model.busy || online ? null : _model.startNetwork,
            child: const Text('네트워크 시작'),
          ),
          OutlinedButton(
            onPressed:
                _model.busy ||
                    info == null ||
                    info.state == rust.NetworkState.stopped
                ? null
                : _model.stopNetwork,
            child: const Text('네트워크 중지'),
          ),
        ],
      ),
      const SizedBox(height: 12),
      _identity('ID', info?.endpointId),
      const SizedBox(height: 8),
      _identity('티켓', info?.ticket),
      const SizedBox(height: 8),
      const Text(
        '티켓에는 연결 주소가 들어 있어요. 연결할 상대에게만 공유해 주세요.',
        style: TextStyle(color: AppColors.textMuted),
      ),
      const SizedBox(height: 12),
      const Text(
        '프로세스를 다시 시작하면 내 ID가 바뀌어요.',
        style: TextStyle(color: AppColors.textMuted),
      ),
      const SizedBox(height: 20),
      TextField(
        key: const Key('peer-target'),
        controller: _target,
        enabled: !_model.busy,
        onChanged: (_) => setState(() {}),
        minLines: 1,
        maxLines: 3,
        decoration: const InputDecoration(
          labelText: '상대 ID 또는 티켓',
          border: OutlineInputBorder(),
        ),
      ),
      const SizedBox(height: 12),
      FilledButton(
        onPressed: _model.busy || !online || _target.text.trim().isEmpty
            ? null
            : () => _model.connectPeer(_target.text.trim()),
        child: const Text('연결'),
      ),
      const SizedBox(height: 20),
      Text('연결 상대', style: Theme.of(context).textTheme.titleMedium),
      const SizedBox(height: 8),
      if (info == null || info.peers.isEmpty)
        const Text(
          '상대 ID나 티켓을 입력해 연결해 주세요.',
          style: TextStyle(color: AppColors.textSecondary),
        )
      else
        for (final peer in info.peers)
          ListTile(
            contentPadding: EdgeInsets.zero,
            leading: Icon(
              peer.peerId == _selectedPeerId
                  ? Icons.radio_button_checked
                  : Icons.radio_button_off,
              color: peer.peerId == _selectedPeerId
                  ? AppColors.accent
                  : AppColors.textMuted,
            ),
            title: Text(peer.peerId),
            subtitle: Text(
              '${_phase(peer.phase)} · ${_path(peer.path)}'
              '${peer.connectMs == null ? '' : '\n연결 ${peer.connectMs}ms'}'
              '${peer.directMs == null ? '' : ' · 직접 ${peer.directMs}ms'}',
            ),
            onTap: _model.busy
                ? null
                : () => setState(() => _selectedPeerId = peer.peerId),
          ),
      const SizedBox(height: 12),
      OutlinedButton(
        onPressed:
            _model.busy ||
                selected == null ||
                selected.phase == rust.PeerPhase.disconnected
            ? null
            : () => _model.disconnectPeer(selected.peerId),
        child: const Text('연결 해제'),
      ),
    ]);
  }

  Widget _probe() {
    final peer = _selectedPeer;
    final connected = peer?.phase == rust.PeerPhase.connected;
    return _panel('글자 측정', [
      Text(
        peer == null ? '연결 상대를 선택해 주세요.' : '선택한 상대: ${peer.peerId}',
        style: const TextStyle(color: AppColors.textSecondary),
      ),
      const SizedBox(height: 16),
      TextField(
        key: const Key('probe-text'),
        controller: _text,
        enabled: !_model.busy,
        onChanged: (_) => setState(() {}),
        minLines: 3,
        maxLines: 6,
        decoration: const InputDecoration(
          labelText: '시험 글자',
          border: OutlineInputBorder(),
        ),
      ),
      const SizedBox(height: 12),
      FilledButton(
        onPressed: _model.busy || !connected || _text.text.isEmpty
            ? null
            : () async {
                final success = await _model.sendProbeText(
                  peer!.peerId,
                  _text.text,
                );
                if (mounted && success) setState(_text.clear);
              },
        child: const Text('전송'),
      ),
      const SizedBox(height: 12),
      const Text(
        '상대 화면 표시나 저장을 확인한 상태는 아니에요',
        style: TextStyle(color: AppColors.textMuted),
      ),
      const SizedBox(height: 20),
      Text(
        '최근 기록 (${_model.recentLines.length}/200)',
        style: Theme.of(context).textTheme.titleMedium,
      ),
      const SizedBox(height: 8),
      if (_model.recentLines.isEmpty)
        const Text(
          '주고받은 시험 글자가 여기에 표시돼요.',
          style: TextStyle(color: AppColors.textSecondary),
        )
      else
        for (final line in _model.recentLines)
          Padding(
            padding: const EdgeInsets.symmetric(vertical: 8),
            child: SelectableText(line),
          ),
    ]);
  }

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: const Text('네트워크 측정')),
    body: SafeArea(
      child: LayoutBuilder(
        builder: (context, constraints) => SingleChildScrollView(
          padding: const EdgeInsets.all(22),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.start,
            children: [
              const Text(
                '앱을 열어 둔 동안만 연결을 측정해요.',
                style: TextStyle(color: AppColors.textSecondary),
              ),
              const SizedBox(height: 16),
              if (_model.busy)
                const Padding(
                  padding: EdgeInsets.only(bottom: 16),
                  child: Text(
                    '작업 중',
                    style: TextStyle(color: AppColors.accent),
                  ),
                ),
              if (_model.error case final error?) ...[
                Text(error, style: const TextStyle(color: AppColors.warning)),
                TextButton(
                  onPressed: _model.busy ? null : _model.retry,
                  child: const Text('다시 시도'),
                ),
                const SizedBox(height: 8),
              ],
              if (constraints.maxWidth >= 720)
                Row(
                  crossAxisAlignment: CrossAxisAlignment.start,
                  children: [
                    Expanded(child: _connections()),
                    const SizedBox(width: 20),
                    Expanded(child: _probe()),
                  ],
                )
              else ...[
                _connections(),
                const SizedBox(height: 20),
                _probe(),
              ],
            ],
          ),
        ),
      ),
    ),
  );
}

String _phase(rust.PeerPhase phase) => switch (phase) {
  rust.PeerPhase.connecting => '연결 중',
  rust.PeerPhase.connected => '연결됨',
  rust.PeerPhase.reconnecting => '재연결 중',
  rust.PeerPhase.disconnected => '연결 끊김',
};

String _path(rust.PeerPath path) => switch (path) {
  rust.PeerPath.unknown => '경로 확인 중',
  rust.PeerPath.relay => '릴레이',
  rust.PeerPath.direct => '직접',
};
