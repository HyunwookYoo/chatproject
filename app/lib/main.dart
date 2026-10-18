import 'package:flutter/material.dart';

import 'bootstrap.dart';
import 'account/account_model.dart';
import 'account/account_routes.dart';
import 'account/invite_links.dart';
import 'core_client.dart';
import 'home/home_screen.dart';
import 'network/network_client.dart';
import 'probe/probe_host.dart';
import 'src/rust/frb_generated.dart';
import 'theme/app_theme.dart';

Future<void> main(List<String> arguments) async {
  WidgetsFlutterBinding.ensureInitialized();
  for (final argument in arguments) {
    if (InviteLinks.isPublicInvite(argument)) InviteLinks.initial = argument;
  }
  runApp(
    await bootstrap(
      arguments.where((value) => !value.startsWith('chatproject:')).toList(),
      initializeRust: RustLib.init,
      createClient: (dataDir) => FrbCoreClient(dataDir: dataDir),
    ),
  );
}

class ChatApp extends StatelessWidget {
  const ChatApp({
    super.key,
    required this.core,
    this.probe,
    this.network,
    this.account,
  });

  final CoreClient core;
  final AccountModel? account;
  final ProbeHost? probe;
  final NetworkClient? network;

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      title: 'ChatProject',
      theme: buildAppTheme(),
      home: account == null
          ? HomeScreen(core: core, probe: probe, network: network)
          : AccountRoutes(
              model: account!,
              child: HomeScreen(core: core, probe: probe, network: network),
            ),
    );
  }
}
