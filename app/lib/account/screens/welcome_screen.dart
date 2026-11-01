import 'package:flutter/material.dart';

class WelcomeScreen extends StatelessWidget {
  const WelcomeScreen({
    super.key,
    required this.busy,
    required this.onCreate,
    required this.onRecover,
    this.onPair,
  });
  final bool busy;
  final VoidCallback onCreate, onRecover;
  final VoidCallback? onPair;
  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      Text('내 계정 시작하기', style: Theme.of(context).textTheme.headlineMedium),
      const SizedBox(height: 16),
      const Text('복구 문구로 내 계정을 보관해요. 문구를 아는 사람은 내 계정을 사용할 수 있어요.'),
      const SizedBox(height: 24),
      FilledButton(
        onPressed: busy ? null : onCreate,
        child: const Text('새 계정 만들기'),
      ),
      TextButton(
        onPressed: busy ? null : onRecover,
        child: const Text('복구 문구로 복구'),
      ),
      TextButton(
        onPressed: busy ? null : onPair,
        child: const Text('다른 기기로 연결'),
      ),
    ],
  );
}
