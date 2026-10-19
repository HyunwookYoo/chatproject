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
        '이전에 신뢰한 기기는 복구 문구를 받은 적이 없어도 계정 서명 키 사본을 가지고 있을 수 있어요. 공개 목록을 새 기기로 교체해도 그 사본은 지워지지 않아요. 키 유출이 의심되면 새 계정 ID를 만들어야 해요.',
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
