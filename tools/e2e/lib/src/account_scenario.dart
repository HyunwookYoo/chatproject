import 'dart:async';
import 'dart:convert';
import 'controller.dart';
import 'protocol.dart';

typedef AccountRestart =
    Future<Participant> Function(Participant, {bool freshStore});

/// Local debug UI actions on the same native account actor as Flutter. Secret
/// display responses stay in memory; receipts contain only public identities.
Future<void> runAccountScenario(
  Controller controller,
  List<Participant> participants, {
  required AccountRestart restart,
  bool authOnly = false,
  Future<int> Function(Participant)? wallClock,
  void Function(String, Map<String, Object?>)? evidence,
  Duration timeout = const Duration(seconds: 45),
}) async {
  if (participants.length != 2) {
    throw ArgumentError('Two app participants required');
  }
  var a = participants[0];
  var b = participants[1];
  final recovery = <String>[];
  Map<String, Object?> object(Object? v) => (v as Map).cast<String, Object?>();
  bool same(Object? a, Object? b) => jsonEncode(a) == jsonEncode(b);
  void require(bool condition, String code) {
    if (!condition) throw StateError('Account scenario: $code');
  }

  Future<Object?> call(
    Participant p,
    String op, [
    Map<String, Object?> args = const {},
  ]) => p.call('account_local_$op', args, timeout: timeout);
  Future<Map<String, Object?>> snapshot(Participant p) async =>
      object(await call(p, 'snapshot'));
  Future<Map<String, Object?>> until(
    Participant p,
    bool Function(Map<String, Object?>) predicate,
  ) async {
    final deadline = DateTime.now().add(timeout);
    while (DateTime.now().isBefore(deadline)) {
      try {
        final view = await snapshot(p);
        if (predicate(view)) return view;
      } on RpcError catch (e) {
        if (e.code != 'NotAvailable') rethrow;
      }
      await Future<void>.delayed(const Duration(milliseconds: 100));
    }
    throw TimeoutException('Account state deadline');
  }

  Future<Map<String, Object?>> active(Participant p) =>
      until(p, (v) => v['phase'] == 'Active');
  Future<void> record(String stage, Participant p) async {
    final view = await snapshot(p);
    final network = object(await p.call('network_info', {}));
    evidence?.call(stage, {
      'instance': p.instance,
      'peer': view['public_peer_id'],
      'installation': view['installation'],
      'endpoint': network['endpoint_id'],
      'phase': view['phase'],
      'devices': view['devices'],
    });
  }

  Future<Participant> restarted(
    Participant p, {
    bool freshStore = false,
  }) async {
    final before = await snapshot(p);
    final endpoint = object(await p.call('network_info', {}))['endpoint_id'];
    final next = await restart(p, freshStore: freshStore);
    require(
      next.instance != p.instance,
      'restart must create new core instance',
    );
    final after = await until(next, (_) => true);
    if (!freshStore) {
      require(
        same(before['public_peer_id'], after['public_peer_id']),
        'restart peer changed',
      );
      require(
        same(before['installation'], after['installation']),
        'restart installation changed',
      );
      final nextEndpoint = object(
        await next.call('network_info', {}),
      )['endpoint_id'];
      require(
        endpoint != null && endpoint == nextEndpoint,
        'M3 restart endpoint changed',
      );
    } else {
      require(
        after['phase'] == 'Unconfigured',
        'fresh owned store already configured',
      );
    }
    await record(freshStore ? 'fresh_store' : 'stable_restart', next);
    return next;
  }

  Future<void> create(Participant p, {bool keepRecovery = false}) async {
    await until(p, (v) => v['phase'] == 'Unconfigured');
    final display = object(await call(p, 'begin_create'));
    final words = (display['words'] as List).cast<String>();
    require(words.length == 24, 'recovery word count');
    if (keepRecovery) recovery.addAll(words);
    final answers = (display['positions'] as List)
        .map((i) => words[(i as int) - 1])
        .toList();
    await call(p, 'confirm_create', {
      'flow': display['flow'],
      'answers': answers,
    });
    answers.clear();
    words.clear();
    display.clear();
    await call(p, 'refresh_record', {
      'peer': (await snapshot(p))['public_peer_id'],
    });
    await active(p);
    await record('created', p);
  }

  Future<void> pair(String direction, {bool pausePublication = false}) async {
    final owner = direction == 'NewDisplays' ? b : a;
    final scanner = direction == 'NewDisplays' ? a : b;
    final display = object(
      await call(owner, 'pair_begin', {'direction': direction, 'resume': null}),
    );
    final flow = display['flow'];
    if (wallClock != null) {
      evidence?.call('pair_clock', {
        'direction': direction,
        'expires_at': display['expires_at'],
        'owner_seconds': await wallClock(owner),
        'scanner_seconds': await wallClock(scanner),
      });
    }
    final target = object(await owner.call('network_info', {}))['ticket'];
    await call(scanner, 'route', {'ticket': target});
    final scanned = await call(scanner, 'pair_scan', {'qr': display['qr']});
    display.clear();
    require(same(flow, scanned), 'pair flow mismatch');
    final av = await until(
      a,
      (v) => v['pair'] is Map && (v['pair'] as Map)['sas'] != null,
    );
    final bv = await until(
      b,
      (v) => v['pair'] is Map && (v['pair'] as Map)['sas'] != null,
    );
    require(
      (av['pair'] as Map)['sas'] == (bv['pair'] as Map)['sas'],
      'SAS mismatch',
    );
    require(bv['phase'] == 'Candidate', 'candidate activated before approval');
    final candidateEndpoint = object(
      await b.call('network_info', {}),
    )['endpoint_id'];
    require(
      candidateEndpoint is String &&
          RegExp(r'^[0-9a-f]{64}$').hasMatch(candidateEndpoint),
      'candidate endpoint malformed',
    );
    final candidateTransport = List<int>.generate(
      32,
      (i) => int.parse(
        (candidateEndpoint as String).substring(i * 2, i * 2 + 2),
        radix: 16,
      ),
    );
    bool included(Map<String, Object?> view) {
      final publication = view['publication'];
      if (publication is! Map ||
          publication['state'] != 'Observed' ||
          publication['accepted'] is! Map) {
        return false;
      }
      return ((publication['accepted'] as Map)['devices'] as List).any(
        (d) =>
            same((d as Map)['installation_id'], bv['installation']) &&
            same(d['transport_public_key'], candidateTransport),
      );
    }

    await call(a, 'pair_confirm_code', {'flow': flow, 'matches': true});
    await call(b, 'pair_confirm_code', {'flow': flow, 'matches': true});
    // Remote consent has to reach E before the explicit local approval command.
    await until(
      a,
      (v) =>
          v['pair'] is Map && (v['pair'] as Map)['phase'] == 'AwaitingApproval',
    );
    controller.recordPublicationsPaused = pausePublication;
    await call(a, 'pair_approve', {'flow': flow});
    if (pausePublication) {
      final pending = await until(b, (v) => v['phase'] == 'PendingImport');
      require(
        (pending['pending_pair_transactions'] as List).isNotEmpty,
        'pending transaction absent',
      );
      for (final action in ['contacts', 'connect_account', 'create_invite']) {
        final args = action == 'connect_account'
            ? {'peer': av['public_peer_id']}
            : action == 'create_invite'
            ? {'name': null}
            : <String, Object?>{};
        var denied = false;
        try {
          await call(b, action, args);
        } on RpcError catch (e) {
          denied = e.code == 'AccountRestricted' || e.code == 'NotAvailable';
        }
        require(denied, 'pending import allowed normal account command');
      }
      await record('pending_import_never_normal_sync', b);
      controller.recordPublicationsPaused = false;
      await call(a, 'refresh_record', {'peer': av['public_peer_id']});
      // A publish acknowledgement is not an observed membership record.
      await call(a, 'refresh_record', {'peer': av['public_peer_id']});
    }
    await until(a, (v) => v['phase'] == 'Active' && included(v));
    await record('membership_observed', a);
    // Publication failure can close the ephemeral channel and its finite import
    // polling. The ordinary Refresh action rechecks the durable import.
    await call(b, 'refresh_record', {'peer': av['public_peer_id']});
    await active(b);
    final current = await snapshot(a);
    final joined = await snapshot(b);
    require(
      same(current['public_peer_id'], joined['public_peer_id']),
      'paired peer mismatch',
    );
    require(included(current), 'membership not observed');
    await record('paired_$direction', b);
  }

  try {
    await create(a, keepRecovery: true);
    await create(b);
    final aPeer = (await snapshot(a))['public_peer_id'];
    final bPeer = (await snapshot(b))['public_peer_id'];
    for (final peers in [(a, b), (b, a)]) {
      var invite = await call(peers.$1, 'create_invite', {'name': '로컬 시험'});
      final contact = object(
        await call(peers.$2, 'accept_invite', {
          'input': invite,
          'alias': '시험 연락처',
        }),
      );
      invite = null;
      require(contact['verified'] == false, 'invite automatically verified');
    }
    final sa = object(await call(a, 'safety', {'local': aPeer, 'peer': bPeer}));
    final sb = object(await call(b, 'safety', {'local': bPeer, 'peer': aPeer}));
    require(same(sa['digest'], sb['digest']), 'safety digest mismatch');
    await call(a, 'confirm_safety', {'peer': bPeer, 'digest': sa['digest']});
    await call(b, 'confirm_safety', {'peer': aPeer, 'digest': sb['digest']});
    require(
      (await call(a, 'contacts') as List).every(
        (v) => (v as Map)['verified'] == true,
      ),
      'safety verification not saved',
    );
    if (authOnly) {
      Future<void> routeBoth() async {
        for (final pair in [(a, b), (b, a)]) {
          final deadline = DateTime.now().add(timeout);
          Object? ticket;
          while (DateTime.now().isBefore(deadline)) {
            ticket = object(await pair.$2.call('network_info', {}))['ticket'];
            if (ticket is String) break;
            await Future<void>.delayed(const Duration(milliseconds: 100));
          }
          require(ticket is String, 'public route did not become ready');
          await call(pair.$1, 'route', {'ticket': ticket});
        }
      }

      bool proved(Map<String, Object?> v) => (v['contacts'] as List).any(
        (c) => (c as Map)['device_unverified'] == false,
      );
      bool cached(Map<String, Object?> v) => (v['remote_records'] as List).any(
        (r) => (r as Map)['cached'] == true,
      );
      Future<void> proof(
        String stage,
        Participant initiator,
        Object? peer,
        bool useCache,
      ) async {
        await call(initiator, 'connect_account', {'peer': peer});
        for (final p in [a, b]) {
          final v = await until(p, proved);
          require(
            cached(v) == useCache,
            'incorrect cache provenance at $stage',
          );
          evidence?.call(stage, {
            'instance': p.instance,
            'contacts': v['contacts'],
            'remote_records': v['remote_records'],
          });
        }
      }

      await routeBoth();
      await proof('auth_fresh', a, bPeer, false);
      controller.recordLookupsPaused = true;
      await proof('auth_cached', a, bPeer, true);
      controller.recordLookupsPaused = false;
      await proof('auth_fresh_again', a, bPeer, false);
      await b.call('network_stop', {});
      final disconnected = await until(a, (v) => !proved(v));
      require(
        !cached(disconnected),
        'disconnected proof retained cache status',
      );
      var failed = false;
      try {
        await call(a, 'connect_account', {'peer': bPeer});
      } on RpcError {
        failed = true;
      }
      require(
        failed && !proved(await snapshot(a)),
        'failed proof showed ready',
      );
      await record('auth_disconnected', a);
      final beforeRestart = await snapshot(b);
      final oldInstance = b.instance;
      b = await restart(b);
      final afterRestart = await until(b, (_) => true);
      require(
        b.instance != oldInstance &&
            same(
              beforeRestart['public_peer_id'],
              afterRestart['public_peer_id'],
            ) &&
            same(beforeRestart['installation'], afterRestart['installation']),
        'auth restart identity changed',
      );
      require(
        !proved(await snapshot(b)) && !cached(await snapshot(b)),
        'restart retained authorization',
      );
      await b.call('network_start', {});
      await routeBoth();
      await proof('auth_restarted', b, aPeer, false);
      return;
    }
    b = await restarted(b, freshStore: true);
    await call(b, 'prepare_candidate');
    b = await restarted(b);
    await pair('NewDisplays', pausePublication: true);
    final firstInstallation = (await snapshot(b))['installation'];
    b = await restarted(b);
    b = await restarted(b, freshStore: true);
    await call(b, 'prepare_candidate');
    await pair('ExistingDisplays');
    await call(a, 'revoke_device', {'target': firstInstallation});
    await call(a, 'refresh_record', {'peer': aPeer});
    await until(
      a,
      (v) =>
          v['phase'] == 'Active' &&
          !(v['devices'] as List).any(
            (d) => same((d as Map)['installation_id'], firstInstallation),
          ),
    );
    await record('revocation_observed', a);
    b = await restarted(b, freshStore: true);
    await call(b, 'recover', {
      'words': recovery.join(' '),
      'confirm_replacement': true,
    });
    recovery.clear();
    await call(b, 'refresh_record', {'peer': aPeer});
    final recovered = await active(b);
    require(
      same(recovered['public_peer_id'], aPeer),
      'recovery changed PeerId',
    );
    require(
      !same(recovered['installation'], firstInstallation),
      'recovery reused installation',
    );
    require(
      (recovered['contacts'] as List).isEmpty,
      'recovery restored contacts',
    );
    await record('same_peer_recovery', b);
    b = await restarted(b);
    await record('recovery_restart', b);
  } finally {
    recovery.clear();
    controller.recordPublicationsPaused = false;
    controller.recordLookupsPaused = false;
  }
}
