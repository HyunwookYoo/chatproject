import 'package:flutter/material.dart';

import 'bootstrap.dart';
import 'core_client.dart';
import 'home/home_screen.dart';
import 'network/network_client.dart';
import 'probe/probe_host.dart';
import 'src/rust/frb_generated.dart';
import 'theme/app_theme.dart';

Future<void> main(List<String> arguments) async {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(
    await bootstrap(
      arguments,
      initializeRust: RustLib.init,
      createClient: (dataDir) => FrbCoreClient(dataDir: dataDir),
    ),
  );
}

class ChatApp extends StatelessWidget {
  const ChatApp({super.key, required this.core, this.probe, this.network});

  final CoreClient core;
  final ProbeHost? probe;
  final NetworkClient? network;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'ChatProject',
      theme: buildAppTheme(),
      home: HomeScreen(core: core, probe: probe, network: network),
    );
  }
}
