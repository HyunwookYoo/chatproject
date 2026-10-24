import 'dart:convert';
import 'dart:io';
import 'dart:ui' as ui;

import 'package:chat_app/src/rust/api/account.dart' as account;
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
Future<void> main(List<String> arguments) async {
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
  if (!RegExp(r'^[a-zA-Z0-9_-]+$').hasMatch(nonce)) {
    throw StateError('Release contract nonce required');
  }
  final Directory parent;
  if (Platform.isWindows) {
    if (arguments.length != 2 ||
        arguments.first != '--data-dir' ||
        !Directory(arguments.last).isAbsolute) {
      throw StateError('Explicit owned Release root required');
    }
    parent = Directory(arguments.last);
  } else if (Platform.isAndroid) {
    // Android's trusted private cache may use the /data/user/0 alias.
    final temporary = await getTemporaryDirectory();
    parent = Directory(await temporary.resolveSymbolicLinks());
  } else {
    throw StateError('Release SQL contract platform unavailable');
  }
  await parent.create(recursive: true);
  if (await FileSystemEntity.type(parent.path, followLinks: false) !=
          FileSystemEntityType.directory ||
      (await parent.resolveSymbolicLinks()).replaceAll('\\', '/') !=
          parent.absolute.path.replaceAll('\\', '/')) {
    throw StateError('Unsafe Release parent root');
  }
  final root = Directory('${parent.path}/release-contract-$nonce');
  final owner = File('${root.path}/owner.json');
  Map<String, dynamic>? previous;
  if (await FileSystemEntity.type(root.path, followLinks: false) ==
      FileSystemEntityType.notFound) {
    await root.create(recursive: true);
    await owner.create(exclusive: true);
    await owner.writeAsString(
      jsonEncode({'nonce': nonce, 'phase': 'prepared'}),
      flush: true,
    );
  } else {
    await _checkOwnedFiles(root);
    try {
      final decoded = jsonDecode(await owner.readAsString());
      if (decoded is! Map<String, dynamic>) {
        throw StateError('Malformed Release fixture ownership');
      }
      previous = decoded;
    } on FormatException {
      throw StateError('Malformed Release fixture ownership');
    }
    if (previous['nonce'] != nonce ||
        !['created', 'reopened'].contains(previous['phase'])) {
      throw StateError('Release fixture ownership/phase mismatch');
    }
    for (final name in ['db.keys', 'db.sqlite3', 'storage.mode', 'store.id']) {
      if (!await File('${root.path}/$name').exists()) {
        throw StateError('Release SQL reopen material missing');
      }
    }
    if (base64Encode(await File('${root.path}/storage.mode').readAsBytes()) !=
        previous['binding']) {
      throw StateError('Release SQL binding mismatch');
    }
    if (previous['phase'] == 'reopened') {
      if (!Platform.isAndroid) {
        throw StateError('Unexpected Release cleanup launch');
      }
      // Third owned process: no Rust/native account initialization, hence no open
      // SQL handles. Only this unique nonce's enumerated regular files are removed.
      final files = await _checkOwnedFiles(root);
      for (final file in files.where((file) => file.path != owner.path)) {
        await file.delete();
      }
      await owner.delete();
      await root.delete();
      _receipt(nonce, 'cleaned', previous['binding'] as String, 'cleanup-$pid');
      return;
    }
  }
  await _checkOwnedFiles(root);
  await RustLib.init();
  final core = await rust.coreStart(dataDir: root.path);
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
  await account.openAccount();
  final view = await account.snapshot();
  if (view.phase != account.AccountPhase.unconfigured ||
      view.lockError != null ||
      view.publicPeerId != null ||
      view.installation != null) {
    throw StateError('Release SQL sentinel query failed');
  }
  final after = await rust.networkInfo();
  if (after.state != rust.NetworkState.stopped || after.endpointId != null) {
    throw StateError('Release SQL query started networking');
  }
  await _checkOwnedFiles(root);
  for (final name in ['db.keys', 'db.sqlite3', 'storage.mode', 'store.id']) {
    if (!await File('${root.path}/$name').exists()) {
      throw StateError('Release SQL material missing');
    }
  }
  final binding = base64Encode(
    await File('${root.path}/storage.mode').readAsBytes(),
  );
  if (previous != null &&
      (binding != previous['binding'] ||
          core.instanceId == previous['instance'])) {
    throw StateError('Release SQL reopen mismatch');
  }
  if (Platform.isAndroid) await _checkQrImage();
  final phase = previous == null ? 'created' : 'reopened';
  await owner.writeAsString(
    jsonEncode({
      'nonce': nonce,
      'phase': phase,
      'binding': binding,
      'instance': core.instanceId,
    }),
    flush: true,
  );
  _receipt(nonce, phase, binding, core.instanceId);
}

const _ownedNames = {
  'owner.json',
  '.account.lock',
  'db.keys',
  'db.sqlite3',
  'db.sqlite3-wal',
  'db.sqlite3-shm',
  'migration.prep',
  'storage.mode',
  'store.id',
};

Future<List<FileSystemEntity>> _checkOwnedFiles(Directory root) async {
  if (await FileSystemEntity.type(root.path, followLinks: false) !=
          FileSystemEntityType.directory ||
      Directory(
            await root.resolveSymbolicLinks(),
          ).absolute.path.replaceAll('\\', '/') !=
          root.absolute.path.replaceAll('\\', '/')) {
    throw StateError('Unsafe Release fixture root');
  }
  final files = await root.list(followLinks: false).toList();
  for (final file in files) {
    final name = file.path.replaceAll('\\', '/').split('/').last;
    if (!_ownedNames.contains(name) ||
        await FileSystemEntity.type(file.path, followLinks: false) !=
            FileSystemEntityType.file) {
      throw StateError('Unexpected Release fixture entry; preserved');
    }
  }
  return files;
}

void _receipt(String nonce, String phase, String binding, String instance) {
  // Public binding and instance only. Wrapped keys never cross into Dart.
  // Final success belongs to the runner after reopen, process exit and cleanup.
  // ignore: avoid_print
  print(
    jsonEncode({
      'type': 'release_contract',
      'version': 2,
      'mode': 'release',
      'nonce': nonce,
      'hooks': phase == 'cleaned' ? 'notRun' : 'notAvailable',
      'network': 'stopped',
      'endpoint': null,
      'phase': phase,
      'binding': binding,
      'instance': instance,
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
    QrPainter(
      data: payload,
      version: QrVersions.auto,
    ).paint(canvas, const Size(448, 448));
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
