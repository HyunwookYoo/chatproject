import 'package:flutter/material.dart';
import '../src/rust/api/account.dart' as a;
import 'public_id.dart';

/// Consent captures the exact public evidence displayed, never a later snapshot.
class RecordReview extends StatelessWidget {
  const RecordReview({
    super.key,
    required this.record,
    required this.hash,
    required this.enabled,
    required this.generationChanged,
    required this.onConfirm,
  });
  final a.RecordView record;
  final List<int> hash;
  final bool enabled, generationChanged;
  final Future<bool> Function(List<int>) onConfirm;

  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      const Text('검토할 현재 공개 목록'),
      Text('세대: ${publicId(record.generation)}'),
      Text('목록 해시: ${publicId(hash)}'),
      if (generationChanged)
        const Text('계정 세대가 바뀌었어요. 복구 또는 기기 변경 내용을 직접 확인한 뒤 결정해 주세요.'),
      for (final device in record.devices)
        Text(
          '${device.kind == a.DeviceKind.desktop ? '데스크톱' : '모바일'} · ${publicId(device.installationId)}',
        ),
      FilledButton(
        onPressed: !enabled
            ? null
            : () async {
                final displayedHash = List<int>.of(hash);
                final yes = await showDialog<bool>(
                  context: context,
                  builder: (context) => AlertDialog(
                    title: const Text('현재 목록을 확인했나요?'),
                    content: SingleChildScrollView(
                      child: Text(
                        '세대: ${publicId(record.generation)}\n목록 해시: ${publicId(displayedHash)}\n${record.devices.map((d) => publicId(d.installationId)).join('\n')}\n목록을 수용한 뒤에도 새 기기 인증이 필요해요.',
                      ),
                    ),
                    actions: [
                      TextButton(
                        onPressed: () => Navigator.pop(context, false),
                        child: const Text('취소'),
                      ),
                      FilledButton(
                        onPressed: () => Navigator.pop(context, true),
                        child: const Text('확인한 목록 적용'),
                      ),
                    ],
                  ),
                );
                if (yes == true) await onConfirm(displayedHash);
              },
        child: const Text('이 목록 확인'),
      ),
    ],
  );
}
