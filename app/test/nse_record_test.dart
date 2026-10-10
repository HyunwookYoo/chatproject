import 'package:chat_app/probe/nse_record.dart';
import 'package:flutter_test/flutter_test.dart';

const _okPair = '''
{"footprint":3000000,"phase":"start","pid":7,"received_at":1000500,"sent_at":1000000,"seq":1}
{"available":0,"decrypt_us":1700,"footprint":5000000,"load_us":3700,"ok":true,"peak":6291456,"phase":"end","pid":7,"plaintext_len":1024,"seq":1}
''';

void main() {
  test('pairs start and end lines by pid and seq', () {
    final records = parseNseLog(_okPair);
    expect(records, hasLength(1));
    final r = records.single;
    expect(r.seq, 1);
    expect(r.ok, isTrue);
    expect(r.peakBytes, 6291456);
    expect(r.decryptMicros, 1700);
    expect(r.latencyMs, 500);
    expect(r.stopped, isFalse);
  });

  test('start_without_end_is_reported_as_stopped', () {
    final records = parseNseLog('{"phase":"start","pid":9,"seq":4,"sent_at":1,"received_at":2,"footprint":1}\n');
    expect(records.single.stopped, isTrue);
    expect(records.single.ok, isNull);
  });

  test('garbage_lines_are_skipped', () {
    final records = parseNseLog('not json\n[1,2]\n{"phase":"start"}\n{"phase":"end","pid":"x","seq":1}\n$_okPair{"phase":"st');
    expect(records, hasLength(1));
    expect(records.single.seq, 1);
  });

  test('a push without ciphertext is a baseline record', () {
    const log = '{"phase":"start","pid":3,"seq":2,"sent_at":10,"received_at":20,"footprint":1}\n'
        '{"phase":"end","pid":3,"seq":2,"ok":false,"error":"noCiphertext","footprint":2,"peak":4194304,"available":0}\n';
    final r = parseNseLog(log).single;
    expect(r.baseline, isTrue);
    expect(r.ok, isFalse);
  });

  test('an unknown peak (-1) and a missing sent_at give nulls', () {
    const log = '{"phase":"start","pid":3,"seq":5,"sent_at":-1,"received_at":20,"footprint":1}\n'
        '{"phase":"end","pid":3,"seq":5,"ok":true,"footprint":2,"peak":-1,"available":0}\n';
    final r = parseNseLog(log).single;
    expect(r.peakBytes, isNull);
    expect(r.latencyMs, isNull);
  });
}
