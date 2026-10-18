import 'package:flutter/material.dart';

class RecoverScreen extends StatefulWidget {
  const RecoverScreen({super.key, required this.busy, required this.onRecover});
  final bool busy;
  final Future<void> Function(String) onRecover;
  @override
  State<RecoverScreen> createState() => _RecoverScreenState();
}

class _RecoverScreenState extends State<RecoverScreen> {
  final _words = TextEditingController();
  bool _replacement = false;
  @override
  void dispose() {
    _words.clear();
    _words.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      Text('복구 문구로 복구', style: Theme.of(context).textTheme.headlineSmall),
      const SizedBox(height: 16),
      const Text('계정 ID는 돌아오지만 내 연락처와 대화 기록은 돌아오지 않아요'),
      const SizedBox(height: 12),
      const Text(
        '침해된 기기가 문구를 알고 있다면 같은 계정 ID 복구만으로 접근을 막을 수 없어요. 기존 기기를 새 기기로 교체하는 공개 목록 변경을 요청해요.',
      ),
      const SizedBox(height: 16),
      TextField(
        controller: _words,
        obscureText: true,
        autocorrect: false,
        enableSuggestions: false,
        decoration: const InputDecoration(labelText: '24단어 복구 문구'),
      ),
      CheckboxListTile(
        contentPadding: EdgeInsets.zero,
        value: _replacement,
        onChanged: widget.busy
            ? null
            : (v) => setState(() => _replacement = v ?? false),
        title: const Text('기존 기기 교체에 동의해요'),
      ),
      FilledButton(
        onPressed: widget.busy || !_replacement
            ? null
            : () async {
                final words = _words.text;
                _words.clear();
                await widget.onRecover(words);
              },
        child: const Text('계정 복구'),
      ),
    ],
  );
}
