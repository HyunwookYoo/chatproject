import 'dart:async';
import 'package:flutter/material.dart';
import '../core_client.dart';

/// Shared protection: covered routes cannot disable a newer route.
class SensitiveScreen extends StatefulWidget {
  const SensitiveScreen({
    super.key,
    required this.child,
    this.onHidden,
    this.onProtectionFailure,
  });
  final Widget child;
  final VoidCallback? onHidden;
  final VoidCallback? onProtectionFailure;
  @override
  State<SensitiveScreen> createState() => _SensitiveScreenState();
}

class _SensitiveScreenState extends State<SensitiveScreen>
    with WidgetsBindingObserver {
  static int _references = 0;
  static final _lifecycle = _ProtectionLifecycle();
  static Future<void>? _queue;
  bool _ready = false, _hidden = false, _failed = false;
  static Future<void> _native(bool value) {
    final previous = _queue;
    final next = previous == null
        ? setSensitiveScreen(value)
        : previous.then((_) => setSensitiveScreen(value));
    final settled = next.catchError((Object _) {});
    _queue = settled;
    unawaited(
      settled.then((_) {
        if (identical(_queue, settled)) _queue = null;
      }),
    );
    return next;
  }

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    if (_references++ == 0) {
      WidgetsBinding.instance.addObserver(_lifecycle);
      _lifecycle.background =
          WidgetsBinding.instance.lifecycleState != null &&
          WidgetsBinding.instance.lifecycleState != AppLifecycleState.resumed;
    }
    _protect();
  }

  Future<void> _protect() async {
    try {
      await _native(true);
      if (mounted && !_hidden) setState(() => _ready = true);
    } catch (_) {
      if (mounted) {
        setState(() => _failed = true);
        widget.onProtectionFailure?.call();
      }
    }
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state != AppLifecycleState.resumed) {
      unawaited(
        _native(true).catchError((Object _) {
          if (mounted) setState(() => _failed = true);
        }),
      );
      if (!_hidden) {
        setState(() {
          _hidden = true;
          _ready = false;
        });
        widget.onHidden?.call();
      }
    }
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _references--;
    if (_references == 0 && !_lifecycle.background) {
      WidgetsBinding.instance.removeObserver(_lifecycle);
      unawaited(_native(false).catchError((Object _) {}));
    }
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => _ready && !_hidden && !_failed
      ? widget.child
      : Center(
          child: Padding(
            padding: const EdgeInsets.all(24),
            child: Text(
              _failed
                  ? '민감한 화면을 보호할 수 없어요. 잠시 뒤 다시 시도해 주세요.'
                  : '민감한 내용을 숨겼어요. 다시 시작해 주세요.',
            ),
          ),
        );
}

/// Remains registered through background even if the last secret route closes.
class _ProtectionLifecycle extends WidgetsBindingObserver {
  bool background = false;
  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    background = state != AppLifecycleState.resumed;
    if (!background && _SensitiveScreenState._references == 0) {
      WidgetsBinding.instance.removeObserver(this);
      unawaited(_SensitiveScreenState._native(false).catchError((Object _) {}));
    }
  }
}
