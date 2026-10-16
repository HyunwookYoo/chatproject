import 'dart:async';

import 'controller.dart';
import 'protocol.dart';

/// Exercises one foreground session; process ownership stays with the caller.
Future<void> runScenario(
  Controller controller,
  List<Participant> participants, {
  required Future<Participant> Function(Participant) restart,
  Duration timeout = const Duration(seconds: 15),
  void Function(Object, String?, String?)? onFailure,
}) async {
  if (participants.length != 3) {
    throw ArgumentError('Three participants required');
  }
  final current = participants.toList();
  var reported = false;
  void report(Object error, [String? phase, String? kind]) {
    if (reported) return;
    reported = true;
    onFailure?.call(error, phase, kind);
  }

  // Slots follow positions, including a replacement node, never private names.
  const slots = ['scenario_app1', 'scenario_app2', 'scenario_node'];
  String slot(Participant peer) => slots[current.indexOf(peer)];
  String pair(Participant subject, Participant observer) => const {
    (0, 1): 'scenario_app1_to_app2',
    (0, 2): 'scenario_app1_to_node',
    (1, 0): 'scenario_app2_to_app1',
    (1, 2): 'scenario_app2_to_node',
    (2, 0): 'scenario_node_to_app1',
    (2, 1): 'scenario_node_to_app2',
  }[(current.indexOf(subject), current.indexOf(observer))]!;
  Future<T> positive<T>(Future<T> work, String phase, String kind) async {
    try {
      return await work;
    } on TimeoutException catch (error) {
      report(error, phase, kind);
      rethrow;
    }
  }

  final addresses = <String, Map<String, Object?>>{};
  final token = DateTime.now().microsecondsSinceEpoch;
  var counter = 0;
  String fresh(String stage) => '$stage:$token:${++counter}:안녕하세요 🦀';
  Future<Object?> call(
    Participant peer,
    String op, [
    Map<String, Object?> args = const {},
  ]) =>
      positive(peer.call(op, args, timeout: timeout), slot(peer), switch (op) {
        'network_start' => 'timeout_network_start',
        'network_info' => 'timeout_network_info',
        'connect_peer' => 'timeout_connect_peer',
        'send_probe_text' => 'timeout_send_probe_text',
        'set_offline' => 'timeout_set_offline',
        'disconnect_peer' => 'timeout_disconnect_peer',
        'network_stop' => 'timeout_network_stop',
        _ => throw ArgumentError('Unsupported scenario operation'),
      });
  Map<String, Object?> object(Object? value) =>
      (value as Map).cast<String, Object?>();
  Future<Map<String, Object?>> online(Participant peer, Object? result) async {
    var info = object(result);
    final deadline = DateTime.now().add(timeout);
    while (info['state'] != 'Online') {
      final revision = info['revision'] as int;
      await positive(
        peer.expectEvent(
          (e) =>
              e['kind'] == 'network_changed' &&
              e['state'] == 'Online' &&
              (e['revision'] as int) >= revision,
          timeout: deadline.difference(DateTime.now()),
        ),
        slot(peer),
        'timeout_state_event',
      );
      info = object(await call(peer, 'network_info'));
    }
    if (info['endpoint_id'] is! String ||
        info['ticket'] is! String ||
        (info['endpoint_id'] as String).isEmpty ||
        (info['ticket'] as String).isEmpty) {
      throw StateError('Online address missing');
    }
    return info;
  }

  Future<void> connect(Participant left, Participant right) async {
    await call(left, 'connect_peer', {
      'target': addresses[right.name]!['ticket'],
    });
  }

  Future<void> exchange(
    Participant sender,
    Participant receiver,
    String stage,
  ) async {
    final body = fresh(stage);
    await call(sender, 'send_probe_text', {
      'peer_id': addresses[receiver.name]!['endpoint_id'],
      'text': body,
    });
    await positive(
      receiver.expectEvent(
        (e) =>
            e['kind'] == 'probe_text_received' &&
            e['peer_id'] == addresses[sender.name]!['endpoint_id'] &&
            e['text'] == body,
        timeout: timeout,
      ),
      pair(sender, receiver),
      'timeout_probe_event',
    );
  }

  Future<void> knownPath(Participant peer, Participant remote) async {
    final id = addresses[remote.name]!['endpoint_id'];
    bool known(Map<String, Object?> value) =>
        value['peer_id'] == id &&
        value['phase'] == 'Connected' &&
        ['Direct', 'Relay'].contains(value['path']);
    final deadline = DateTime.now().add(timeout);
    while (true) {
      final info = object(await call(peer, 'network_info'));
      if ((info['peers'] as List).any((v) => known(object(v)))) return;
      final revision = info['revision'] as int;
      await positive(
        peer.expectEvent(
          (e) =>
              e['kind'] == 'peer_changed' &&
              (e['revision'] as int) >= revision &&
              known(object(e['peer'])),
          timeout: deadline.difference(DateTime.now()),
        ),
        pair(remote, peer),
        'timeout_state_event',
      );
      // Re-read: a later snapshot may have superseded this buffered path event.
    }
  }

  Future<void> absent(
    Participant peer,
    bool Function(Map<String, Object?>) predicate,
    Duration duration,
  ) async {
    try {
      await peer.expectEvent(predicate, timeout: duration);
    } on TimeoutException {
      return;
    }
    throw StateError('Forbidden replay or retry event observed');
  }

  Object? failure;
  StackTrace? failureStack;
  try {
    for (final peer in current) {
      addresses[peer.name] = await online(
        peer,
        await call(peer, 'network_start'),
      );
    }
    for (var a = 0; a < current.length; a++) {
      for (var b = a + 1; b < current.length; b++) {
        final left = current[a], right = current[b];
        await connect(left, right);
        await exchange(left, right, '${left.name}>${right.name}');
        await exchange(right, left, '${right.name}>${left.name}');
        await knownPath(left, right);
        await knownPath(right, left);
      }
    }

    final owner = current.first, remote = current[1];
    final ownerRevision =
        object(await call(owner, 'network_info'))['revision'] as int;
    await call(remote, 'set_offline', {'offline': true});
    final failedBody = fresh('offline-failed');
    try {
      await call(remote, 'send_probe_text', {
        'peer_id': addresses[owner.name]!['endpoint_id'],
        'text': failedBody,
      });
      throw StateError('Offline send unexpectedly succeeded');
    } on RpcError catch (error) {
      if (error.code != 'PeerNotConnected') rethrow;
    }
    await positive(
      owner.expectEvent(
        (e) =>
            e['kind'] == 'peer_changed' &&
            (e['revision'] as int) > ownerRevision &&
            object(e['peer'])['peer_id'] ==
                addresses[remote.name]!['endpoint_id'] &&
            ['Reconnecting', 'Disconnected'].contains(object(e['peer'])['phase']),
        timeout: timeout,
      ),
      pair(remote, owner),
      'timeout_offline_event',
    );
    addresses[remote.name] = await online(
      remote,
      await call(remote, 'set_offline', {'offline': false}),
    );
    await knownPath(owner, remote);
    await knownPath(remote, owner);
    await exchange(owner, remote, 'recovered-forward');
    await exchange(remote, owner, 'recovered-reverse');
    await absent(
      owner,
      (e) => e['kind'] == 'probe_text_received' && e['text'] == failedBody,
      timeout < const Duration(seconds: 1)
          ? timeout
          : const Duration(seconds: 1),
    );

    // Keep the remote offline so independently accepted fresh inbound sessions
    // cannot be mistaken for this owner's outgoing retry intention.
    await call(remote, 'set_offline', {'offline': true});
    await call(remote, 'disconnect_peer', {
      'peer_id': addresses[owner.name]!['endpoint_id'],
    });
    final remoteId = addresses[remote.name]!['endpoint_id'];
    await call(owner, 'disconnect_peer', {'peer_id': remoteId});
    final disconnected = object(await call(owner, 'network_info'));
    final revision = disconnected['revision'] as int;
    if ((disconnected['peers'] as List).any(
      (p) =>
          object(p)['peer_id'] == remoteId &&
          object(p)['phase'] != 'Disconnected',
    )) {
      throw StateError('Disconnect retained outgoing intent');
    }
    await absent(
      owner,
      (e) =>
          e['kind'] == 'peer_changed' &&
          (e['revision'] as int) > revision &&
          object(e['peer'])['peer_id'] == remoteId &&
          [
            'Connecting',
            'Reconnecting',
            'Connected',
          ].contains(object(e['peer'])['phase']),
      timeout < const Duration(seconds: 15)
          ? timeout
          : const Duration(seconds: 32),
    );
    addresses[remote.name] = await online(
      remote,
      await call(remote, 'set_offline', {'offline': false}),
    );
    await connect(owner, remote);
    await exchange(owner, remote, 'explicit-after-disconnect');

    final old = current.last;
    final oldAddress = addresses[old.name]!;
    final replacement = await restart(old);
    current[current.length - 1] = replacement;
    if (replacement.name != old.name || replacement.instance == old.instance) {
      throw StateError('Restart did not replace core instance');
    }
    final nextAddress = await online(
      replacement,
      await call(replacement, 'network_start'),
    );
    if (nextAddress['endpoint_id'] == oldAddress['endpoint_id'] ||
        nextAddress['ticket'] == oldAddress['ticket']) {
      throw StateError('Restart did not replace endpoint address');
    }
    addresses[replacement.name] = nextAddress;
    await connect(owner, replacement);
    await exchange(owner, replacement, 'restart-forward');
    await exchange(replacement, owner, 'restart-reverse');
    await knownPath(owner, replacement);
    // Include the later Disconnect/restart stages in the no-replay check.
    await absent(
      owner,
      (e) => e['kind'] == 'probe_text_received' && e['text'] == failedBody,
      timeout < const Duration(seconds: 1)
          ? timeout
          : const Duration(seconds: 1),
    );
  } catch (error, stack) {
    report(error);
    failure = error;
    failureStack = stack;
  } finally {
    for (final peer in current) {
      try {
        await call(peer, 'network_stop');
      } catch (error, stack) {
        if (failure == null) report(error);
        failure ??= error;
        failureStack ??= stack;
      }
    }
  }
  if (failure != null) Error.throwWithStackTrace(failure, failureStack!);
}
