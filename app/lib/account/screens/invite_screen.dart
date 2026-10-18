import 'dart:convert';
import 'package:flutter/material.dart';

class InviteScreen extends StatefulWidget {
  const InviteScreen({
    super.key,
    required this.busy,
    required this.onParse,
    required this.onCreate,
  });
  final bool busy;
  final Future<void> Function(String) onParse;
  final Future<String?> Function(String?) onCreate;
  @override
  State<InviteScreen> createState() => _InviteScreenState();
}

class _InviteScreenState extends State<InviteScreen> {
  final _input = TextEditingController();
  String? _invite, _error;
  @override
  void dispose() {
    _input.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      Text('초대', style: Theme.of(context).textTheme.headlineSmall),
      const Text('초대 링크를 붙여넣어 확인해 주세요. 상대의 실제 신원은 안전번호로 따로 확인해요.'),
      const SizedBox(height: 16),
      TextField(
        controller: _input,
        decoration: InputDecoration(
          labelText: '초대 링크 또는 내 표시 이름',
          errorText: _error,
        ),
      ),
      FilledButton(
        onPressed: widget.busy ? null : () => widget.onParse(_input.text),
        child: const Text('초대 확인'),
      ),
      TextButton(
        onPressed: widget.busy
            ? null
            : () async {
                if (utf8.encode(_input.text).length > 64) {
                  setState(() => _error = '이름은 UTF-8 64바이트까지 입력할 수 있어요.');
                  return;
                }
                final result = await widget.onCreate(
                  _input.text.isEmpty ? null : _input.text,
                );
                if (mounted) setState(() => _invite = result);
              },
        child: const Text('내 초대 만들기'),
      ),
      if (_invite != null)
        SelectableText(_invite!, textDirection: TextDirection.ltr),
    ],
  );
}
