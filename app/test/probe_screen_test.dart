import 'package:chat_app/probe/probe_host.dart';
import 'package:chat_app/probe/probe_screen.dart';
import 'package:chat_app/theme/app_theme.dart';
import 'package:flutter/material.dart';
import 'package:flutter_test/flutter_test.dart';

class FakeProbeHost implements ProbeHost {
  FakeProbeHost({this.log, this.allow = true});

  String? token;
  String? log;
  final bool allow;
  bool cleared = false;

  @override
  Future<bool> requestPush() async {
    if (allow) token = 'a1b2c3';
    return allow;
  }

  @override
  Future<String?> deviceToken() async => token;

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

Future<void> pumpProbe(WidgetTester tester, FakeProbeHost host) async {
  await tester.pumpWidget(MaterialApp(theme: buildAppTheme(), home: ProbeScreen(host: host)));
  await tester.pumpAndSettle();
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
    expect(find.text('#2 · 복호 성공 · 피크 6.0 MB · 복호 1.7 ms · 지연 300 ms'), findsOneWidget);
    expect(find.text('#3 · 끝 기록 없음 (중단됐을 수 있어요)'), findsOneWidget);
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
