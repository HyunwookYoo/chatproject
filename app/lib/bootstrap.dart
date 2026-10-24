import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';

import 'core_client.dart';
import 'account/account_client.dart';
import 'account/account_model.dart';
import 'account/screens/devices_screen.dart';
import 'account/screens/pair_screen.dart';
import 'src/rust/api/account.dart' as account;
import 'main.dart';
import 'network/network_client.dart';
import 'probe/probe_host.dart';
import 'src/rust/api/chat.dart' as rust;
import 'startup_error_app.dart';

Future<Widget> bootstrap(
  List<String> arguments, {
  required Future<void> Function() initializeRust,
  required CoreClient Function(String?) createClient,
  Future<void> Function(String, String)? startTestHooks,
  AccountClient? Function(CoreClient)? createAccountClient,
}) async {
  var startingHooks = false;
  try {
    final dataDir = startupDataDir(arguments, releaseMode: kReleaseMode);
    await initializeRust();
    final core = createClient(dataDir);
    await core.start();
    if (kDebugMode) {
      final control = _controlArguments(arguments);
      if (control != null) {
        startingHooks = true;
        await (startTestHooks ?? _startTestHooks)(control.$1, control.$2);
        startingHooks = false;
      }
    }
    final accountClient =
        createAccountClient?.call(core) ??
        (core is FrbCoreClient ? core.account : null);
    final accountModel = accountClient == null
        ? null
        : AccountModel(accountClient);
    await accountModel?.open();
    final app = ChatApp(
      core: core,
      account: accountModel,
      messages: core is FrbCoreClient ? core.messages : null,
      network: core is NetworkClient ? core as NetworkClient : null,
      probe: Platform.isIOS ? IosProbeHost() : null,
    );
    return accountModel == null
        ? app
        : AccountGate(model: accountModel, child: app);
  } catch (error, stack) {
    final reason = error is rust.CoreError
        ? error.code.name
        : startingHooks
        ? 'Debug control startup failed'
        : '$error';
    debugPrint('ChatProject startup failed: $reason');
    debugPrintStack(label: 'ChatProject startup failed', stackTrace: stack);
    return StartupErrorApp(reason: reason);
  }
}

/// Parses startup configuration before library loading or core creation.
String? startupDataDir(List<String> arguments, {required bool releaseMode}) {
  String? dataDir;
  for (var i = 0; i < arguments.length; i++) {
    final argument = arguments[i];
    if (releaseMode &&
        [
          '--debug-hooks',
          '--control-url',
          '--name',
        ].any((flag) => argument == flag || argument.startsWith('$flag='))) {
      throw const FormatException(
        'Debug control arguments are unavailable in Release',
      );
    }
    if (argument == '--data-dir') {
      if (++i >= arguments.length) {
        throw const FormatException('--data-dir requires an absolute path');
      }
      dataDir = arguments[i];
    } else if (argument.startsWith('--data-dir=')) {
      dataDir = argument.substring('--data-dir='.length);
    }
  }
  if (dataDir != null &&
      !(Platform.isWindows
          ? RegExp(r'^(?:[A-Za-z]:[\\/]|\\\\[^\\]+\\[^\\]+)').hasMatch(dataDir)
          : dataDir.startsWith('/'))) {
    throw const FormatException('--data-dir requires an absolute path');
  }
  return dataDir;
}

Future<void> _startTestHooks(String url, String name) =>
    rust.testHooksStart(controlUrl: url, name: name);

(String, String)? _controlArguments(List<String> arguments) {
  String? url;
  String? name;
  for (var i = 0; i < arguments.length; i++) {
    final argument = arguments[i];
    for (final flag in ['--control-url', '--name']) {
      String? value;
      if (argument == flag) {
        if (++i >= arguments.length) {
          throw const FormatException('Debug control arguments require values');
        }
        value = arguments[i];
      } else if (argument.startsWith('$flag=')) {
        value = argument.substring(flag.length + 1);
      }
      if (value != null) {
        if (flag == '--control-url') {
          url = value;
        } else {
          name = value;
        }
      }
    }
  }
  if (url == null && name == null) return null;
  if (url == null || url.isEmpty || name == null || name.isEmpty) {
    throw const FormatException(
      'Debug control requires --control-url and --name',
    );
  }
  return (url, name);
}

/// Minimal safe startup boundary; account setup/recovery screens are added separately.
class AccountGate extends StatefulWidget {
  const AccountGate({super.key, required this.model, required this.child});
  final AccountModel model;
  final Widget child;
  @override
  State<AccountGate> createState() => _AccountGateState();
}

class _AccountGateState extends State<AccountGate> {
  @override
  void dispose() {
    widget.model.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => ListenableBuilder(
    listenable: widget.model,
    builder: (context, _) =>
        widget.model.snapshot?.phase != account.AccountPhase.locked
        ? widget.child
        : MaterialApp(
            home: Scaffold(
              body: SafeArea(
                child: Center(
                  child: Padding(
                    padding: const EdgeInsets.all(24),
                    child: Column(
                      mainAxisSize: MainAxisSize.min,
                      children: [
                        const Text('계정 보호 저장을 열 수 없어요.'),
                        if (widget
                                .model
                                .snapshot!
                                .pendingPairTransactions
                                .isNotEmpty ||
                            widget
                                .model
                                .snapshot!
                                .pendingImportRequiresRemoteRevoke)
                          Builder(
                            builder: (context) => TextButton(
                              onPressed: () => Navigator.of(context).push(
                                MaterialPageRoute<void>(
                                  builder: (routeContext) => DevicesScreen(
                                    model: widget.model,
                                    onPair: (resume) =>
                                        Navigator.of(routeContext).push(
                                          MaterialPageRoute<void>(
                                            builder: (_) => PairScreen(
                                              model: widget.model,
                                              resume: resume,
                                            ),
                                          ),
                                        ),
                                  ),
                                ),
                              ),
                              child: const Text('중단된 연결 상태 확인'),
                            ),
                          ),
                        const SizedBox(height: 16),
                        Text(
                          widget.model.error ??
                              accountErrorMessage(rust.ErrorCode.storageLocked),
                        ),
                        const SizedBox(height: 16),
                        FilledButton(
                          onPressed: widget.model.busy
                              ? null
                              : widget.model.retry,
                          child: const Text('다시 시도'),
                        ),
                      ],
                    ),
                  ),
                ),
              ),
            ),
          ),
  );
}
