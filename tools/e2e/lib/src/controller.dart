import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'protocol.dart';

/// Owns only its loopback listener, sockets and diagnostic log.
class Controller {
  Controller._(
    this._server,
    this._log,
    this._relay, [
    this._accountRecords = false,
    this._controlledTime = false,
  ]) {
    _listener = _server.listen((request) {
      final handling = _accept(request);
      _requests.add(handling);
      unawaited(handling.whenComplete(() => _requests.remove(handling)));
    }, onError: (Object error) => _fail(error));
  }

  static Future<Controller> start({
    int port = 47000,
    required Directory logDir,
    String? relay = localRelay,
    bool accountRecords = false,
    bool controlledTime = false,
  }) async {
    if (relay != null && relay != localRelay) {
      throw ArgumentError.value(relay, 'relay');
    }
    if (controlledTime && !accountRecords) {
      throw ArgumentError("Controlled time requires local records");
    }
    final server = await HttpServer.bind(
      InternetAddress.loopbackIPv4,
      port,
      shared: false,
    );
    try {
      await logDir.create(recursive: true);
      final log = File(
        '${logDir.path}/wire.jsonl',
      ).openSync(mode: FileMode.append);
      return Controller._(server, log, relay, accountRecords, controlledTime);
    } catch (_) {
      await server.close(force: true);
      rethrow;
    }
  }

  final HttpServer _server;
  final RandomAccessFile _log;
  final String? _relay;
  final bool _accountRecords;
  final bool _controlledTime;
  final _records = <String, List<int>>{};
  final _recordSockets = <WebSocket>{};
  bool recordPublicationsPaused = false;
  bool recordLookupsPaused = false;
  late final StreamSubscription<HttpRequest> _listener;
  final _requests = <Future<void>>{};
  final _connections = <_Connection>{};
  final _participants = <String, Participant>{};
  final _waiting = <String, List<Completer<Participant>>>{};
  final _sequences = SequenceGuard();
  Object? _failure;
  Object? _cleanupFailure;
  bool _closed = false;
  Future<void>? _closing;
  int _connectionId = 0;

  int get port => _server.port;

  Future<Participant> waitParticipant(
    String name, {
    Duration timeout = const Duration(seconds: 10),
  }) async {
    _check();
    final participant = _participants[name];
    if (participant != null) return participant;
    final completer = Completer<Participant>();
    final waiters = _waiting.putIfAbsent(name, () => []);
    waiters.add(completer);
    try {
      return await completer.future.timeout(timeout);
    } finally {
      waiters.remove(completer);
      if (waiters.isEmpty) _waiting.remove(name);
    }
  }

  void _check() {
    if (_failure != null) throw _failure!;
    if (_closed) throw StateError('controller closed');
  }

  Future<void> _accept(HttpRequest request) async {
    try {
      if (!_closed &&
          _accountRecords &&
          request.uri.path == '/records' &&
          !request.uri.hasQuery &&
          WebSocketTransformer.isUpgradeRequest(request)) {
        await _serveRecord(request);
        return;
      }
      if (_closed ||
          request.uri.path != '/' ||
          request.uri.hasQuery ||
          !WebSocketTransformer.isUpgradeRequest(request)) {
        request.response.statusCode = HttpStatus.notFound;
        await request.response.close();
        return;
      }
      final socket = await WebSocketTransformer.upgrade(request);
      if (_closed) {
        await socket.close();
        return;
      }
      final connection = _Connection(socket, ++_connectionId);
      _connections.add(connection);
      connection.helloTimeout = Timer(const Duration(seconds: 10), () {
        _fail(StateError('protocol hello timeout'));
      });
      connection.subscription = socket.listen(
        (raw) => _receive(connection, raw),
        onError: (Object error) => _detach(connection, error),
        onDone: () =>
            _detach(connection, StateError('participant disconnected')),
      );
    } catch (error) {
      _fail(error);
    }
  }

  // One bounded request per socket; bytes are public signed DNS packets only.
  // No raw fixture requests/responses enter wire or process logs.
  Future<void> _serveRecord(HttpRequest request) async {
    final socket = await WebSocketTransformer.upgrade(request);
    _recordSockets.add(socket);
    try {
      if (_recordSockets.length > 64) return;
      final raw = await socket.first.timeout(const Duration(seconds: 5));
      if (raw is! String || raw.length > 8192) return;
      final value = jsonDecode(raw);
      if (value is! Map) return;
      List<int>? bytes(Object? v, int min, int max) =>
          v is List &&
              v.length >= min &&
              v.length <= max &&
              v.every((b) => b is int && b >= 0 && b <= 255)
          ? v.cast<int>()
          : null;
      Map<String, Object?> reply = {'unavailable': true};
      if (value['op'] == 'lookup' &&
          value.length == 2 &&
          !recordLookupsPaused) {
        final peer = bytes(value['peer'], 32, 32);
        if (peer != null) {
          final packet = _records[peer.join(',')];
          reply = packet == null ? {'absent': true} : {'packet': packet};
        }
      } else if (value['op'] == 'publish' &&
          value.length == 2 &&
          !recordPublicationsPaused) {
        final packet = bytes(value['packet'], 104, 1000);
        if (packet != null) {
          final key = packet.take(32).join(',');
          final old = _records[key];
          int seq(List<int> p) =>
              p.sublist(96, 104).fold(0, (a, b) => (a << 8) | b);
          if (old != null &&
              (seq(old) > seq(packet) ||
                  (seq(old) == seq(packet) &&
                      old.join(',') != packet.join(',')))) {
            reply = {'conflict': true};
          } else if (_records.length < 64 || old != null) {
            _records[key] = packet;
            reply = {'ok': true};
          }
        }
      }
      socket.add(jsonEncode(reply));
    } catch (_) {
      // Malformed fixture input is unavailable, never a raw diagnostic.
    } finally {
      await socket.close();
      _recordSockets.remove(socket);
    }
  }

  void _record(_Connection connection, String direction, Object? wire) {
    _log.writeStringSync(
      '${jsonEncode({'connection': connection.id, 'direction': direction, 'wire': _safeWire(connection, direction, wire)})}\n',
    );
  }

  // Parse/classify before any file write. Unknown and malformed frames never
  // reach diagnostics, including late results whose pending context is gone.
  Object _safeWire(_Connection connection, String direction, Object? wire) {
    if (wire is! String) return '[redacted]';
    try {
      final value = jsonDecode(wire);
      if (value is! Map<String, dynamic>) return '[redacted]';
      if (direction == 'sent') {
        if (value['type'] == 'config') {
          return jsonEncode({
            'type': 'config',
            'relay': _relay,
            if (_accountRecords) 'records': 'ws://127.0.0.1:47000/records',
            if (_controlledTime) 'controlled_time': true,
          });
        }
        if (value case {
          'type': 'cmd',
          'id': final int id,
          'op': final String op,
        }) {
          return jsonEncode({
            'type': 'cmd',
            'id': id,
            'op': op,
            'args': _publicCommandArgs(op, value['args']) ?? '[redacted]',
          });
        }
        return '[redacted]';
      }
      if (direction != 'received') return '[redacted]';
      WireRecord.parse(
        wire,
      ); // Inbound cmd/config and unknown top fields fail closed.
      final safe = Map<String, dynamic>.of(value);
      switch (safe['type']) {
        case 'result':
          final op = connection.participant?._operations[safe['id']];
          if (op == null ||
              op.startsWith('account_local_') ||
              op.startsWith('message_local_')) {
            return jsonEncode({
              'type': 'result',
              'id': safe['id'],
              'payload': '[redacted]',
              if (safe['ok'] == false)
                'error_code': (safe['error'] as Map)['code'],
            });
          }
          if (safe.containsKey('error')) {
            safe['error'] = '[redacted]';
          } else {
            safe['value'] = switch (op) {
              'network_start' ||
              'network_info' ||
              'network_stop' ||
              'set_offline' => _publicNetwork(safe['value']) ?? '[redacted]',
              'connect_peer' => _publicPeer(safe['value']) ?? '[redacted]',
              'disconnect_peer' ||
              'send_probe_text' when safe['value'] == null => null,
              _ => '[redacted]',
            };
          }
        case 'event':
          safe['event'] = _publicEvent(safe['event']) ?? '[redacted]';
        case 'hello':
          break;
        default:
          return '[redacted]';
      }
      return jsonEncode(safe);
    } catch (_) {
      return '[redacted]';
    }
  }

  // These projections are only for diagnostics. Never return an input map or
  // list: even known events/results can contain unknown nested secret fields.
  Map<String, Object?>? _publicPeer(Object? value) {
    if (value
        case {
          'peer_id': final String id,
          'phase': final String phase,
          'path': final String path,
          'connect_ms': final int? connect,
          'direct_ms': final int? direct,
        }
        when const [
              'Connecting',
              'Connected',
              'Reconnecting',
              'Disconnected',
            ].contains(phase) &&
            const ['Unknown', 'Relay', 'Direct'].contains(path) &&
            (connect == null || connect >= 0) &&
            (direct == null || direct >= 0)) {
      return {
        'peer_id': id,
        'phase': phase,
        'path': path,
        'connect_ms': connect,
        'direct_ms': direct,
      };
    }
    return null;
  }

  Map<String, Object?>? _publicNetwork(Object? value) {
    if (value
        case {
          'revision': final int revision,
          'state': final String state,
          'endpoint_id': final String? endpoint,
          'ticket': final String? ticket,
          'peers': final List peers,
        }
        when revision >= 0 &&
            const ['Stopped', 'Starting', 'Online', 'Failed'].contains(state)) {
      final projected = <Map<String, Object?>>[];
      for (final peer in peers) {
        final public = _publicPeer(peer);
        if (public == null) return null;
        projected.add(public);
      }
      return {
        'revision': revision,
        'state': state,
        'endpoint_id': endpoint,
        'ticket': ticket,
        'peers': projected,
      };
    }
    return null;
  }

  Map<String, Object?>? _publicEvent(Object? value) {
    switch (value) {
      case {'kind': 'network_changed'}:
        final network = _publicNetwork(value);
        return network == null ? null : {'kind': 'network_changed', ...network};
      case {
            'kind': 'peer_changed',
            'revision': final int revision,
            'peer': final Object peer,
          }
          when revision >= 0:
        final public = _publicPeer(peer);
        return public == null
            ? null
            : {'kind': 'peer_changed', 'revision': revision, 'peer': public};
      case {
        'kind': 'probe_text_received',
        'peer_id': final String id,
        'text': final String text,
      }:
        return {'kind': 'probe_text_received', 'peer_id': id, 'text': text};
      case {
            'kind': 'connection_state',
            'direct_peers': final int peers,
            'mailbox_ok': final bool mailbox,
            'queue_ok': final bool? queue,
          }
          when peers >= 0:
        return {
          'kind': 'connection_state',
          'direct_peers': peers,
          'mailbox_ok': mailbox,
          'queue_ok': queue,
        };
      default:
        return null;
    }
  }

  Map<String, Object?>? _publicCommandArgs(String op, Object? value) =>
      switch ((op, value)) {
        ('network_start' || 'network_info' || 'network_stop', Map()) => {},
        ('connect_peer', {'target': final String target}) => {'target': target},
        ('disconnect_peer', {'peer_id': final String id}) => {'peer_id': id},
        (
          'send_probe_text',
          {'peer_id': final String id, 'text': final String text},
        ) =>
          {'peer_id': id, 'text': text},
        ('set_offline', {'offline': final bool offline}) => {
          'offline': offline,
        },
        _ => null,
      };

  void _send(_Connection connection, Map<String, Object?> record) {
    final wire = jsonEncode(record);
    _record(connection, 'sent', wire);
    connection.socket.add(wire);
  }

  void _receive(_Connection connection, Object? raw) {
    if (_closed) return;
    try {
      // Obsolete sockets are logged but cannot inspect or mutate current state.
      _record(connection, 'received', raw);
      final participant = connection.participant;
      if (participant != null &&
          !identical(_participants[participant.name], participant)) {
        return;
      }
      if (_failure != null) return;
      final record = WireRecord.parse(raw);
      if (participant == null) {
        if (record is! HelloRecord) throw StateError('protocol expected hello');
        _send(connection, {
          'type': 'config',
          'relay': _relay,
          if (_accountRecords) 'records': 'ws://127.0.0.1:47000/records',
          if (_controlledTime) 'controlled_time': true,
        });
        connection.helloTimeout?.cancel();
        final next = Participant._(
          this,
          connection,
          record.name,
          record.instance,
        );
        final previous = _participants[record.name];
        previous?._end(StateError('participant disconnected: replaced'));
        connection.participant = next;
        _participants[record.name] = next;
        for (final waiter
            in _waiting.remove(record.name) ?? <Completer<Participant>>[]) {
          waiter.complete(next);
        }
        return;
      }
      switch (record) {
        case ResultRecord():
          final pending = participant._pending[record.id];
          if (pending == null || pending.isCompleted) {
            throw StateError('protocol unmatched result id');
          }
          if (record.error != null) {
            pending.completeError(
              const ['account_local_', 'message_local_'].any(
                    (prefix) =>
                        participant._operations[record.id]?.startsWith(
                          prefix,
                        ) ==
                        true,
                  )
                  ? RpcError(record.error!.code, '[redacted]')
                  : record.error!,
            );
          } else {
            pending.complete(record.value);
          }
        case EventRecord():
          if (record.instance != participant.instance) {
            throw StateError('protocol event instance mismatch');
          }
          _sequences.accept(record.instance, record.seq);
          participant._event(record.event);
        case HelloRecord():
          throw StateError('protocol repeated hello');
      }
    } catch (error) {
      _fail(
        error is FormatException ? StateError('invalid protocol JSON') : error,
      );
    }
  }

  void _detach(_Connection connection, Object error) {
    connection.helloTimeout?.cancel();
    try {
      _record(connection, 'failure', null);
    } catch (logError) {
      _fail(logError);
    }
    final participant = connection.participant;
    if (participant != null &&
        identical(_participants[participant.name], participant)) {
      _participants.remove(participant.name);
      participant._end(StateError('participant disconnected'));
    }
  }

  void _fail(Object error) {
    if (_closed) {
      _cleanupFailure ??= error;
      return;
    }
    if (_failure != null) return;
    _failure = error;
    for (final participant in _participants.values) {
      participant._end(error);
    }
    for (final waiters in _waiting.values) {
      for (final waiter in waiters) {
        if (!waiter.isCompleted) waiter.completeError(error);
      }
    }
    _waiting.clear();
  }

  /// Fatal scenario errors remain visible even if the last assertion finished.
  Future<void> close() => _closing ??= _close();

  Future<void> _close() async {
    _closed = true;
    final closed = _failure ?? StateError('controller closed');
    for (final participant in _participants.values) {
      participant._end(closed);
    }
    for (final waiters in _waiting.values) {
      for (final waiter in waiters) {
        if (!waiter.isCompleted) waiter.completeError(closed);
      }
    }
    _waiting.clear();
    Future<void> clean(Future<void> Function() operation) async {
      try {
        await operation();
      } catch (error) {
        _cleanupFailure ??= error;
      }
    }

    await clean(() async {
      await _server.close(force: true);
    });
    for (final socket in _recordSockets.toList()) {
      await clean(() => socket.close());
    }
    await clean(() async {
      await Future.wait(_requests.toList());
    });
    for (final connection in _connections) {
      connection.helloTimeout?.cancel();
      await clean(() async {
        await connection.socket.close();
      });
      await clean(() => connection.subscription!.cancel());
    }
    await clean(() => _listener.cancel());
    await clean(() async {
      _log.closeSync();
    });
    if (_failure != null) throw _failure!;
    if (_cleanupFailure != null) throw _cleanupFailure!;
  }
}

class _Connection {
  _Connection(this.socket, this.id);
  final WebSocket socket;
  final int id;
  Participant? participant;
  StreamSubscription<dynamic>? subscription;
  Timer? helloTimeout;
}

class Participant {
  Participant._(this._controller, this._connection, this.name, this.instance);
  final Controller _controller;
  final _Connection _connection;
  final String name;
  final String instance;
  final _pending = <int, Completer<Object?>>{};
  final _operations = <int, String>{};
  final _events = <Map<String, Object?>>[];
  final _eventWaiters = <_EventWaiter>[];
  int _id = 0;
  Object? _error;

  void _check() {
    _controller._check();
    if (_error != null) throw _error!;
  }

  Future<Object?> call(
    String op,
    Map<String, Object?> args, {
    Duration timeout = const Duration(seconds: 15),
  }) async {
    _check();
    if (_pending.length >= 64) {
      throw StateError('participant pending command limit');
    }
    if (_id >= 0x1fffffffffffff) {
      throw StateError('protocol command id exhausted');
    }
    final id = ++_id;
    final completer = Completer<Object?>();
    _pending[id] = completer;
    _operations[id] = op;
    // Attach the timeout/error handler before any write can fail the scenario.
    final result = completer.future.timeout(timeout);
    try {
      try {
        _controller._send(_connection, {
          'type': 'cmd',
          'id': id,
          'op': op,
          'args': args,
        });
      } catch (error) {
        _controller._fail(error);
      }
      return await result;
    } on TimeoutException catch (error) {
      _controller._fail(error);
      rethrow;
    } finally {
      _pending.remove(id);
      _operations.remove(id);
    }
  }

  Future<Map<String, Object?>> expectEvent(
    bool Function(Map<String, Object?>) predicate, {
    Duration timeout = const Duration(seconds: 15),
  }) async {
    _check();
    final index = _events.indexWhere(predicate);
    if (index >= 0) return _events.removeAt(index);
    final waiter = _EventWaiter(predicate);
    _eventWaiters.add(waiter);
    try {
      return await waiter.completer.future.timeout(timeout);
    } finally {
      _eventWaiters.remove(waiter);
    }
  }

  void _event(Map<String, Object?> event) {
    for (final waiter in _eventWaiters.toList()) {
      if (waiter.predicate(event)) {
        _eventWaiters.remove(waiter);
        waiter.completer.complete(event);
        return;
      }
    }
    if (_events.length >= 256) {
      throw StateError('participant event buffer overflow');
    }
    _events.add(event);
  }

  void _end(Object error) {
    if (_error != null) return;
    _error = error;
    for (final pending in _pending.values) {
      if (!pending.isCompleted) pending.completeError(error);
    }
    for (final waiter in _eventWaiters) {
      if (!waiter.completer.isCompleted) waiter.completer.completeError(error);
    }
    _eventWaiters.clear();
    _events.clear();
  }
}

class _EventWaiter {
  _EventWaiter(this.predicate);
  final bool Function(Map<String, Object?>) predicate;
  final completer = Completer<Map<String, Object?>>();
}
