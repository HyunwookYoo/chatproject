import 'dart:convert';

/// One notification as the M1b NSE probe recorded it in `nse_memory.jsonl` (App Group):
/// a `start` line, then an `end` line unless iOS stopped the extension first.
class NseRecord {
  const NseRecord({
    required this.seq,
    required this.pid,
    required this.sentAtMs,
    required this.receivedAtMs,
    this.ok,
    this.error,
    this.peakBytes,
    this.loadMicros,
    this.decryptMicros,
  });

  final int seq;
  final int pid;
  final int sentAtMs;
  final int receivedAtMs;

  /// Null when there is no `end` line: the extension stopped before it finished.
  final bool? ok;
  final String? error;
  final int? peakBytes;
  final int? loadMicros;
  final int? decryptMicros;

  bool get stopped => ok == null;

  /// A push without ciphertext: the extension's memory without MLS work.
  bool get baseline => error == 'noCiphertext';

  int? get latencyMs => sentAtMs > 0 && receivedAtMs > 0 ? receivedAtMs - sentAtMs : null;
}

/// Pairs `start` and `end` lines by (pid, seq), in the order the starts appear.
/// Lines that are not JSON objects with integer `pid` and `seq` are skipped.
List<NseRecord> parseNseLog(String text) {
  final starts = <(int, int), Map<String, dynamic>>{};
  final ends = <(int, int), Map<String, dynamic>>{};
  final order = <(int, int)>[];
  for (final line in const LineSplitter().convert(text)) {
    Object? decoded;
    try {
      decoded = jsonDecode(line);
    } on FormatException {
      continue;
    }
    if (decoded is! Map<String, dynamic>) continue;
    final pid = decoded['pid'];
    final seq = decoded['seq'];
    if (pid is! int || seq is! int) continue;
    final key = (pid, seq);
    switch (decoded['phase']) {
      case 'start':
        if (!starts.containsKey(key)) order.add(key);
        starts[key] = decoded;
      case 'end':
        ends[key] = decoded;
    }
  }
  return [for (final key in order) _record(starts[key]!, ends[key])];
}

NseRecord _record(Map<String, dynamic> start, Map<String, dynamic>? end) => NseRecord(
      seq: start['seq'] as int,
      pid: start['pid'] as int,
      sentAtMs: _int(start['sent_at']) ?? -1,
      receivedAtMs: _int(start['received_at']) ?? -1,
      ok: end == null ? null : end['ok'] == true,
      error: end?['error'] is String ? end!['error'] as String : null,
      peakBytes: _positive(end?['peak']),
      loadMicros: _int(end?['load_us']),
      decryptMicros: _int(end?['decrypt_us']),
    );

int? _int(Object? value) => value is int ? value : null;

int? _positive(Object? value) => value is int && value > 0 ? value : null;
