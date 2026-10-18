import 'dart:async';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter/scheduler.dart';
import '../core_client.dart';

/// Shared protection: covered routes cannot disable a newer route.
class SensitiveScreen extends StatefulWidget {
  const SensitiveScreen({
    super.key,
    required this.child,
    this.onHidden,
    this.onProtectionFailure,
  });
  @visibleForTesting
  static int Function()? frameNumberForTesting;
  final Widget child;
  final VoidCallback? onHidden;
  final VoidCallback? onProtectionFailure;
  @override
  State<SensitiveScreen> createState() => _SensitiveScreenState();
}

class _SensitiveScreenState extends State<SensitiveScreen>
    with WidgetsBindingObserver {
  static int _references = 0;
  static const _events = MethodChannel('chat_app/privacy_events');
  static final _screens = <_SensitiveScreenState>{};
  void _nativeFailure() {
    if (!mounted) return;
    setState(() {
      _failed = true;
      _ready = false;
    });
    widget.onProtectionFailure?.call();
  }

  static final _lifecycle = _ProtectionLifecycle();
  static Future<void>? _queue;
  static int _releaseTicket = 0;
  static TimingsCallback? _releaseTiming;
  static bool _watchingLifecycle = false;
  static void _cancelRelease() {
    ++_releaseTicket;
    final callback = _releaseTiming;
    if (callback != null) {
      SchedulerBinding.instance.removeTimingsCallback(callback);
    }
    _releaseTiming = null;
  }

  static void _releaseAfterRaster() {
    _cancelRelease();
    if (_references != 0 || _lifecycle.background) return;
    final binding = WidgetsBinding.instance;
    final removedFrame =
        SensitiveScreen.frameNumberForTesting?.call() ??
        binding.platformDispatcher.frameData.frameNumber;
    if (removedFrame < 0) return; // No frame identity: keep native protection.
    final ticket = _releaseTicket;
    int? replacementFrame;
    void completed(List<FrameTiming> frames) {
      if (replacementFrame == null ||
          !frames.any((frame) => frame.frameNumber == replacementFrame)) {
        return;
      }
      if (ticket != _releaseTicket ||
          _references != 0 ||
          _lifecycle.background) {
        return;
      }
      _cancelRelease();
      unawaited(
        _native(false)
            .then((_) {
              if (_references == 0 && !_lifecycle.background) {
                binding.removeObserver(_lifecycle);
                _watchingLifecycle = false;
              }
            })
            .catchError((Object _) {}),
      );
    }

    _releaseTiming = completed;
    binding.addTimingsCallback(completed);
    binding.addPostFrameCallback((_) {
      if (ticket != _releaseTicket) return;
      // Identify the next complete build, not an older timing delivered later.
      binding.addPostFrameCallback((_) {
        if (ticket != _releaseTicket) return;
        final frame =
            SensitiveScreen.frameNumberForTesting?.call() ??
            binding.platformDispatcher.frameData.frameNumber;
        if (frame <= removedFrame) {
          _cancelRelease(); // Unknown/non-advancing metadata stays protected.
          return;
        }
        replacementFrame = frame;
      });
      binding.scheduleFrame();
    });
    binding.scheduleFrame();
  }

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
    _screens.add(this);
    _events.setMethodCallHandler((call) async {
      if (call.method == 'protectionFailed') {
        for (final screen in _screens.toList()) {
          screen._nativeFailure();
        }
      }
    });
    _cancelRelease();
    if (_references++ == 0) {
      if (!_watchingLifecycle) {
        WidgetsBinding.instance.addObserver(_lifecycle);
        _watchingLifecycle = true;
      }
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
    _screens.remove(this);
    if (_screens.isEmpty) _events.setMethodCallHandler(null);
    _references--;
    if (_references == 0 && !_lifecycle.background) {
      _releaseAfterRaster();
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
    if (background) {
      _SensitiveScreenState._cancelRelease();
    } else if (_SensitiveScreenState._references == 0) {
      _SensitiveScreenState._releaseAfterRaster();
    }
  }
}
