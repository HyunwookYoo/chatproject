import 'dart:async';
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
        TimeoutException('Message scenario deadline: message_state'),
        allowed,
      ),
      'Message scenario deadline: message_state',
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
      for (final name in ['until', 'view']) {
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
        r"\b(?:StateError|TimeoutException)\(\s*'([^'$]*)',?\s*\)",
      ).allMatches(source)) {
        need(match.group(1)!);
      }
    }
    expect(missing, isEmpty);
    expect(allowed.length, greaterThan(100));
  });
}
