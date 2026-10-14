import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:chat_e2e/src/controller.dart';
import 'package:chat_e2e/src/protocol.dart';
import 'package:test/test.dart';

const short = Duration(milliseconds: 80);

Matcher failure(String message) => isA<StateError>().having(
  (error) => error.message,
  'message',
  contains(message),
);

/// A typed peer speaking the complete Rust puppet wire over a real socket.
class MockParticipant {
  MockParticipant(this.socket) : records = StreamIterator(socket);
  final WebSocket socket;
  final StreamIterator<dynamic> records;

  static Future<MockParticipant> connect(
    Controller controller, {
    String name = 'node',
    String instance = 'one',
    String protocol = 'chat-test-hooks:v1',
  }) async {
    final peer = MockParticipant(
      await WebSocket.connect('ws://127.0.0.1:${controller.port}/'),
    );
    peer.send({
      'type': 'hello',
      'protocol': protocol,
      'name': name,
      'instance': instance,
      'relay': 'http://127.0.0.1:3340',
    });
    return peer;
  }

  void send(Map<String, Object?> record) => socket.add(jsonEncode(record));
  Future<Map<String, Object?>> next() async {
    if (!await records.moveNext().timeout(const Duration(seconds: 2))) {
      throw StateError('peer closed before record');
    }
    return (jsonDecode(records.current as String) as Map)
        .cast<String, Object?>();
  }

  void result(int id, Object? value) =>
      send({'type': 'result', 'id': id, 'ok': true, 'value': value});
  void event(int seq, {String instance = 'one', String text = 'hello'}) =>
      send({
        'type': 'event',
        'seq': seq,
        'instance': instance,
        'event': {
          'kind': 'probe_text_received',
          'peer_id': 'fixture-peer',
          'text': text,
        },
      });
  Future<void> close() async {
    await socket.close();
    await records.cancel();
  }
}

void main() {
  late Controller controller;
  late Directory logs;
  final peers = <MockParticipant>[];
  Matcher? expectedCloseFailure;

  Matcher scenarioFailure(String message) {
    final matcher = failure(message);
    expectedCloseFailure = matcher;
    return matcher;
  }

  Future<MockParticipant> join({
    String name = 'node',
    String instance = 'one',
  }) async {
    final peer = await MockParticipant.connect(
      controller,
      name: name,
      instance: instance,
    );
    peers.add(peer);
    expect(await peer.next(), {
      'type': 'config',
      'relay': 'http://127.0.0.1:3340',
    });
    return peer;
  }

  setUp(() async {
    expectedCloseFailure = null;
    logs = await Directory('build/controller-tests').create(recursive: true);
    logs = await logs.createTemp('run-');
    controller = await Controller.start(port: 0, logDir: logs);
  });
  tearDown(() async {
    for (final peer in peers) {
      await peer.close();
    }
    peers.clear();
    if (expectedCloseFailure == null) {
      await controller.close();
    } else {
      await expectLater(controller.close(), throwsA(expectedCloseFailure!));
    }
    await logs.delete(recursive: true);
  });

  test(
    'loopback handshake config precedes registration and RPC ids increase',
    () async {
      final waiting = controller.waitParticipant('node');
      final peer = await join();
      final participant = await waiting;
      expect(participant.name, 'node');
      expect(participant.instance, 'one');
      final first = participant.call('network_info', {});
      expect(await peer.next(), {
        'type': 'cmd',
        'id': 1,
        'op': 'network_info',
        'args': {},
      });
      peer.result(1, {
        'revision': 0,
        'state': 'Stopped',
        'endpoint_id': null,
        'ticket': null,
        'peers': <Object?>[],
      });
      expect(await first, containsPair('state', 'Stopped'));
      final second = participant.call('network_stop', {});
      expect((await peer.next())['id'], 2);
      peer.result(2, null);
      expect(await second, isNull);
      await controller.close();
      final records = await File('${logs.path}/wire.jsonl').readAsLines();
      final wires = records.map((line) => jsonDecode(line) as Map).toList();
      expect(wires.where((record) => record['direction'] == 'sent').length, 3);
      expect(
        wires.where((record) => record['direction'] == 'received').length,
        3,
      );
      expect(wires.first['wire'], contains('chat-test-hooks:v1'));
    },
  );

  test('direct-only config has mandatory null relay', () async {
    await controller.close();
    controller = await Controller.start(port: 0, logDir: logs, relay: null);
    final peer = await MockParticipant.connect(controller);
    peers.add(peer);
    expect(await peer.next(), {'type': 'config', 'relay': null});
    expect((await controller.waitParticipant('node')).instance, 'one');
  });

  test('typed Rust errors fail only their matching call', () async {
    final peer = await join();
    final participant = await controller.waitParticipant('node');
    final call = participant.call('send_probe_text', {
      'peer_id': 'fixture-peer',
      'text': 'x',
    });
    final assertion = expectLater(
      call,
      throwsA(
        isA<RpcError>()
            .having((error) => error.code, 'code', 'PeerNotConnected')
            .having((error) => error.detail, 'detail', 'not connected'),
      ),
    );
    final command = await peer.next();
    peer.send({
      'type': 'result',
      'id': command['id'],
      'ok': false,
      'error': {'code': 'PeerNotConnected', 'detail': 'not connected'},
    });
    await assertion;
    final next = participant.call('network_stop', {});
    peer.result((await peer.next())['id'] as int, null);
    await next;
  });

  test(
    'concurrent RPC results match ids even when arriving in reverse order',
    () async {
      final peer = await join();
      final participant = await controller.waitParticipant('node');
      final first = participant.call('network_info', {});
      final second = participant.call('network_stop', {});
      expect((await peer.next())['id'], 1);
      expect((await peer.next())['id'], 2);
      peer.result(2, null);
      peer.result(1, {
        'revision': 0,
        'state': 'Stopped',
        'endpoint_id': null,
        'ticket': null,
        'peers': <Object?>[],
      });
      expect(await first, containsPair('state', 'Stopped'));
      expect(await second, isNull);
    },
  );

  test(
    'unmatched result id fails outstanding calls and future scenario waits',
    () async {
      final peer = await join();
      final participant = await controller.waitParticipant('node');
      final pending = expectLater(
        participant.call('network_info', {}),
        throwsA(scenarioFailure('protocol')),
      );
      await peer.next();
      peer.result(9, null);
      await pending;
      await expectLater(
        controller.waitParticipant('other'),
        throwsA(scenarioFailure('protocol')),
      );
      await expectLater(
        controller.close(),
        throwsA(scenarioFailure('protocol')),
      );
    },
  );

  test('disconnect fails outstanding commands and event waits once', () async {
    final peer = await join();
    final participant = await controller.waitParticipant('node');
    final call = expectLater(
      participant.call('network_info', {}),
      throwsA(failure('disconnected')),
    );
    final event = expectLater(
      participant.expectEvent((_) => true),
      throwsA(failure('disconnected')),
    );
    await peer.next();
    await peer.socket.close();
    await call;
    await event;
    await expectLater(
      participant.call('network_info', {}),
      throwsA(failure('disconnected')),
    );
  });

  test(
    'command timeout releases pending capacity and ids keep increasing',
    () async {
      final peer = await join();
      final participant = await controller.waitParticipant('node');
      final timeout = expectLater(
        participant.call('network_info', {}, timeout: short),
        throwsA(isA<TimeoutException>()),
      );
      expect((await peer.next())['id'], 1);
      await timeout;
      final calls = <Future<Object?>>[];
      for (var i = 0; i < 64; i++) {
        calls.add(participant.call('network_stop', {}));
      }
      await expectLater(
        participant.call('network_stop', {}),
        throwsA(failure('pending')),
      );
      for (var i = 0; i < 64; i++) {
        final command = await peer.next();
        expect(command['id'], i + 2);
        peer.result(command['id'] as int, null);
      }
      await Future.wait(calls);
    },
  );

  test('late result after timeout is unmatched and visible', () async {
    final peer = await join();
    final participant = await controller.waitParticipant('node');
    final timeout = expectLater(
      participant.call('network_info', {}, timeout: short),
      throwsA(isA<TimeoutException>()),
    );
    await peer.next();
    await timeout;
    final waiter = expectLater(
      participant.expectEvent((_) => true),
      throwsA(scenarioFailure('protocol')),
    );
    peer.result(1, null);
    await waiter;
  });

  for (final seq in [1, 0, 3]) {
    test('sequence $seq after 1 fails the entire scenario', () async {
      final peer = await join();
      final participant = await controller.waitParticipant('node');
      peer.event(1);
      await participant.expectEvent((_) => true);
      final wait = expectLater(
        participant.expectEvent((_) => true),
        throwsA(scenarioFailure('sequence')),
      );
      peer.event(seq);
      await wait;
      await expectLater(
        participant.call('network_info', {}),
        throwsA(scenarioFailure('sequence')),
      );
      await expectLater(
        controller.waitParticipant('node'),
        throwsA(scenarioFailure('sequence')),
      );
      await expectLater(
        controller.close(),
        throwsA(scenarioFailure('sequence')),
      );
    });
  }

  test(
    'buffered event is consumed exactly once; timed out waiter is removed',
    () async {
      final peer = await join();
      final participant = await controller.waitParticipant('node');
      peer.event(1, text: 'old');
      peer.event(2, text: 'new');
      expect(
        (await participant.expectEvent(
          (event) => event['text'] == 'new',
        ))['text'],
        'new',
      );
      expect(
        (await participant.expectEvent(
          (event) => event['text'] == 'old',
        ))['text'],
        'old',
      );
      await expectLater(
        participant.expectEvent(
          (event) => event['text'] == 'old',
          timeout: short,
        ),
        throwsA(isA<TimeoutException>()),
      );
      final next = participant.expectEvent((event) => event['text'] == 'old');
      peer.event(3, text: 'old');
      expect((await next)['text'], 'old');
    },
  );

  test(
    'flattened Rust state events reach predicates without FRB nesting',
    () async {
      final peer = await join();
      final participant = await controller.waitParticipant('node');
      peer.send({
        'type': 'event',
        'seq': 1,
        'instance': 'one',
        'event': {
          'kind': 'network_changed',
          'revision': 2,
          'state': 'Online',
          'endpoint_id': null,
          'ticket': null,
          'peers': <Object?>[],
        },
      });
      final event = await participant.expectEvent(
        (event) => event['state'] == 'Online',
      );
      expect(event['revision'], 2);
      expect(event.containsKey('info'), isFalse);
    },
  );

  test('every CoreEvent serde variant preserves producer fields', () async {
    final peer = await join();
    final participant = await controller.waitParticipant('node');
    final events = <Map<String, Object?>>[
      {
        'kind': 'connection_state',
        'direct_peers': 1,
        'mailbox_ok': false,
        'queue_ok': null,
      },
      {'kind': 'error', 'code': 'TransportFailed', 'detail': 'fixture failure'},
      {
        'kind': 'peer_changed',
        'revision': 4,
        'peer': {
          'peer_id': 'fixture-peer',
          'phase': 'Connected',
          'path': 'Direct',
          'connect_ms': 2,
          'direct_ms': 3,
        },
      },
    ];
    for (var i = 0; i < events.length; i++) {
      peer.send({
        'type': 'event',
        'seq': i + 1,
        'instance': 'one',
        'event': events[i],
      });
      expect(await participant.expectEvent((_) => true), events[i]);
    }
  });

  test(
    '257th unconsumed event fails instead of silently dropping text',
    () async {
      final peer = await join();
      final participant = await controller.waitParticipant('node');
      final pending = expectLater(
        participant.call('network_info', {}),
        throwsA(scenarioFailure('overflow')),
      );
      await peer.next();
      for (var i = 1; i <= 257; i++) {
        peer.event(i);
      }
      await pending;
      await expectLater(
        participant.expectEvent((_) => true),
        throwsA(scenarioFailure('overflow')),
      );
    },
  );

  test(
    'new instance starts at one and returning old instance retains sequence',
    () async {
      final first = await join();
      final old = await controller.waitParticipant('node');
      first.event(1);
      await old.expectEvent((_) => true);
      final second = await join(instance: 'two');
      final replacement = await controller.waitParticipant('node');
      expect(replacement.instance, 'two');
      second.event(1, instance: 'two');
      await replacement.expectEvent((_) => true);
      final third = await join();
      final returning = await controller.waitParticipant('node');
      third.event(2);
      await returning.expectEvent((_) => true);
      final call = returning.call('network_stop', {});
      expect((await third.next())['id'], 1);
      third.result(1, null);
      await call;
    },
  );

  test('same instance reconnect rejects a reset sequence', () async {
    final first = await join();
    final participant = await controller.waitParticipant('node');
    first.event(1);
    await participant.expectEvent((_) => true);
    await first.socket.close();
    final second = await join();
    final replacement = await controller.waitParticipant('node');
    final wait = expectLater(
      replacement.expectEvent((_) => true),
      throwsA(scenarioFailure('sequence')),
    );
    second.event(1);
    await wait;
  });

  test(
    'late previous socket records and close cannot process or detach replacement',
    () async {
      final first = await join();
      final old = await controller.waitParticipant('node');
      final pending = expectLater(
        old.call('network_info', {}),
        throwsA(failure('disconnected')),
      );
      await first.next();
      final second = await join(instance: 'two');
      final replacement = await controller.waitParticipant('node');
      first.event(99);
      first.result(99, null);
      await first.socket.close();
      await pending;
      second.event(1, instance: 'two');
      await replacement.expectEvent((_) => true);
      expect(await controller.waitParticipant('node'), same(replacement));
      final call = replacement.call('network_stop', {});
      second.result((await second.next())['id'] as int, null);
      await call;
    },
  );

  test('event instance must match its hello', () async {
    final peer = await join();
    final participant = await controller.waitParticipant('node');
    final wait = expectLater(
      participant.expectEvent((_) => true),
      throwsA(scenarioFailure('protocol')),
    );
    peer.event(1, instance: 'other');
    await wait;
  });

  test('wrong hello protocol never registers a participant', () async {
    final peer = await MockParticipant.connect(
      controller,
      protocol: 'chat-test-hooks:v2',
    );
    peers.add(peer);
    final wait = expectLater(
      controller.waitParticipant('node'),
      throwsA(scenarioFailure('protocol')),
    );
    await wait;
  });

  for (final name in ['', 'x' * 129, 'é' * 65]) {
    test(
      'rejects empty or UTF8 oversized hello name (${name.length})',
      () async {
        final wait = expectLater(
          controller.waitParticipant('node'),
          throwsA(scenarioFailure('protocol')),
        );
        final peer = await MockParticipant.connect(controller, name: name);
        peers.add(peer);
        await wait;
      },
    );
  }

  test('rejects oversized instance', () async {
    final wait = expectLater(
      controller.waitParticipant('node'),
      throwsA(scenarioFailure('protocol')),
    );
    final peer = await MockParticipant.connect(controller, instance: 'x' * 129);
    peers.add(peer);
    await wait;
  });

  test('rejects empty instance', () async {
    final wait = expectLater(
      controller.waitParticipant('node'),
      throwsA(scenarioFailure('protocol')),
    );
    peers.add(await MockParticipant.connect(controller, instance: ''));
    await wait;
  });

  for (final raw in <Object>[
    '{broken',
    <int>[1, 2],
    '{"type":"unknown"}',
  ]) {
    test('malformed wire $raw fails active calls and scenario', () async {
      final peer = await join();
      final participant = await controller.waitParticipant('node');
      final pending = expectLater(
        participant.call('network_info', {}),
        throwsA(scenarioFailure('protocol')),
      );
      await peer.next();
      peer.socket.add(raw);
      await pending;
      await expectLater(
        controller.close(),
        throwsA(scenarioFailure('protocol')),
      );
    });
  }

  test('unknown event kind fails rather than reaching predicates', () async {
    final peer = await join();
    final participant = await controller.waitParticipant('node');
    final event = expectLater(
      participant.expectEvent((_) => true),
      throwsA(scenarioFailure('protocol')),
    );
    peer.send({
      'type': 'event',
      'seq': 1,
      'instance': 'one',
      'event': {'kind': 'unknown'},
    });
    await event;
  });

  test('predicate exceptions stay visible to active waits and close', () async {
    final peer = await join();
    final participant = await controller.waitParticipant('node');
    final error = StateError('fixture predicate failed');
    expectedCloseFailure = same(error);
    final event = expectLater(
      participant.expectEvent((_) => throw error),
      throwsA(same(error)),
    );
    peer.event(1);
    await event;
    await expectLater(controller.close(), throwsA(same(error)));
  });

  test(
    'SequenceGuard rejects gaps before accepting text and remembers instances',
    () {
      final guard = SequenceGuard();
      guard.accept('one', 1);
      expect(() => guard.accept('one', 3), throwsStateError);
      guard.accept('two', 1);
      guard.accept('one', 2);
      expect(() => guard.accept('one', 2), throwsStateError);
    },
  );

  test(
    'fatal failure fails other participants and preexisting name waits',
    () async {
      final peer = await join();
      await join(name: 'other', instance: 'two');
      final first = await controller.waitParticipant('node');
      final other = await controller.waitParticipant('other');
      final missing = expectLater(
        controller.waitParticipant('missing'),
        throwsA(scenarioFailure('sequence')),
      );
      final otherEvent = expectLater(
        other.expectEvent((_) => true),
        throwsA(scenarioFailure('sequence')),
      );
      final firstEvent = expectLater(
        first.expectEvent((_) => true),
        throwsA(scenarioFailure('sequence')),
      );
      peer.event(2);
      await Future.wait([missing, otherEvent, firstEvent]);
    },
  );

  test('new instance cannot start above one', () async {
    await join();
    final peer = await join(instance: 'two');
    final participant = await controller.waitParticipant('node');
    final wait = expectLater(
      participant.expectEvent((_) => true),
      throwsA(scenarioFailure('sequence')),
    );
    peer.event(2, instance: 'two');
    await wait;
  });

  test(
    'rejects non-root websocket path and preserves server for root',
    () async {
      await expectLater(
        WebSocket.connect('ws://127.0.0.1:${controller.port}/other'),
        throwsA(isA<WebSocketException>()),
      );
      await join();
      expect((await controller.waitParticipant('node')).name, 'node');
    },
  );

  test('occupied port fails without killing holder', () async {
    final holder = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    holder.listen((request) {
      request.response.write('still alive');
      request.response.close();
    });
    try {
      await expectLater(
        Controller.start(port: holder.port, logDir: logs),
        throwsA(isA<SocketException>()),
      );
      final client = HttpClient();
      try {
        final response = await (await client.getUrl(
          Uri.parse('http://127.0.0.1:${holder.port}/'),
        )).close();
        expect(await response.transform(utf8.decoder).join(), 'still alive');
      } finally {
        client.close(force: true);
      }
    } finally {
      await holder.close(force: true);
    }
  });

  test('log startup failure releases the newly owned listener', () async {
    final holder = await HttpServer.bind(InternetAddress.loopbackIPv4, 0);
    final port = holder.port;
    await holder.close(force: true);
    final badLogs = await Directory('${logs.path}/bad').create();
    await Directory('${badLogs.path}/wire.jsonl').create();
    await expectLater(
      Controller.start(port: port, logDir: badLogs),
      throwsA(isA<FileSystemException>()),
    );
    final reclaimed = await HttpServer.bind(InternetAddress.loopbackIPv4, port);
    await reclaimed.close(force: true);
  });

  test('close fails pending calls and events and releases its port', () async {
    final peer = await join();
    final participant = await controller.waitParticipant('node');
    final pending = expectLater(
      participant.call('network_info', {}),
      throwsA(failure('closed')),
    );
    final event = expectLater(
      participant.expectEvent((_) => true),
      throwsA(failure('closed')),
    );
    await peer.next();
    final port = controller.port;
    await controller.close();
    await pending;
    await event;
    final reclaimed = await HttpServer.bind(InternetAddress.loopbackIPv4, port);
    await reclaimed.close(force: true);
  });

  test(
    'participant wait timeout removes waiter and close is idempotent',
    () async {
      await expectLater(
        controller.waitParticipant('node', timeout: short),
        throwsA(isA<TimeoutException>()),
      );
      await join();
      await controller.waitParticipant('node');
      final waiting = expectLater(
        controller.waitParticipant('missing'),
        throwsA(failure('closed')),
      );
      final close = controller.close();
      expect(controller.close(), same(close));
      await close;
      await waiting;
      await expectLater(
        controller.waitParticipant('node'),
        throwsA(failure('closed')),
      );
    },
  );
}
