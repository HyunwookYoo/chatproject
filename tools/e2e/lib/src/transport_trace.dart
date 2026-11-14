import 'dart:convert';
import 'dart:io';
import 'dart:isolate';

import 'controller.dart';
import 'diagnostic.dart';

/// Each participant's transport trace (core `network_trace`, chat_core
/// transport/trace.rs): per endpoint bind and per connection attempt, fixed
/// kinds, flags, counts and milliseconds only. The checked-in shape is shared
/// with tools/ci/write_e2e_summary.py.
final Map<String, dynamic> _spec = () {
  final source = Isolate.resolvePackageUriSync(
    Uri.parse('package:chat_e2e/src/transport_trace.dart'),
  )!;
  final file = File.fromUri(source.resolve('../../diagnostic_details.json'));
  return (jsonDecode(file.readAsStringSync()) as Map)['transport']
      as Map<String, dynamic>;
}();
final _participants = loadSafeValues('participants');
final _counts = (_spec['dial_counts'] as List).cast<String>().toSet();
final _flags = (_spec['dial_flags'] as List).cast<String>().toSet();
final _kinds = {
  for (final MapEntry(:key, :value)
      in (_spec['dial_kinds'] as Map<String, dynamic>).entries)
    key: (value as List).cast<String>().toSet(),
};
final _required = (_spec['dial_required'] as List).cast<String>().toSet();
final _limit = _spec['dials_limit'] as int;

final Set<String> transportEndpointKeys = (_spec['endpoint'] as List)
    .cast<String>()
    .toSet();
final Set<String> transportDialKeys = {..._counts, ..._flags, ..._kinds.keys};
final _liveCounts = (_spec['live_counts'] as List).cast<String>().toSet();
final _liveRequired = (_spec['live_required'] as List).cast<String>().toSet();
final _liveLimit = _spec['live_limit'] as int;
final Set<String> transportLiveKeys = {..._liveCounts, 'path'};

bool _count(Object? v) => v is int && v >= 0 && v < 1 << 31;

bool _dial(Object? dial) {
  if (dial is! Map ||
      dial.keys.toSet().difference(transportDialKeys).isNotEmpty) {
    return false;
  }
  if (dial.length != transportDialKeys.length) return false;
  for (final key in _required) {
    if (dial[key] == null) return false;
  }
  return _counts.every((k) => dial[k] == null || _count(dial[k])) &&
      _flags.every((k) => dial[k] is bool) &&
      _kinds.entries.every((e) => e.value.contains(dial[e.key]));
}

bool _live(Object? live) =>
    live is Map &&
    live.length == transportLiveKeys.length &&
    live.keys.toSet().containsAll(transportLiveKeys) &&
    _liveRequired.every((k) => live[k] != null) &&
    _liveCounts.every((k) => live[k] == null || _count(live[k])) &&
    _kinds['path']!.contains(live['path']);

/// [value] as a fixed record named [name]; null when any part is not listed.
Map<String, Object?>? safeTransport(String name, Object? value) {
  if (!_participants.contains(name) ||
      value is! Map ||
      value.length != 3 ||
      value['endpoint'] is! Map ||
      value['dials'] is! List ||
      value['live'] is! List) {
    return null;
  }
  final endpoint = value['endpoint'] as Map;
  final dials = value['dials'] as List;
  final live = value['live'] as List;
  if (endpoint.length != transportEndpointKeys.length ||
      !transportEndpointKeys.every((k) => _count(endpoint[k])) ||
      dials.length > _limit ||
      !dials.every(_dial) ||
      live.length > _liveLimit ||
      !live.every(_live)) {
    return null;
  }
  return {
    'name': name,
    'endpoint': {for (final k in transportEndpointKeys) k: endpoint[k]},
    'dials': [
      for (final dial in dials.cast<Map>())
        {for (final k in transportDialKeys) k: dial[k]},
    ],
    'live': [
      for (final connection in live.cast<Map>())
        {for (final k in transportLiveKeys) k: connection[k]},
    ],
  };
}

/// Every answering participant's trace; a participant that does not answer
/// in time or answers with anything unlisted is left out. Null when none.
Future<List<Map<String, Object?>>?> collectTransport(
  Iterable<Participant> participants,
) async {
  final traces = await Future.wait([
    for (final p in participants.toList())
      () async {
        try {
          return safeTransport(
            p.name,
            await p.call(
              'network_trace',
              {},
              timeout: const Duration(seconds: 5),
              failOnTimeout: false,
            ),
          );
        } catch (_) {
          return null;
        }
      }(),
  ]);
  final kept = traces.nonNulls.toList();
  return kept.isEmpty ? null : kept;
}
