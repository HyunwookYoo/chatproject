import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:isolate';

import 'protocol.dart';

/// Fixed public failure texts the retained summary may name. The checked-in
/// list is shared with tools/ci/write_e2e_summary.py.
Set<String> loadSafeDetails() {
  final source = Isolate.resolvePackageUriSync(
    Uri.parse('package:chat_e2e/src/diagnostic.dart'),
  )!;
  final file = File.fromUri(source.resolve('../../diagnostic_details.json'));
  final data = jsonDecode(file.readAsStringSync()) as Map<String, dynamic>;
  return (data['details'] as List).cast<String>().toSet();
}

/// The failure's own text only when it is one of [allowed]; otherwise null.
String? safeDetail(Object? error, Set<String> allowed) {
  final text = switch (error) {
    StateError() => error.message,
    TimeoutException() => error.message,
    RpcError() => error.toString(),
    _ => null,
  };
  return allowed.contains(text) ? text : null;
}
