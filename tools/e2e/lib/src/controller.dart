import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'protocol.dart';

/// Owns only its loopback listener, sockets and diagnostic log.
class Controller {
  Controller._(this._server, this._log, this._relay) {
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
  }) async {
    if (relay != null && relay != localRelay) {
      throw ArgumentError.value(relay, 'relay');
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
      return Controller._(server, log, relay);
    } catch (_) {
      await server.close(force: true);
      rethrow;
    }
  }

  final HttpServer _server;
  final RandomAccessFile _log;
  final String? _relay;
  late final StreamSubscription<HttpRequest> _listener;
  final _requests = <Future<void>>{};
  final _connections = <_Connection>{};
  final _participants = <String, Participant>{};
  final _waiting = <String, List<Completer<Participant>>>{};
  final _sequences = SequenceGuard();
  Object? _failure;
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
      if (!_closed) _fail(error);
    }
  }

  void _record(_Connection connection, String direction, Object? wire) {
    _log.writeStringSync(
      '${jsonEncode({'connection': connection.id, 'direction': direction, 'wire': wire})}\n',
    );
  }

  void _send(_Connection connection, Map<String, Object?> record) {
    final wire = jsonEncode(record);
    _record(connection, 'sent', wire);
    connection.socket.add(wire);
  }

  void _receive(_Connection connection, Object? raw) {
    try {
      // Obsolete sockets are logged but cannot inspect or mutate current state.
      _record(connection, 'received', raw);
      final participant = connection.participant;
      if (participant != null &&
          !identical(_participants[participant.name], participant)) {
        return;
      }
      if (_failure != null || _closed) return;
      final record = WireRecord.parse(raw);
      if (participant == null) {
        if (record is! HelloRecord) throw StateError('protocol expected hello');
        _send(connection, {'type': 'config', 'relay': _relay});
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
            pending.completeError(record.error!);
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
      _record(connection, 'failure', error.toString());
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
    if (_failure != null || _closed) return;
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
    Object? cleanupError;
    Future<void> clean(Future<void> Function() operation) async {
      try {
        await operation();
      } catch (error) {
        cleanupError ??= error;
      }
    }

    await clean(() async {
      await _server.close(force: true);
    });
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
    if (cleanupError != null) throw cleanupError!;
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
    } finally {
      _pending.remove(id);
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
