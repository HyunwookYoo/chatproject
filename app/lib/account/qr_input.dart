import 'dart:async';
import 'package:flutter/foundation.dart';
import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';
import 'package:qr_flutter/qr_flutter.dart';

/// One original payload for both renderings. Oversized payloads remain complete text.
class QrPayload extends StatelessWidget {
  const QrPayload({super.key, required this.payload});
  final String payload;
  @override
  Widget build(BuildContext context) => Column(
    children: [
      LayoutBuilder(
        builder: (context, constraints) => QrImageView(
          data: payload,
          version: QrVersions.auto,
          size: constraints.maxWidth.clamp(0, 320),
          backgroundColor: Colors.white,
          errorStateBuilder: (_, _) =>
              const Text('QR로 표시하기에 너무 길어요. 전체 텍스트를 사용해 주세요.'),
        ),
      ),
      const SizedBox(height: 12),
      SelectableText(payload, textDirection: TextDirection.ltr),
    ],
  );
}

/// A new widget key starts a new detection flow; duplicate detections never deliver twice.
class QrInput extends StatefulWidget {
  const QrInput({super.key, required this.onPayload, this.busy = false});
  final ValueChanged<String> onPayload;
  final bool busy;
  @override
  State<QrInput> createState() => _QrInputState();
}

class _QrInputState extends State<QrInput> with WidgetsBindingObserver {
  final _text = TextEditingController();
  MobileScannerController? _camera;
  bool _delivered = false;
  bool _cameraVisible = false;
  bool get _mobile =>
      !kIsWeb &&
      (defaultTargetPlatform == TargetPlatform.android ||
          defaultTargetPlatform == TargetPlatform.iOS);
  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
  }

  void _deliver(String value) {
    if (_delivered || widget.busy || value.isEmpty) return;
    setState(() {
      _delivered = true;
      _cameraVisible = false;
    });
    _text.clear();
    unawaited(_camera?.stop());
    widget.onPayload(value);
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state != AppLifecycleState.resumed) {
      _text.clear();
      if (_camera?.value.hasCameraPermission ?? false) {
        unawaited(_camera?.stop());
        if (mounted) setState(() => _cameraVisible = false);
      }
    }
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _text.clear();
    _text.dispose();
    unawaited(_camera?.dispose());
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      if (!_mobile) const Text('이 기기에서는 QR을 표시해 반대편에서 스캔하거나 텍스트를 붙여넣어 주세요.'),
      if (_mobile && !_delivered)
        TextButton(
          onPressed: widget.busy
              ? null
              : () {
                  _camera ??= MobileScannerController(
                    formats: [BarcodeFormat.qrCode],
                  );
                  setState(() => _cameraVisible = !_cameraVisible);
                  if (!_cameraVisible) unawaited(_camera?.stop());
                },
          child: const Text('QR 스캔'),
        ),
      if (_cameraVisible)
        SizedBox(
          height: 240,
          child: MobileScanner(
            controller: _camera,
            onDetect: (capture) {
              for (final barcode in capture.barcodes) {
                if (barcode.rawValue case final String value) {
                  _deliver(value);
                  break;
                }
              }
            },
            errorBuilder: (_, _) => const Center(
              child: Text(
                '카메라를 사용할 수 없어요. 권한을 확인하거나 붙여넣기 또는 반대 방향 QR을 사용해 주세요.',
              ),
            ),
          ),
        ),
      TextField(
        controller: _text,
        enabled: !_delivered && !widget.busy,
        autocorrect: false,
        enableSuggestions: false,
        decoration: const InputDecoration(labelText: 'QR 텍스트 붙여넣기'),
        onSubmitted: _deliver,
      ),
      FilledButton(
        onPressed: _delivered || widget.busy
            ? null
            : () => _deliver(_text.text),
        child: const Text('입력 확인'),
      ),
      if (_delivered) const Text('입력을 전달했어요. 다시 시도하려면 새 입력을 시작해 주세요.'),
    ],
  );
}
