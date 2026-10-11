import 'package:flutter/material.dart';
import 'package:flutter/services.dart';

import '../theme/tokens.dart';
import 'nse_record.dart';
import 'probe_host.dart';

/// M1b device measurements: the APNs token to send to, and what the NSE recorded.
/// Lives only until M7 replaces the probe with the real extension.
class ProbeScreen extends StatefulWidget {
  const ProbeScreen({super.key, required this.host});

  final ProbeHost host;

  @override
  State<ProbeScreen> createState() => _ProbeScreenState();
}

String _mb(int bytes) => (bytes / 1048576).toStringAsFixed(1);

/// One line per notification.
String describeRecord(NseRecord r) {
  final latency = r.latencyMs == null ? '' : ' · 지연 ${r.latencyMs} ms';
  final peak = r.peakBytes == null ? '' : ' · 피크 ${_mb(r.peakBytes!)} MB';
  if (r.stopped) return '#${r.seq} · 끝 기록 없음 (중단됐을 수 있어요)';
  if (r.baseline) return '#${r.seq} · 기준(복호 없음)$peak$latency';
  if (r.ok == true) {
    final decrypt = r.decryptMicros == null ? '' : ' · 복호 ${(r.decryptMicros! / 1000).toStringAsFixed(1)} ms';
    return '#${r.seq} · 복호 성공$peak$decrypt$latency';
  }
  return '#${r.seq} · 복호 실패 · ${r.error ?? '이유 없음'}';
}

class _ProbeScreenState extends State<ProbeScreen> {
  String? _token;
  String? _registrationError;
  bool? _allowed;
  bool _requesting = false;
  List<NseRecord> _records = const [];

  @override
  void initState() {
    super.initState();
    _refresh();
  }

  Future<void> _refresh() async {
    final token = await widget.host.deviceToken();
    final failure = await widget.host.registrationError();
    final log = await widget.host.readNseLog();
    if (!mounted) return;
    setState(() {
      _token = token;
      _registrationError = failure;
      _records = log == null ? const [] : parseNseLog(log);
    });
  }

  Future<void> _requestPush() async {
    setState(() => _requesting = true);
    try {
      final allowed = await widget.host.requestPush();
      if (!mounted) return;
      setState(() => _allowed = allowed);
      await _refresh();
    } finally {
      if (mounted) setState(() => _requesting = false);
    }
  }

  Future<void> _clear() async {
    await widget.host.clearNseLog();
    await _refresh();
  }

  @override
  Widget build(BuildContext context) {
    final peaks = [for (final r in _records) if (r.peakBytes != null) r.peakBytes!];
    final token = _token;
    return Scaffold(
      appBar: AppBar(title: const Text('알림 측정')),
      body: ListView(
        padding: const EdgeInsets.fromLTRB(22, 16, 22, 16),
        children: [
          const Text('알림 토큰', style: TextStyle(fontSize: 18, fontWeight: FontWeight.w600, color: AppColors.text)),
          const SizedBox(height: 8),
          if (token == null)
            FilledButton(
              onPressed: _requesting ? null : _requestPush,
              child: Text(_requesting ? '토큰 받는 중…' : '알림 허용하고 토큰 받기'),
            )
          else ...[
            SelectableText(token, style: const TextStyle(fontSize: 13, color: AppColors.textSecondary)),
            TextButton(
              onPressed: () => Clipboard.setData(ClipboardData(text: token)),
              child: const Text('토큰 복사'),
            ),
          ],
          if (_allowed == false)
            const Text('알림이 꺼져 있어요. 설정 앱에서 허용해 주세요.', style: TextStyle(color: AppColors.textSecondary)),
          if (_registrationError != null)
            Text('토큰을 받지 못했어요 · $_registrationError', style: const TextStyle(color: AppColors.warning)),
          if (_allowed == true && token == null && _registrationError == null && !_requesting)
            const Text('토큰이 아직 안 왔어요. 잠시 뒤 새로고침을 눌러 보세요.', style: TextStyle(color: AppColors.textSecondary)),
          const SizedBox(height: 24),
          const Text('NSE 기록', style: TextStyle(fontSize: 18, fontWeight: FontWeight.w600, color: AppColors.text)),
          Row(children: [
            TextButton(onPressed: _refresh, child: const Text('새로고침')),
            TextButton(onPressed: _clear, child: const Text('기록 지우기')),
          ]),
          if (_records.isEmpty)
            const Text('아직 기록이 없어요', style: TextStyle(color: AppColors.textSecondary))
          else ...[
            if (peaks.isNotEmpty)
              Text(
                '가장 큰 피크 ${_mb(peaks.reduce((a, b) => a > b ? a : b))} MB · 기준 15 MB 미만',
                style: const TextStyle(fontWeight: FontWeight.w600, color: AppColors.text),
              ),
            const SizedBox(height: 8),
            for (final r in _records)
              Padding(
                padding: const EdgeInsets.symmetric(vertical: 4),
                child: Text(describeRecord(r), style: const TextStyle(fontSize: 14, color: AppColors.textSecondary)),
              ),
          ],
        ],
      ),
    );
  }
}
