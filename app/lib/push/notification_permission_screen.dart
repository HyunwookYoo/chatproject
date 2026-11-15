import 'package:flutter/material.dart';

import 'push_model.dart';

/// The notification question (M7 spec 4, 8): after the recovery phrase check
/// or a recovery, and once after the update on a device onboarded before.
/// Android and iOS only.
class NotificationPermissionScreen extends StatelessWidget {
  const NotificationPermissionScreen({
    super.key,
    required this.model,
    required this.onDone,
  });
  final PushModel model;
  final VoidCallback onDone;

  Future<void> _answer(Future<void> Function() answer) async {
    await answer();
    onDone();
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: model,
    builder: (context, _) => Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Text(
          '새 메시지를 알림으로 알려 드릴까요?',
          style: Theme.of(context).textTheme.headlineSmall,
        ),
        const SizedBox(height: 24),
        FilledButton(
          onPressed: model.busy ? null : () => _answer(model.allow),
          child: const Text('알림 켜기'),
        ),
        TextButton(
          onPressed: model.busy ? null : () => _answer(model.later),
          child: const Text('나중에'),
        ),
      ],
    ),
  );
}
