import 'package:flutter/material.dart';

import 'core_client.dart';
import 'home/home_screen.dart';
import 'src/rust/frb_generated.dart';
import 'startup_error_app.dart';
import 'theme/app_theme.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  try {
    await RustLib.init();
  } catch (error) {
    // Missing or wrong-ABI library, or stale generated bindings: say so instead of showing nothing.
    runApp(StartupErrorApp(reason: '$error'));
    return;
  }
  runApp(ChatApp(core: FrbCoreClient()));
}

class ChatApp extends StatelessWidget {
  const ChatApp({super.key, required this.core});

  final CoreClient core;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'ChatProject',
      theme: buildAppTheme(),
      home: HomeScreen(core: core),
    );
  }
}
