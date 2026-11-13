import 'dart:io';

import 'package:chat_e2e/src/controller.dart';
import 'package:chat_e2e/src/transport_trace.dart';
import 'package:test/test.dart';

import 'controller_test.dart' show MockParticipant;

Map<String, Object?> _dial([Map<String, Object?> patch = const {}]) => {
  'seq': 3,
  'at_ms': 41000,
  'alpn': 'auth',
  'incoming': false,
  'outcome': 'dropped',
  'relay_up': true,
  'target_relays': 1,
  'target_ips': 3,
  'target_own_ips': 1,
  'prior_ip': 1,
  'prior_relay': 0,
  'stall_ip': 1,
  'stall_relay': 0,
  'from': 'none',
  'stall_from': 'none',
  'ip_out': 3600,
  'relay_out': 0,
  'ip_in': 0,
  'relay_in': 0,
  'resolve_ms': 0,
  'handshake_ms': null,
  'total_ms': 8001,
  'path': 'none',
  'paths_ip': 0,
  'paths_relay': 0,
  'rtt_ms': null,
  'tx': null,
  'rx': null,
  'lost': null,
  ...patch,
};

Map<String, Object?> _live([Map<String, Object?> patch = const {}]) => {
  'seq': 3,
  'age_ms': 52000,
  'path': 'ip',
  'paths_ip': 1,
  'paths_relay': 1,
  'rtt_ms': 14,
  'tx': 90,
  'rx': 80,
  'lost': 2,
  ...patch,
};

Map<String, Object?> _trace({
  List<Object?>? dials,
  Object? endpoint,
  List<Object?>? live,
}) => {
  'endpoint':
      endpoint ??
      {'binds': 2, 'bind_ms_max': 40, 'online_ms_max': 900, 'relay_drops': 1},
  'dials': dials ?? [_dial()],
  'live': live ?? [_live()],
};

void main() {
  test('a trace keeps fixed kinds, flags and bounded numbers only', () {
    expect(safeTransport('android-1', _trace()), {
      'name': 'android-1',
      ..._trace(),
    });
    expect(safeTransport('node', _trace(dials: [])), isNotNull);
    for (final bad in <Object?>[
      null,
      'PRIVATE_TOKEN',
      {..._trace(), 'extra': 'PRIVATE_TOKEN'},
      _trace(endpoint: {'binds': 'PRIVATE_TOKEN'}),
      _trace(
        dials: [
          _dial({'alpn': 'PRIVATE_TOKEN'}),
        ],
      ),
      _trace(
        dials: [
          _dial({'outcome': 'PRIVATE_TOKEN'}),
        ],
      ),
      _trace(
        dials: [
          _dial({'path': 'PRIVATE_TOKEN'}),
        ],
      ),
      _trace(
        dials: [
          _dial({'from': 'PRIVATE_TOKEN'}),
        ],
      ),
      _trace(
        dials: [
          _dial({'stall_from': null}),
        ],
      ),
      _trace(
        dials: [
          _dial({'ip_in': 'PRIVATE_TOKEN'}),
        ],
      ),
      _trace(
        dials: [
          _dial({'incoming': 'PRIVATE_TOKEN'}),
        ],
      ),
      _trace(
        dials: [
          _dial({'total_ms': 'PRIVATE_TOKEN'}),
        ],
      ),
      _trace(
        dials: [
          _dial({'total_ms': null}),
        ],
      ),
      _trace(
        dials: [
          _dial({'tx': -1}),
        ],
      ),
      _trace(
        dials: [
          _dial({'target': 'PRIVATE_TOKEN'}),
        ],
      ),
      _trace(dials: List.filled(33, _dial())),
      _trace(live: List.filled(17, _live())),
      _trace(
        live: [
          _live({'path': 'PRIVATE_TOKEN'}),
        ],
      ),
      _trace(
        live: [
          _live({'age_ms': null}),
        ],
      ),
      _trace(
        live: [
          _live({'peer': 'PRIVATE_TOKEN'}),
        ],
      ),
      _trace(
        dials: [
          _dial({'seq': null}),
        ],
      ),
    ]) {
      expect(safeTransport('android-1', bad), isNull, reason: '$bad');
    }
    expect(safeTransport('PRIVATE_TOKEN', _trace()), isNull);
  });

  test('the core emits exactly the listed trace keys', () {
    final source = File(
      '../../core/chat_core/src/transport/trace.rs',
    ).readAsStringSync();
    final emitted = RegExp(
      r'"([a-z_]+)":',
    ).allMatches(source).map((m) => m[1]!).toSet();
    expect(emitted, {
      ...transportEndpointKeys,
      ...transportDialKeys,
      ...transportLiveKeys,
      'endpoint',
      'dials',
      'live',
    });
  });

  test('collects each participant that answers network_trace', () async {
    final logs = await Directory.systemTemp.createTemp('transport-trace-');
    final controller = await Controller.start(port: 0, logDir: logs);
    final node = await MockParticipant.connect(controller);
    final app = await MockParticipant.connect(controller, name: 'android-1');
    try {
      await node.next();
      await app.next();
      await controller.waitParticipant('node');
      await controller.waitParticipant('android-1');
      final collected = collectTransport(controller.participants);
      for (final (peer, value) in [
        (node, _trace()),
        (app, {'dials': 'PRIVATE_TOKEN'}),
      ]) {
        final command = await peer.next();
        expect(command['op'], 'network_trace');
        peer.result(command['id'] as int, value);
      }
      expect(await collected, [
        {'name': 'node', ..._trace()},
      ]);
      await controller.close();
    } finally {
      await node.close();
      await app.close();
      await logs.delete(recursive: true);
    }
  });
}
