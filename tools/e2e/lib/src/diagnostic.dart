import 'dart:async';
import 'dart:convert';
import 'dart:io';
import 'dart:isolate';

import 'protocol.dart';

/// Fixed public failure texts the retained summary may name. The checked-in
/// list is shared with tools/ci/write_e2e_summary.py.
Set<String> loadSafeDetails() => loadSafeValues('details');

/// One fixed value list of tools/e2e/diagnostic_details.json (`details`,
/// `last_view`, `participants`).
Set<String> loadSafeValues(String key) {
  final source = Isolate.resolvePackageUriSync(
    Uri.parse('package:chat_e2e/src/diagnostic.dart'),
  )!;
  final file = File.fromUri(source.resolve('../../diagnostic_details.json'));
  final data = jsonDecode(file.readAsStringSync()) as Map<String, dynamic>;
  return (data[key] as List).cast<String>().toSet();
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

/// A scenario wait that ran out. Besides its fixed text it names, as fixed
/// values, what the last read showed (an account phase, `NotAvailable` or
/// `other`) and which participant it read.
class ScenarioDeadline extends TimeoutException {
  ScenarioDeadline(String super.message, {this.lastView, this.participant});
  final String? lastView;
  final String? participant;
}

const _phases = {
  'Unconfigured',
  'Candidate',
  'PendingImport',
  'LocalReady',
  'PublishPending',
  'Active',
  'Conflict',
  'Locked',
};

/// The fixed category of one read in a scenario wait: an account snapshot's
/// phase, `NotAvailable` for that RPC error, otherwise `other`.
String observedView(Object? read) {
  if (read is RpcError) {
    return read.code == 'NotAvailable' ? 'NotAvailable' : 'other';
  }
  if (read is Map && _phases.contains(read['phase'])) {
    return read['phase'] as String;
  }
  return 'other';
}
