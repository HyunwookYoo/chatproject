import 'dart:async';

import 'package:chat_app/probe/probe_host.dart';
import 'package:chat_app/probe/probe_screen.dart';
import 'package:chat_app/theme/app_theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

class FakeProbeHost implements ProbeHost {
  FakeProbeHost({this.log, this.allow = true, this.tokenOnRequest = 'a1b2c3', this.errorOnRequest});

  String? token;
  String? error;
  String? log;
  final bool allow;

  /// What iOS answers once the app has asked to register: a token, an error, or (both null) nothing.
  final String? tokenOnRequest;
  final String? errorOnRequest;

  /// When set, requestPush waits for it, as the real call waits for iOS.
  Completer<void>? gate;
  bool cleared = false;

  @override
  Future<bool> requestPush() async {
    if (!allow) return false;
    await gate?.future;
    token = tokenOnRequest;
    error = errorOnRequest;
    return true;
  }

  @override
  Future<String?> deviceToken() async => token;

  @override
  Future<String?> registrationError() async => error;

  @override
  Future<String?> readNseLog() async => log;

  @override
  Future<void> clearNseLog() async {
    cleared = true;
    log = null;
  }
}

const _log = '''
{"phase":"start","pid":7,"seq":1,"sent_at":1000,"received_at":1400,"footprint":1}
{"phase":"end","pid":7,"seq":1,"ok":false,"error":"noCiphertext","footprint":1,"peak":4194304,"available":0}
{"phase":"start","pid":7,"seq":2,"sent_at":2000,"received_at":2300,"footprint":1}
{"phase":"end","pid":7,"seq":2,"ok":true,"plaintext_len":1024,"load_us":3700,"decrypt_us":1700,"footprint":1,"peak":6291456,"available":0}
{"phase":"start","pid":8,"seq":3,"sent_at":3000,"received_at":3100,"footprint":1}
''';

const _failedLog = '''
{"phase":"start","pid":9,"seq":4,"sent_at":4000,"received_at":4100,"footprint":1}
{"phase":"end","pid":9,"seq":4,"ok":false,"error":"noState","footprint":1,"peak":5242880,"available":0}
{"phase":"start","pid":9,"seq":5,"sent_at":5000,"received_at":5100,"footprint":1}
{"phase":"end","pid":9,"seq":5,"ok":false,"footprint":1,"peak":5242880,"available":0}
''';

Future<void> pumpProbe(WidgetTester tester, FakeProbeHost host) async {
  await tester.pumpWidget(MaterialApp(theme: buildAppTheme(), home: ProbeScreen(host: host)));
  await tester.pumpAndSettle();
}

/// Records what the app puts on the clipboard.
List<String?> recordClipboard(WidgetTester tester) {
  final copied = <String?>[];
  tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(SystemChannels.platform, (call) async {
    if (call.method == 'Clipboard.setData') copied.add((call.arguments as Map)['text'] as String?);
    return null;
  });
  addTearDown(() => tester.binding.defaultBinaryMessenger.setMockMethodCallHandler(SystemChannels.platform, null));
  return copied;
}

void main() {
  testWidgets('shows empty state without records', (tester) async {
    await pumpProbe(tester, FakeProbeHost());
    expect(find.text('아직 기록이 없어요'), findsOneWidget);
    expect(find.text('알림 허용하고 토큰 받기'), findsOneWidget);
  });

  testWidgets('allowing notifications shows the token', (tester) async {
    await pumpProbe(tester, FakeProbeHost());
    await tester.tap(find.text('알림 허용하고 토큰 받기'));
    await tester.pumpAndSettle();
    expect(find.text('a1b2c3'), findsOneWidget);
    expect(find.text('토큰 복사'), findsOneWidget);
  });

  testWidgets('the token shows up once iOS has delivered it, without refreshing', (tester) async {
    final host = FakeProbeHost()..gate = Completer<void>();
    await pumpProbe(tester, host);
    await tester.tap(find.text('알림 허용하고 토큰 받기'));
    await tester.pump();
    expect(find.text('토큰 받는 중…'), findsOneWidget);
    expect(tester.widget<FilledButton>(find.byType(FilledButton)).onPressed, isNull);

    host.gate!.complete();
    await tester.pumpAndSettle();
    expect(find.text('a1b2c3'), findsOneWidget);
    expect(find.text('토큰 받는 중…'), findsNothing);
  });

  testWidgets('a registration error is shown and the button stays for another try', (tester) async {
    final host = FakeProbeHost(tokenOnRequest: null, errorOnRequest: 'no valid aps-environment (NSCocoaErrorDomain 3000)');
    await pumpProbe(tester, host);
    await tester.tap(find.text('알림 허용하고 토큰 받기'));
    await tester.pumpAndSettle();
    expect(find.text('토큰을 받지 못했어요 · no valid aps-environment (NSCocoaErrorDomain 3000)'), findsOneWidget);
    expect(find.text('알림 허용하고 토큰 받기'), findsOneWidget);
  });

  testWidgets('without a token or an error the screen says to refresh, and refreshing finds a late token', (tester) async {
    final host = FakeProbeHost(tokenOnRequest: null);
    await pumpProbe(tester, host);
    await tester.tap(find.text('알림 허용하고 토큰 받기'));
    await tester.pumpAndSettle();
    expect(find.text('토큰이 아직 안 왔어요. 잠시 뒤 새로고침을 눌러 보세요.'), findsOneWidget);

    host.token = 'late';
    await tester.tap(find.text('새로고침'));
    await tester.pumpAndSettle();
    expect(find.text('late'), findsOneWidget);
    expect(find.text('토큰이 아직 안 왔어요. 잠시 뒤 새로고침을 눌러 보세요.'), findsNothing);
  });

  testWidgets('a denied permission says how to turn it on', (tester) async {
    await pumpProbe(tester, FakeProbeHost(allow: false));
    await tester.tap(find.text('알림 허용하고 토큰 받기'));
    await tester.pumpAndSettle();
    expect(find.text('알림이 꺼져 있어요. 설정 앱에서 허용해 주세요.'), findsOneWidget);
  });

  testWidgets('shows the largest peak and one line per notification', (tester) async {
    await pumpProbe(tester, FakeProbeHost(log: _log));
    expect(find.text('가장 큰 피크 6.0 MB · 기준 15 MB 미만'), findsOneWidget);
    expect(find.text('#1 · 기준(복호 없음) · 피크 4.0 MB · 지연 400 ms'), findsOneWidget);
    expect(find.text('#2 · 복호 성공 · 피크 6.0 MB · 로드 3.7 ms · 복호 1.7 ms · 지연 300 ms'), findsOneWidget);
    expect(find.text('#3 · 끝 기록 없음 (중단됐을 수 있어요)'), findsOneWidget);
  });

  testWidgets('a failed decryption shows its reason, or says there is none', (tester) async {
    await pumpProbe(tester, FakeProbeHost(log: _failedLog));
    expect(find.text('#4 · 복호 실패 · noState'), findsOneWidget);
    expect(find.text('#5 · 복호 실패 · 이유 없음'), findsOneWidget);
    expect(find.text('가장 큰 피크 5.0 MB · 기준 15 MB 미만'), findsOneWidget);
  });

  testWidgets('copying the log puts the raw file text on the clipboard', (tester) async {
    final copied = recordClipboard(tester);
    final raw = '${_log}not json\n'; // the parser skips this line, the copy keeps it
    await pumpProbe(tester, FakeProbeHost(log: raw));
    await tester.tap(find.text('기록 복사'));
    await tester.pumpAndSettle();
    expect(copied, [raw]);
    expect(find.text('기록을 복사했어요'), findsOneWidget);
  });

  testWidgets('there is nothing to copy without a log', (tester) async {
    await pumpProbe(tester, FakeProbeHost());
    expect(tester.widget<TextButton>(find.widgetWithText(TextButton, '기록 복사')).onPressed, isNull);
  });

  testWidgets('clearing the log empties the list', (tester) async {
    final host = FakeProbeHost(log: _log);
    await pumpProbe(tester, host);
    await tester.tap(find.text('기록 지우기'));
    await tester.pumpAndSettle();
    expect(host.cleared, isTrue);
    expect(find.text('아직 기록이 없어요'), findsOneWidget);
  });
}
