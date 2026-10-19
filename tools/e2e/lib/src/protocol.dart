import 'dart:convert';

const puppetProtocol = 'chat-test-hooks:v1';
const localRelay = 'http://127.0.0.1:3340';

/// A core command failure, distinct from controller/protocol failures.
class RpcError implements Exception {
  const RpcError(this.code, this.detail);
  final String code;
  final String detail;
  @override
  String toString() => 'RPC failed: $code';
}

const _errorCodes = {
  'InvalidConfig',
  'AlreadyStartedDifferentConfig',
  'CoreNotStarted',
  'NetworkNotStarted',
  'InvalidPeerAddress',
  'SelfConnection',
  'PeerNotConnected',
  'MessageTooLarge',
  'Busy',
  'NetworkTimeout',
  'TransportFailed',
  'NotAvailable',
  'InvalidRecovery',
  'StorageLocked',
  'StorageMissing',
  'StorageCorrupt',
  'StorageFull',
  'PairHistoryFull',
  'AccountExists',
  'WriterBusy',
  'InvalidInvite',
  'InviteExpired',
  'DeviceLimit',
  'PairMismatch',
  'PairExpired',
  'RecordConflict',
  'RecordUnavailable',
  'AuthFailed',
  'AccountRestricted',
};

Map<String, Object?> _wireObject(Object? value) {
  if (value is! Map<String, dynamic>) {
    throw StateError('invalid protocol object');
  }
  return value.cast<String, Object?>();
}

void _fields(Map<String, Object?> record, Set<String> fields) {
  if (record.length != fields.length || !fields.every(record.containsKey)) {
    throw StateError('invalid protocol fields');
  }
}

String _identity(Object? value) {
  if (value is! String || value.isEmpty || utf8.encode(value).length > 128) {
    throw StateError('invalid protocol identity');
  }
  return value;
}

sealed class WireRecord {
  static WireRecord parse(Object? raw) {
    if (raw is! String || utf8.encode(raw).length > 64 * 1024) {
      throw StateError('invalid protocol text');
    }
    final record = _wireObject(jsonDecode(raw));
    switch (record['type']) {
      case 'hello':
        _fields(record, {'type', 'protocol', 'name', 'instance', 'relay'});
        if (record['protocol'] != puppetProtocol ||
            record['relay'] != localRelay) {
          throw StateError('invalid protocol hello');
        }
        return HelloRecord(
          _identity(record['name']),
          _identity(record['instance']),
        );
      case 'result':
        final id = record['id'];
        if (id is! int || id <= 0) {
          throw StateError('invalid protocol result id');
        }
        if (record['ok'] == true) {
          _fields(record, {'type', 'id', 'ok', 'value'});
          return ResultRecord(id, record['value'], null);
        }
        if (record['ok'] == false) {
          _fields(record, {'type', 'id', 'ok', 'error'});
          final error = _wireObject(record['error']);
          _fields(error, {'code', 'detail'});
          if (!_errorCodes.contains(error['code']) ||
              error['detail'] is! String) {
            throw StateError('invalid protocol error');
          }
          return ResultRecord(
            id,
            null,
            RpcError(error['code'] as String, error['detail'] as String),
          );
        }
        throw StateError('invalid protocol result');
      case 'event':
        _fields(record, {'type', 'seq', 'instance', 'event'});
        final seq = record['seq'];
        if (seq is! int) throw StateError('invalid protocol sequence');
        final event = _wireObject(record['event']);
        if (!const {
          'connection_state',
          'error',
          'network_changed',
          'peer_changed',
          'probe_text_received',
          'account_changed',
          'account_auth_changed',
          'account_cache_used',
        }.contains(event['kind'])) {
          throw StateError('invalid protocol event kind');
        }
        return EventRecord(_identity(record['instance']), seq, event);
      default:
        throw StateError('invalid protocol record type');
    }
  }
}

class HelloRecord extends WireRecord {
  HelloRecord(this.name, this.instance);
  final String name;
  final String instance;
}

class ResultRecord extends WireRecord {
  ResultRecord(this.id, this.value, this.error);
  final int id;
  final Object? value;
  final RpcError? error;
}

class EventRecord extends WireRecord {
  EventRecord(this.instance, this.seq, this.event);
  final String instance;
  final int seq;
  final Map<String, Object?> event;
}

/// Sequences belong to core instances, including instances that reconnect.
class SequenceGuard {
  final _last = <String, int>{};
  void accept(String instance, int seq) {
    if (seq != (_last[instance] ?? 0) + 1) {
      throw StateError('event sequence gap, duplicate or regression');
    }
    _last[instance] = seq;
  }
}
