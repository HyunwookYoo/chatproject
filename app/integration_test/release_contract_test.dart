import 'dart:convert';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:chat_app/src/rust/api/chat.dart' as rust;
import 'package:chat_app/src/rust/frb_generated.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/widgets.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:mobile_scanner/mobile_scanner.dart';
import 'package:path_provider/path_provider.dart';
import 'package:qr_flutter/qr_flutter.dart';

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
  if (Platform.isAndroid) await _checkQrImage();
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

// Exercise the actual minified Android decoder without camera permission or
// account material. The nonce receipt is emitted only after exact decoding.
Future<void> _checkQrImage() async {
  const payload = 'chatproject-public-release-qr-v1';
  final root = await (await getTemporaryDirectory()).createTemp('release-qr-');
  final file = File('${root.path}/public.png');
  final scanner = MobileScannerController(autoStart: false);
  try {
    final recorder = ui.PictureRecorder();
    final canvas = Canvas(recorder);
    canvas.drawColor(const Color(0xFFFFFFFF), BlendMode.src);
    canvas.translate(32, 32);
    QrPainter(data: payload, version: QrVersions.auto).paint(
      canvas,
      const Size(448, 448),
    );
    final picture = recorder.endRecording();
    final image = await picture.toImage(512, 512);
    picture.dispose();
    final png = await image.toByteData(format: ui.ImageByteFormat.png);
    image.dispose();
    if (png == null) throw StateError('Release public QR image unavailable');
    await file.writeAsBytes(png.buffer.asUint8List(), flush: true);
    final capture = await scanner.analyzeImage(
      file.path,
      formats: const [BarcodeFormat.qrCode],
    );
    if (capture?.barcodes.length != 1 ||
        capture!.barcodes.single.rawValue != payload) {
      throw StateError('Release public QR image decode mismatch');
    }
  } finally {
    try {
      await scanner.dispose();
    } finally {
      if (await file.exists()) await file.delete();
      await root.delete();
    }
  }
}
