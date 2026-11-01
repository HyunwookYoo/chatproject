import 'package:chat_app/account/screens/invite_screen.dart';
import 'dart:async';
import 'package:flutter/material.dart';
import 'package:flutter/foundation.dart';
import 'package:chat_app/account/qr_input.dart';
import 'package:chat_app/account/invite_links.dart';
import 'package:mobile_scanner/mobile_scanner.dart';
import 'package:qr_flutter/qr_flutter.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  moreTests();
  testWidgets('public invitation offers QR scan alongside paste', (
    tester,
  ) async {
    await tester.pumpWidget(
      MaterialApp(
        home: Scaffold(
          body: InviteScreen(
            busy: false,
            onParse: (_) async {},
            onCreate: (_) async => null,
          ),
        ),
      ),
    );
    expect(find.text('QR 스캔'), findsOneWidget);
  });
}

class ScannerFixture extends MobileScannerPlatform {
  bool denied = false;
  int stops = 0;
  final captures = StreamController<BarcodeCapture?>.broadcast();
  @override
  Stream<BarcodeCapture?> get barcodesStream => captures.stream;
  @override
  Stream<TorchState> get torchStateStream => const Stream.empty();
  @override
  Stream<double> get zoomScaleStateStream => const Stream.empty();
  @override
  Future<MobileScannerViewAttributes> start(StartOptions options) async {
    if (denied) {
      throw const MobileScannerException(
        errorCode: MobileScannerErrorCode.permissionDenied,
      );
    }
    return const MobileScannerViewAttributes(
      cameraDirection: CameraFacing.back,
      currentTorchMode: TorchState.off,
      size: Size(320, 240),
    );
  }

  @override
  Widget buildCameraView() => const SizedBox();
  @override
  Future<void> stop() async {
    stops++;
  }

  @override
  Future<void> dispose() async {}
  @override
  Future<void> updateScanWindow(Rect? window) async {}
}

void moreTests() {
  testWidgets(
    'camera duplicate delivery once and permission denied keeps paste fallback',
    (t) async {
      final previous = MobileScannerPlatform.instance;
      final fixture = ScannerFixture();
      MobileScannerPlatform.instance = fixture;
      addTearDown(() async {
        MobileScannerPlatform.instance = previous;
        await fixture.captures.close();
      });
      final values = <String>[];
      await t.pumpWidget(
        MaterialApp(
          home: Scaffold(body: QrInput(onPayload: values.add)),
        ),
      );
      await t.tap(find.text('QR 스캔'));
      await t.pumpAndSettle();
      final capture = BarcodeCapture(
        barcodes: [const Barcode(rawValue: 'PUBLIC_INVALID_QR')],
      );
      final callback = t
          .widget<MobileScanner>(find.byType(MobileScanner))
          .onDetect!;
      callback(capture);
      callback(capture);
      await t.pumpAndSettle();
      expect(values, ['PUBLIC_INVALID_QR']);
      expect(fixture.stops, greaterThan(0));
      await t.pumpWidget(const SizedBox());
      await t.pumpAndSettle();
      fixture.denied = true;
      await t.pumpWidget(
        MaterialApp(
          home: Scaffold(body: QrInput(onPayload: values.add)),
        ),
      );
      await t.tap(find.text('QR 스캔'));
      await t.pumpAndSettle();
      expect(find.textContaining('권한을 확인하거나 붙여넣기'), findsOneWidget);
      await t.enterText(find.byType(TextField), 'PUBLIC_PASTE');
      await t.tap(find.text('입력 확인'));
      await t.pumpAndSettle();
      expect(values, ['PUBLIC_INVALID_QR', 'PUBLIC_PASTE']);
      await t.pumpWidget(const SizedBox());
      await t.pumpAndSettle();
    },
  );
  testWidgets('Windows no camera and paste single delivery', (t) async {
    debugDefaultTargetPlatformOverride = TargetPlatform.windows;
    addTearDown(() => debugDefaultTargetPlatformOverride = null);
    final values = <String>[];
    await t.pumpWidget(
      MaterialApp(
        home: Scaffold(body: QrInput(onPayload: values.add)),
      ),
    );
    expect(find.text('QR 스캔'), findsNothing);
    expect(find.textContaining('반대편에서 스캔'), findsOneWidget);
    await t.enterText(find.byType(TextField), 'PUBLIC_PASTE');
    final submit = t.widget<TextField>(find.byType(TextField)).onSubmitted!;
    submit('PUBLIC_PASTE');
    submit('PUBLIC_PASTE');
    await t.pumpAndSettle();
    expect(values, ['PUBLIC_PASTE']);
    debugDefaultTargetPlatformOverride = null;
  });
  testWidgets(
    'QR and text use identical full payload at 200 percent narrow width',
    (t) async {
      final payload = 'PUBLIC_${List.filled(1500, 'x').join()}';
      await t.pumpWidget(
        MaterialApp(
          home: Scaffold(
            body: MediaQuery(
              data: const MediaQueryData(textScaler: TextScaler.linear(2)),
              child: SingleChildScrollView(
                child: SizedBox(width: 320, child: QrPayload(payload: payload)),
              ),
            ),
          ),
        ),
      );
      final actual = t
          .widgetList<CustomPaint>(find.byType(CustomPaint))
          .map((p) => p.painter)
          .whereType<QrPainter>()
          .single;
      final expected = QrPainter(
        data: payload,
        version: QrVersions.auto,
        gapless: true,
      );
      await t.runAsync(() async {
        final a = await actual.toImageData(320),
            b = await expected.toImageData(320);
        expect(a!.buffer.asUint8List(), b!.buffer.asUint8List());
      });
      expect(find.text(payload), findsOneWidget);
      expect(t.takeException(), isNull);
    },
  );
  test('public invitation intake excludes pair secrets and HTTP', () {
    expect(
      InviteLinks.isPublicInvite('chatproject:identity-invite/1/PUBLIC'),
      isTrue,
    );
    expect(InviteLinks.isPublicInvite('chatproject:pair/1/PUBLIC'), isFalse);
    expect(InviteLinks.isPublicInvite('https://example.invalid'), isFalse);
  });
}
