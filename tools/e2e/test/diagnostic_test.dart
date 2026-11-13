import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:chat_e2e/src/diagnostic.dart';
import 'package:chat_e2e/src/protocol.dart';
import 'package:test/test.dart';

/// Arguments of every `name(` call in [source], with balanced parentheses.
Iterable<String> _calls(String source, String name) sync* {
  for (final match in RegExp('\\b$name(?:<[^>]*>)?\\(').allMatches(source)) {
    var depth = 1;
    var i = match.end;
    while (depth > 0) {
      final c = source[i++];
      if (c == '(') depth++;
      if (c == ')') depth--;
    }
    yield source.substring(match.end, i - 1);
  }
}

/// The trailing literal argument without interpolation, if any.
String? _lastLiteral(String arguments) =>
    RegExp(r"(?:^|,)\s*'([^'$]*)'\s*,?\s*$").firstMatch(arguments)?.group(1);

void main() {
  final allowed = loadSafeDetails();

  test('only listed fixed failure texts become a detail', () {
    expect(
      safeDetail(StateError('Android owned activity exited'), allowed),
      'Android owned activity exited',
    );
    expect(
      safeDetail(StateError('participant disconnected'), allowed),
      'participant disconnected',
    );
    expect(
      safeDetail(
        TimeoutException('Message scenario deadline: contact_blocked'),
        allowed,
      ),
      'Message scenario deadline: contact_blocked',
    );
    expect(
      safeDetail(const RpcError('ContactRequired', '[redacted]'), allowed),
      'RPC failed: ContactRequired',
    );
    expect(
      safeDetail(TimeoutException('Future not completed'), allowed),
      'Future not completed',
    );
    for (final private in <Object?>[
      StateError('Command failed (1): /home/PRIVATE_TOKEN/adb'),
      StateError('Message scenario: PRIVATE_TOKEN'),
      TimeoutException(null),
      const RpcError('PRIVATE_TOKEN', 'PRIVATE_TOKEN'),
      Exception('participant disconnected'),
      'participant disconnected',
      null,
    ]) {
      expect(safeDetail(private, allowed), isNull, reason: '$private');
    }
  });

  test('a deadline names the last read only as a fixed category', () {
    expect(
      observedView({'phase': 'Locked', 'peer': 'PRIVATE_TOKEN'}),
      'Locked',
    );
    expect(observedView({'phase': 'PRIVATE_TOKEN'}), 'other');
    expect(observedView({'items': []}), 'other');
    expect(
      observedView(const RpcError('NotAvailable', 'PRIVATE_TOKEN')),
      'NotAvailable',
    );
    expect(observedView(const RpcError('PRIVATE_TOKEN', 'x')), 'other');
    final views =
        (jsonDecode(File('diagnostic_details.json').readAsStringSync())
                as Map<String, dynamic>)['last_view']
            as List;
    for (final phase in [
      'Unconfigured',
      'Active',
      'Locked',
      'NotAvailable',
      'other',
    ]) {
      expect(views, contains(phase));
    }
    expect(
      safeDetail(
        ScenarioDeadline('Account state deadline', lastView: 'Active'),
        allowed,
      ),
      'Account state deadline',
    );
  });

  test('a failed pairing notes both sides only as fixed values', () async {
    failureFacts.clear();
    await notePairSide(
      'owner',
      () async => {
        'phase': 'Active',
        'public_peer_id': 'PRIVATE_TOKEN',
        'pair': {'phase': 'WaitingQr', 'sas': 'PRIVATE_TOKEN'},
        'publication': {'state': 'Observed'},
      },
      () async => {'state': 'Online', 'ticket': 'PRIVATE_TOKEN'},
    );
    await notePairSide(
      'scanner',
      () async => throw StateError('PRIVATE'),
      () async => {'state': 'PRIVATE_TOKEN'},
    );
    expect(failureFacts, {
      'owner_phase': 'Active',
      'owner_pair': 'WaitingQr',
      'owner_publication': 'Observed',
      'owner_network': 'Online',
      'scanner_network': 'unknown',
      'scanner_phase': 'unknown',
      'scanner_pair': 'unknown',
      'scanner_publication': 'unknown',
    });
    await notePairSide(
      'scanner',
      () async => {
        'phase': 'Candidate',
        'pair': null,
        'publication': 'PRIVATE',
      },
      () async => throw StateError('PRIVATE'),
    );
    expect(failureFacts['scanner_pair'], 'none');
    expect(failureFacts['scanner_phase'], 'Candidate');
    final allowed = loadSafeFacts();
    expect(safeFacts(failureFacts, allowed), failureFacts);
    expect(
      safeFacts({'owner_pair': 'PRIVATE_TOKEN', 'PRIVATE': 'Active'}, allowed),
      isNull,
    );
    expect(safeFacts({}, allowed), isNull);
    failureFacts.clear();
  });

  test('an m6 deadline notes every side group only as buckets', () async {
    failureFacts.clear();
    await noteGroup(
      'node',
      () async => {
        'group': true,
        'epoch': 1,
        'members': 3,
        'processed_mls': 0,
        'deliveries': 1,
        'holds': 0,
        'rejoins': 0,
        'attempted': 2,
        'PRIVATE': 'PRIVATE_TOKEN',
      },
    );
    await noteGroup(
      'mailbox',
      () async => {
        'group': false,
        'epoch': null,
        'members': 0,
        'processed_mls': 7,
        'deliveries': 0,
        'holds': 4,
        'rejoins': null,
        'attempted': 0,
      },
    );
    await noteGroup('phone', () async => throw StateError('PRIVATE'));
    expect(failureFacts, {
      'm6_group_node': 'm3',
      'm6_epoch_node': 'e1',
      'm6_received_node': 'p0',
      'm6_deliveries_node': 'd1',
      'm6_holds_node': 'h0',
      'm6_attempted_node': 'r2',
      'm6_group_mailbox': 'none',
      'm6_epoch_mailbox': 'none',
      'm6_received_mailbox': 'p3plus',
      'm6_deliveries_mailbox': 'd0',
      'm6_holds_mailbox': 'h1plus',
      'm6_attempted_mailbox': 'r0',
      'm6_group_phone': 'unknown',
      'm6_epoch_phone': 'unknown',
      'm6_received_phone': 'unknown',
      'm6_deliveries_phone': 'unknown',
      'm6_holds_phone': 'unknown',
      'm6_attempted_phone': 'unknown',
    });
    expect(safeFacts(failureFacts, loadSafeFacts()), failureFacts);
    failureFacts.clear();
  });

  test('the slowest waits keep listed names only, slowest first', () {
    waitMs.clear();
    noteWait('account_local_pair_scan', 9800);
    noteWait('account_local_pair_scan', 1200);
    noteWait('Mailbox scenario deadline: first_text', 41250);
    noteWait('timeout_probe_event', 15000);
    noteWait('network_info', 30000);
    noteWait('account_local_PRIVATE_TOKEN', 50000);
    noteWait('Mailbox scenario deadline: PRIVATE_TOKEN', 50000);
    expect(waitMs['account_local_pair_scan'], 9800);
    final kinds = loadSafeValues('wait_kinds');
    expect(safeWaits(waitMs, allowed, kinds), [
      {'name': 'Mailbox scenario deadline: first_text', 'ms': 41250},
      {'name': 'timeout_probe_event', 'ms': 15000},
      {'name': 'account_local_pair_scan', 'ms': 9800},
    ]);
    waitMs.clear();
    for (var i = 0; i < 10; i++) {
      noteWait('account_local_op_${String.fromCharCode(97 + i)}', i);
    }
    final kept = safeWaits(waitMs, allowed, kinds)!;
    expect(kept, hasLength(waitsLimit));
    expect(kept.first, {'name': 'account_local_op_j', 'ms': 9});
    waitMs.clear();
    expect(safeWaits(waitMs, allowed, kinds), isNull);
  });

  test('every m2 timeout kind is a listed wait name', () {
    final source = File('lib/src/scenario.dart').readAsStringSync();
    final kinds = {
      for (final match in RegExp(r"'(timeout_[a-z_]+)'").allMatches(source))
        match.group(1)!,
    };
    expect(kinds, isNotEmpty);
    expect(kinds.difference(loadSafeValues('wait_kinds')), isEmpty);
  });

  test('every fixed scenario code, deadline and failure text is listed', () {
    final missing = <String>{};
    void need(String text) {
      if (!allowed.contains(text)) missing.add(text);
    }

    const prefixes = {
      'account_scenario': ('Account scenario: ', null),
      'message_scenario': ('Message scenario: ', 'Message scenario deadline: '),
      'mailbox_scenario': ('Mailbox scenario: ', 'Mailbox scenario deadline: '),
    };
    for (final MapEntry(key: file, value: (code, deadline))
        in prefixes.entries) {
      final source = File('lib/src/$file.dart').readAsStringSync();
      for (final arguments in _calls(source, 'require')) {
        final literal = _lastLiteral(arguments);
        if (literal != null) need('$code$literal');
      }
      if (deadline == null) continue;
      for (final name in ['until', 'view', 'state']) {
        for (final arguments in _calls(source, name)) {
          final literal = _lastLiteral(arguments);
          if (literal != null) need('$deadline$literal');
        }
      }
    }
    final sources = [
      ...Directory('lib/src').listSync().whereType<File>(),
      File('bin/run.dart'),
    ].where((file) => file.path.endsWith('.dart'));
    for (final file in sources) {
      final source = file.readAsStringSync();
      for (final match in RegExp(
        r"\b(?:StateError|TimeoutException|ScenarioDeadline)\(\s*'([^'$]*)'(?:,?\s*\)|,\s*\w+:)",
      ).allMatches(source)) {
        need(match.group(1)!);
      }
    }
    expect(missing, isEmpty);
    expect(allowed.length, greaterThan(100));
  });
}
