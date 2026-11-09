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

/// The allowed fact keys and their values (`facts`).
Map<String, Set<String>> loadSafeFacts() {
  final source = Isolate.resolvePackageUriSync(
    Uri.parse('package:chat_e2e/src/diagnostic.dart'),
  )!;
  final file = File.fromUri(source.resolve('../../diagnostic_details.json'));
  final data = jsonDecode(file.readAsStringSync()) as Map<String, dynamic>;
  return {
    for (final MapEntry(:key, :value)
        in (data['facts'] as Map<String, dynamic>).entries)
      key: (value as List).cast<String>().toSet(),
  };
}

/// Only listed keys with listed values; null when nothing is left.
Map<String, String>? safeFacts(
  Map<String, String> facts,
  Map<String, Set<String>> allowed,
) {
  final kept = {
    for (final MapEntry(:key, :value) in facts.entries)
      if (allowed[key]?.contains(value) == true) key: value,
  };
  return kept.isEmpty ? null : kept;
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

/// Fixed facts a scenario notes about the state a failure left behind, e.g. both
/// sides of a failed pairing. Keys and values are checked against
/// `facts` in tools/e2e/diagnostic_details.json before they are kept.
final failureFacts = <String, String>{};

const _pairPhases = {
  'WaitingQr',
  'Comparing',
  'Approved',
  'AwaitingApproval',
  'DisclosurePossible',
  'ReceiptReady',
  'ReceiptVerified',
  'PublishPending',
  'Active',
  'Observed',
};
const _publications = {'LocalBlocked', 'Pending', 'Observed', 'Conflict'};

const _networkStates = {'Stopped', 'Starting', 'Online', 'Failed'};

/// Notes `<side>_phase`, `<side>_pair`, `<side>_publication` and
/// `<side>_network` of one pairing side from its account snapshot and
/// `network_info` (`unknown` when one could not be read).
Future<void> notePairSide(
  String side,
  Future<Object?> Function() snapshot,
  Future<Object?> Function() network,
) async {
  Object? info;
  try {
    info = await network();
  } catch (_) {
    info = null;
  }
  final state = info is Map ? info['state'] : null;
  failureFacts['${side}_network'] = _networkStates.contains(state)
      ? state as String
      : 'unknown';
  Object? view;
  try {
    view = await snapshot();
  } catch (_) {
    view = null;
  }
  final map = view is Map ? view : const {};
  final phase = map['phase'];
  final pair = map['pair'];
  final publication = map['publication'];
  failureFacts['${side}_phase'] = _phases.contains(phase)
      ? phase as String
      : 'unknown';
  failureFacts['${side}_pair'] = view == null
      ? 'unknown'
      : pair == null
      ? 'none'
      : pair is Map && _pairPhases.contains(pair['phase'])
      ? pair['phase'] as String
      : 'unknown';
  failureFacts['${side}_publication'] =
      publication is Map && _publications.contains(publication['state'])
      ? publication['state'] as String
      : 'unknown';
}

/// The slowest time in ms of each named wait, pass or fail: a scenario
/// deadline's text, an RPC op or an m2 timeout kind (`waits`).
final waitMs = <String, int>{};

/// The most waits a summary keeps.
const waitsLimit = 8;

/// Keeps [ms] if it is the slowest [name] so far.
void noteWait(String name, int ms) {
  if (ms > (waitMs[name] ?? -1)) waitMs[name] = ms;
}

final _scenarioOp = RegExp(r'^(?:account|message|mailbox)_local_[a-z_]{1,48}$');

/// The [waitsLimit] slowest waits whose name is a listed failure text, a listed
/// m2 kind or a scenario op, slowest first; null when none is left.
List<Map<String, Object>>? safeWaits(
  Map<String, int> waits,
  Set<String> details,
  Set<String> kinds,
) {
  final kept =
      [
        for (final MapEntry(:key, :value) in waits.entries)
          if (details.contains(key) ||
              kinds.contains(key) ||
              _scenarioOp.hasMatch(key))
            {'name': key, 'ms': value},
      ]..sort((a, b) => (b['ms'] as int).compareTo(a['ms'] as int));
  return kept.isEmpty ? null : kept.take(waitsLimit).toList();
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

const _states = {
  'Queued',
  'SentDirect',
  'Delivered',
  'Read',
  'Blocked',
  'Failed',
  'Cancelled',
};

/// A message's delivery state as a fixed value: `absent` when the page does
/// not hold it, `other` for anything unknown.
String observedState(Object? item) {
  if (item == null) return 'absent';
  final state = item is Map ? item['state'] : null;
  return _states.contains(state) ? state as String : 'other';
}

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
