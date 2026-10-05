import 'package:flutter/material.dart';

import 'theme/app_theme.dart';
import 'theme/tokens.dart';

/// Shown instead of the app when the Rust library cannot be loaded, so the window
/// says why instead of staying empty.
class StartupErrorApp extends StatelessWidget {
  const StartupErrorApp({super.key, required this.reason});

  final String reason;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'ChatProject',
      theme: buildAppTheme(),
      home: Scaffold(
        body: SafeArea(
          child: Padding(
            padding: const EdgeInsets.fromLTRB(22, 16, 22, 16),
            child: Text(
              '코어를 시작하지 못했어요 · $reason',
              style: const TextStyle(fontSize: 14, color: AppColors.textSecondary),
            ),
          ),
        ),
      ),
    );
  }
}
