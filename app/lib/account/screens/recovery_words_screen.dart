import 'package:flutter/material.dart';
import 'package:flutter/semantics.dart';

class RecoveryWordsScreen extends StatelessWidget {
  const RecoveryWordsScreen({
    super.key,
    required this.words,
    required this.onContinue,
  });
  final List<String> words;
  final VoidCallback onContinue;
  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      Text(
        '24단어를 안전한 곳에 적어 주세요',
        style: Theme.of(context).textTheme.headlineSmall,
      ),
      const SizedBox(height: 16),
      const Text('이 문구는 다시 볼 수 없어요. 다른 사람에게 보내거나 공개하지 마세요.'),
      const SizedBox(height: 16),
      ExcludeSemantics(
        child: Column(
          crossAxisAlignment: CrossAxisAlignment.start,
          children: [
            for (var i = 0; i < words.length; i++)
              Text('${i + 1}. ${words[i]}', textDirection: TextDirection.ltr),
          ],
        ),
      ),
      TextButton(
        onPressed: () => SemanticsService.sendAnnouncement(
          View.of(context),
          '복구 문구. ${List.generate(words.length, (i) => '${i + 1}번 ${words[i]}').join(', ')}',
          TextDirection.ltr,
        ),
        child: const Text('문구 읽어주기'),
      ),
      FilledButton(onPressed: onContinue, child: const Text('적어 두었어요')),
    ],
  );
}
