import 'dart:convert';

import 'package:chat_app/src/rust/api/chat.dart' as rust;
import 'package:chat_app/src/rust/frb_generated.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:path_provider/path_provider.dart';

// Flutter 3.44.6 drive rejects non-web Release. Build this AOT entry and launch
// it directly with release_smoke.dart; ordinary main.dart is smoked separately.
Future<void> main() async {
  if (!kReleaseMode) {
    IntegrationTestWidgetsFlutterBinding.ensureInitialized();
    test(
      'Release contract requires the dedicated AOT direct launch',
      () {},
      skip: true,
    );
    return;
  }
  WidgetsFlutterBinding.ensureInitialized();
  runApp(
    const Directionality(textDirection: TextDirection.ltr, child: SizedBox()),
  );
  const nonce = String.fromEnvironment('RELEASE_CONTRACT_NONCE');
  if (nonce.isEmpty) throw StateError('Release contract nonce required');
  await RustLib.init();
  final support = await getApplicationSupportDirectory();
  await rust.coreStart(dataDir: '${support.path}/release-contract');
  var unavailable = false;
  try {
    await rust.testHooksStart(
      controlUrl: 'ws://127.0.0.1:47000',
      name: 'forbidden',
    );
  } on rust.CoreError catch (error) {
    if (error.code != rust.ErrorCode.notAvailable) rethrow;
    unavailable = true;
  }
  if (!unavailable) {
    throw StateError('Release controller unexpectedly available');
  }
  final info = await rust.networkInfo();
  if (info.state != rust.NetworkState.stopped || info.endpointId != null) {
    throw StateError('Release core did not start offline');
  }
  // Explicit checks above survive AOT assertion stripping. This contains no
  // endpoint identifiers or private runtime data; the nonce binds the build.
  // ignore: avoid_print
  print(
    jsonEncode({
      'type': 'release_contract',
      'version': 1,
      'mode': 'release',
      'nonce': nonce,
      'hooks': 'notAvailable',
      'network': 'stopped',
      'endpoint': null,
    }),
  );
}
