import 'package:flutter/material.dart';

class RecoveryConfirmScreen extends StatefulWidget {
  const RecoveryConfirmScreen({
    super.key,
    required this.positions,
    required this.busy,
    required this.onConfirm,
  });
  final List<int> positions;
  final bool busy;
  final Future<void> Function(List<String>) onConfirm;
  @override
  State<RecoveryConfirmScreen> createState() => _RecoveryConfirmScreenState();
}

class _RecoveryConfirmScreenState extends State<RecoveryConfirmScreen> {
  late final _fields = List.generate(
    widget.positions.length,
    (_) => TextEditingController(),
  );
  @override
  void dispose() {
    for (final f in _fields) {
      f.clear();
      f.dispose();
    }
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      Text('적어 둔 단어를 확인해요', style: Theme.of(context).textTheme.headlineSmall),
      const SizedBox(height: 16),
      for (var i = 0; i < _fields.length; i++)
        Padding(
          padding: const EdgeInsets.only(bottom: 16),
          child: TextField(
            controller: _fields[i],
            autocorrect: false,
            enableSuggestions: false,
            obscureText: true,
            decoration: InputDecoration(
              labelText: '${widget.positions[i]}번째 단어',
            ),
            textInputAction: i == _fields.length - 1
                ? TextInputAction.done
                : TextInputAction.next,
          ),
        ),
      FilledButton(
        onPressed: widget.busy
            ? null
            : () async {
                final answers = _fields.map((f) => f.text).toList();
                for (final f in _fields) {
                  f.clear();
                }
                await widget.onConfirm(answers);
                answers.clear();
              },
        child: const Text('확인하고 계정 만들기'),
      ),
    ],
  );
}
