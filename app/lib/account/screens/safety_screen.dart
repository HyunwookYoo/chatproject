import 'package:flutter/material.dart';
import '../../src/rust/api/account.dart';

class SafetyScreen extends StatefulWidget {
  const SafetyScreen({
    super.key,
    required this.display,
    required this.busy,
    required this.onConfirm,
  });
  final SafetyDisplay display;
  final bool busy;
  final Future<void> Function() onConfirm;
  @override
  State<SafetyScreen> createState() => _SafetyScreenState();
}

class _SafetyScreenState extends State<SafetyScreen> {
  bool _compared = false;
  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      Text('안전번호 확인', style: Theme.of(context).textTheme.headlineSmall),
      const Text('상대와 직접 만나거나 신뢰하는 별도 경로로 전체 64자 번호를 비교해 주세요.'),
      const SizedBox(height: 16),
      SelectableText(
        widget.display.groups,
        textDirection: TextDirection.ltr,
        style: const TextStyle(fontFamily: 'monospace'),
      ),
      CheckboxListTile(
        contentPadding: EdgeInsets.zero,
        value: _compared,
        onChanged: widget.busy
            ? null
            : (v) => setState(() => _compared = v ?? false),
        title: const Text('상대와 별도 경로로 전체 번호를 비교했어요'),
      ),
      FilledButton(
        onPressed: widget.busy || !_compared ? null : widget.onConfirm,
        child: const Text('확인 완료'),
      ),
    ],
  );
}
