import 'dart:async';

/// Cleanup belongs to the run that acquired the resource, in reverse order.
class OwnedResources {
  final _items = <(String, Future<void> Function())>[];
  Future<List<String>>? _closing;

  void add(String label, Future<void> Function() cleanup) {
    if (_closing != null) throw StateError('Resource admission is closed');
    _items.add((label, cleanup));
  }

  Future<List<String>> close() => _closing ??= Future(_close);

  Future<List<String>> _close() async {
    final failures = <String>[];
    for (final item in _items.reversed) {
      try {
        await item.$2();
      } catch (error) {
        failures.add('${item.$1}: $error');
      }
    }
    _items.clear();
    return failures;
  }
}

class RunCancelled implements Exception {}

/// Admission closes synchronously; callers still await in-flight acquisition.
class RunCancellation {
  bool _cancelled = false;
  final _requested = Completer<void>();
  Future<void> get cancelled => _requested.future;
  bool get isCancelled => _cancelled;
  void cancel() {
    _cancelled = true;
    if (!_requested.isCompleted) _requested.complete();
  }

  void check() {
    if (_cancelled) throw RunCancelled();
  }
}
