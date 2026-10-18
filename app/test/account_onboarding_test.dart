import 'dart:async';
import 'package:chat_app/account/account_client.dart';
import 'package:chat_app/account/account_model.dart';
import 'package:chat_app/account/account_routes.dart';
import 'package:chat_app/account/sensitive_screen.dart';
import 'package:chat_app/src/rust/api/account.dart' as a;
import 'package:chat_app/src/rust/api/chat.dart' as r;
import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';

// Public invalid fixture words, never a real mnemonic.
class ScreenClient implements AccountClient {
  final controller = StreamController<r.ChatEvent>.broadcast(sync: true);
  a.AccountPhase phase = a.AccountPhase.unconfigured;
  int revision = 1,
      begins = 0,
      cancels = 0,
      confirms = 0,
      recoveries = 0,
      accepts = 0,
      verifications = 0;
  r.ErrorCode? failure;
  List<String> answers = [];
  List<a.ContactView> saved = [];
  Completer<a.RecoveryDisplay>? pending;
  Completer<void>? cancelWait;
  Uint8List? nextFlow;
  final flow = Uint8List.fromList(List.filled(16, 7));
  a.AccountView view() => a.AccountView(
    revision: BigInt.from(revision),
    phase: phase,
    publicPeerId: phase == a.AccountPhase.unconfigured ? null : Uint8List(32),
    devices: [],
    contacts: saved,
    publishIntents: [],
    publication: a.PublicationView(
      state: a.PublicationState.pending,
      observedSeq: BigInt.zero,
      allocatedSeq: BigInt.one,
      tombstones: [],
      pendingCount: 1,
      pendingNotifications: [],
    ),
    disclosurePossible: false,
    pendingPairTransactions: [],
    pendingImportRequiresRemoteRevoke: false,
  );
  void emit() {
    revision++;
    controller.add(
      r.ChatEvent.accountChanged(revision: BigInt.from(revision), view: view()),
    );
  }

  void check() {
    if (failure != null) {
      throw r.CoreError(code: failure!, detail: 'PUBLIC_RAW_DETAIL');
    }
  }

  @override
  Stream<r.ChatEvent> events() => controller.stream;
  @override
  Future<void> openAccount() async {}
  @override
  Future<a.AccountView> snapshot() async => view();
  @override
  Future<a.AccountView> retryOpen() => snapshot();
  @override
  Future<a.RecoveryDisplay> beginCreate() async {
    begins++;
    return pending == null ? display() : pending!.future;
  }

  a.RecoveryDisplay display() => a.RecoveryDisplay(
    flow: nextFlow ?? flow,
    words: List.generate(24, (i) => 'public-marker-${i + 1}'),
    positions: Uint8List.fromList([1, 9, 24]),
  );
  @override
  Future<void> cancelCreate(List<int> id) async {
    expect(id, flow);
    cancels++;
    await cancelWait?.future;
  }

  @override
  Future<void> confirmCreate(List<int> id, List<String> input) async {
    confirms++;
    answers = List.of(input);
    check();
    phase = a.AccountPhase.publishPending;
    emit();
  }

  @override
  Future<void> recover(String words, bool replacement) async {
    recoveries++;
    check();
    phase = a.AccountPhase.publishPending;
    saved = [];
    emit();
  }

  @override
  Future<a.InvitePreview> parseInvite(String input, BigInt now) async {
    check();
    return a.InvitePreview(
      peerId: Uint8List.fromList(List.filled(32, 1)),
      installation: a.DeviceView(
        installationId: Uint8List(16),
        transportPublicKey: Uint8List(32),
        kind: a.DeviceKind.mobile,
      ),
      displayName: '친구\u202ePUBLIC',
      issuedAt: BigInt.zero,
      expiresAt: BigInt.from(9999999999),
    );
  }

  @override
  Future<a.ContactView> acceptInvite(String input, String alias) async {
    accepts++;
    check();
    final contact = saved.isEmpty
        ? a.ContactView(
            peerId: Uint8List.fromList(List.filled(32, 1)),
            alias: alias,
            verified: false,
            newDevice: false,
            deviceUnverified: false,
          )
        : saved.first;
    saved = [contact];
    emit();
    return contact;
  }

  @override
  Future<String> createInvite(String? name) async {
    check();
    return 'PUBLIC_INVITE';
  }

  @override
  Future<a.SafetyDisplay> safetyDisplay(
    List<int> local,
    List<int> peer,
  ) async => a.SafetyDisplay(
    digest: Uint8List.fromList(List.generate(32, (i) => i)),
    groups: List.generate(
      32,
      (i) => i.toRadixString(16).padLeft(2, '0'),
    ).join(),
    qr: 'PUBLIC_SAFETY',
  );
  @override
  Future<void> confirmSafety(List<int> peer, List<int> digest) async {
    verifications++;
    expect(digest, List.generate(32, (i) => i));
    check();
    saved = [
      a.ContactView(
        peerId: Uint8List.fromList(peer),
        alias: saved.first.alias,
        verified: true,
        newDevice: false,
        deviceUnverified: false,
      ),
    ];
    emit();
  }

  @override
  dynamic noSuchMethod(Invocation invocation) => super.noSuchMethod(invocation);
}

Future<AccountModel> mountAccount(
  WidgetTester t,
  ScreenClient c, {
  double scale = 1,
}) async {
  TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
      .setMockMethodCallHandler(
        const MethodChannel('chat_app/account_storage'),
        (_) async => null,
      );
  final m = AccountModel(c);
  await m.open();
  await t.pumpWidget(
    MaterialApp(
      home: MediaQuery(
        data: MediaQueryData(textScaler: TextScaler.linear(scale)),
        child: AccountRoutes(model: m, child: const Text('diagnostic host')),
      ),
    ),
  );
  await t.pumpAndSettle();
  addTearDown(() async {
    await t.pumpWidget(const SizedBox());
    await t.pumpAndSettle();
    m.dispose();
    await c.controller.close();
  });
  return m;
}

Future<void> tapText(WidgetTester t, String label) async {
  await t.ensureVisible(find.text(label).last);
  await t.tap(find.text(label).last);
  await t.pumpAndSettle();
}

void main() {
  test(
    'abandoned cleanup cannot clear a replacement display or set its error',
    () async {
      final c = ScreenClient()
        ..pending = Completer<a.RecoveryDisplay>()
        ..cancelWait = Completer<void>();
      final m = AccountModel(c);
      await m.open();
      final abandoned = m.beginCreate();
      m.clearSecrets();
      c.pending!.complete(c.display());
      await Future<void>.delayed(Duration.zero);
      expect(c.cancels, 1);
      c.pending = null;
      c.nextFlow = Uint8List.fromList(List.filled(16, 8));
      final replacement = await m.beginCreate();
      expect(replacement!.flow, c.nextFlow);
      c.cancelWait!.completeError(
        const r.CoreError(
          code: r.ErrorCode.notAvailable,
          detail: 'PUBLIC_RAW_DETAIL',
        ),
      );
      expect(await abandoned, isNull);
      expect(m.recoveryDisplay, same(replacement));
      expect(m.errorCode, isNull);
      m.dispose();
      await c.controller.close();
    },
  );
  testWidgets(
    'native failure cancels generated words instead of retaining them',
    (t) async {
      final c = ScreenClient();
      final m = await mountAccount(t, c);
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
          .setMockMethodCallHandler(
            const MethodChannel('chat_app/account_storage'),
            (_) async => throw PlatformException(code: 'PUBLIC_FAILURE'),
          );
      await tapText(t, '새 계정 만들기');
      expect(find.textContaining('public-marker'), findsNothing);
      expect(m.recoveryDisplay, isNull);
      expect(c.cancels, 1);
      expect(find.textContaining('보호할 수 없어요'), findsOneWidget);
    },
  );
  testWidgets(
    'background discards entered recovery text; resume starts empty',
    (t) async {
      final c = ScreenClient();
      await mountAccount(t, c);
      await tapText(t, '복구 문구로 복구');
      await t.enterText(find.byType(TextField), 'PUBLIC_INPUT_MARKER');
      t.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
      await t.pumpAndSettle();
      expect(find.byType(TextField), findsNothing);
      t.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
      await t.pumpAndSettle();
      await tapText(t, '복구 문구로 복구');
      expect(
        t.widget<TextField>(find.byType(TextField)).controller!.text,
        isEmpty,
      );
      expect(c.recoveries, 0);
    },
  );
  testWidgets(
    'confirmation keyboard and semantics follow actual requested positions',
    (t) async {
      final c = ScreenClient();
      await mountAccount(t, c);
      await tapText(t, '새 계정 만들기');
      await tapText(t, '적어 두었어요');
      final h = t.ensureSemantics();
      final fields = t.widgetList<TextField>(find.byType(TextField)).toList();
      expect(fields.map((f) => f.decoration!.labelText), [
        '1번째 단어',
        '9번째 단어',
        '24번째 단어',
      ]);
      await t.tap(find.byType(TextField).first);
      await t.pump();
      final first = t
          .widgetList<EditableText>(find.byType(EditableText))
          .toList();
      expect(first[0].focusNode.hasFocus, isTrue);
      await t.sendKeyEvent(LogicalKeyboardKey.tab);
      await t.pump();
      expect(first[1].focusNode.hasFocus, isTrue);
      await t.sendKeyEvent(LogicalKeyboardKey.tab);
      await t.pump();
      expect(first[2].focusNode.hasFocus, isTrue);
      final semantics = t
          .getSemantics(find.byType(Scaffold).last)
          .toStringDeep();
      expect(
        semantics.indexOf('1번째 단어'),
        lessThan(semantics.indexOf('9번째 단어')),
      );
      expect(
        semantics.indexOf('9번째 단어'),
        lessThan(semantics.indexOf('24번째 단어')),
      );
      h.dispose();
    },
  );

  testWidgets(
    'background during pending begin discards late words and cancels flow',
    (t) async {
      final c = ScreenClient()..pending = Completer<a.RecoveryDisplay>();
      final m = await mountAccount(t, c);
      await t.tap(find.text('새 계정 만들기'));
      await t.pump();
      t.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
      await t.pump();
      c.pending!.complete(c.display());
      await t.pumpAndSettle();
      expect(c.cancels, 1);
      expect(m.recoveryDisplay, isNull);
      expect(find.textContaining('public-marker'), findsNothing);
      t.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
      await t.pumpAndSettle();
      expect(find.text('새 계정 만들기'), findsOneWidget);
    },
  );

  testWidgets(
    'background cancels create and retains native concealment until resume',
    (t) async {
      final c = ScreenClient();
      final m = await mountAccount(t, c);
      final calls = <bool>[];
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
          .setMockMethodCallHandler(
            const MethodChannel('chat_app/account_storage'),
            (call) async {
              calls.add(call.arguments as bool);
              return null;
            },
          );
      await tapText(t, '새 계정 만들기');
      t.binding.handleAppLifecycleStateChanged(AppLifecycleState.inactive);
      await t.pumpAndSettle();
      expect(c.cancels, 1);
      expect(m.recoveryDisplay, isNull);
      expect(find.textContaining('public-marker'), findsNothing);
      expect(calls.last, isTrue);
      t.binding.handleAppLifecycleStateChanged(AppLifecycleState.resumed);
      await t.pumpAndSettle();
      expect(calls.last, isFalse);
      expect(find.textContaining('public-marker'), findsNothing);
      expect(find.text('새 계정 만들기'), findsOneWidget);
    },
  );
  testWidgets('native protection failure withholds sensitive child', (t) async {
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(
          const MethodChannel('chat_app/account_storage'),
          (_) async => throw PlatformException(code: 'PUBLIC_FAILURE'),
        );
    await t.pumpWidget(
      const MaterialApp(
        home: SensitiveScreen(child: Text('PUBLIC_HIDDEN_CHILD')),
      ),
    );
    await t.pumpAndSettle();
    expect(find.text('PUBLIC_HIDDEN_CHILD'), findsNothing);
    expect(find.textContaining('보호할 수 없어요'), findsOneWidget);
    await t.pumpWidget(const SizedBox());
    await t.pumpAndSettle();
  });
  testWidgets('overlapping sensitive routes release only last reference', (
    t,
  ) async {
    final calls = <bool>[];
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(
          const MethodChannel('chat_app/account_storage'),
          (call) async {
            calls.add(call.arguments as bool);
            return null;
          },
        );
    await t.pumpWidget(
      const MaterialApp(
        home: Column(
          children: [
            SensitiveScreen(key: ValueKey('a'), child: Text('a')),
            SensitiveScreen(key: ValueKey('b'), child: Text('b')),
          ],
        ),
      ),
    );
    await t.pumpAndSettle();
    await t.pumpWidget(
      const MaterialApp(
        home: Column(
          children: [SensitiveScreen(key: ValueKey('b'), child: Text('b'))],
        ),
      ),
    );
    await t.pumpAndSettle();
    expect(calls, isNot(contains(false)));
    await t.pumpWidget(const SizedBox());
    await t.pumpAndSettle();
    expect(calls.last, isFalse);
  });
  testWidgets(
    'pending begin duplicate and route disposal cancel exact late reply',
    (t) async {
      final c = ScreenClient()..pending = Completer<a.RecoveryDisplay>();
      final m = await mountAccount(t, c);
      await t.tap(find.text('새 계정 만들기'));
      await t.pump();
      await t.tap(find.text('새 계정 만들기'));
      await t.pump();
      expect(c.begins, 1);
      await t.pumpWidget(const SizedBox());
      c.pending!.complete(c.display());
      await t.pumpAndSettle();
      expect(c.cancels, 1);
      expect(m.recoveryDisplay, isNull);
      expect(c.controller.hasListener, isTrue);
    },
  );

  test(
    'late abandoned create cancels exact flow without clearing newer display',
    () async {
      final c = ScreenClient()..pending = Completer<a.RecoveryDisplay>();
      final m = AccountModel(c);
      await m.open();
      final pending = m.beginCreate();
      m.clearSecrets();
      c.pending!.complete(c.display());
      expect(await pending, isNull);
      expect(c.cancels, 1);
      expect(m.recoveryDisplay, isNull);
      c.pending = null;
      expect(await m.beginCreate(), isNotNull);
      expect(c.cancels, 1);
      expect(m.recoveryDisplay, isNotNull);
      m.dispose();
      await c.controller.close();
    },
  );

  testWidgets('new account: words, three positions, publish pending', (
    t,
  ) async {
    final c = ScreenClient();
    await mountAccount(t, c);
    await tapText(t, '새 계정 만들기');
    expect(find.text('1. public-marker-1'), findsOneWidget);
    await tapText(t, '적어 두었어요');
    for (var i = 0; i < 3; i++) {
      await t.enterText(
        find.byType(TextField).at(i),
        ['public-marker-1', 'public-marker-9', 'public-marker-24'][i],
      );
    }
    await tapText(t, '확인하고 계정 만들기');
    expect(c.answers, [
      'public-marker-1',
      'public-marker-9',
      'public-marker-24',
    ]);
    expect(find.text('공개 목록 반영을 기다리고 있어요.'), findsOneWidget);
  });
  testWidgets('word error and save failure never imply success', (t) async {
    final c = ScreenClient()..failure = r.ErrorCode.invalidRecovery;
    await mountAccount(t, c);
    await tapText(t, '새 계정 만들기');
    await tapText(t, '적어 두었어요');
    await tapText(t, '확인하고 계정 만들기');
    expect(find.text('복구 문구의 단어와 순서를 확인해 주세요.'), findsOneWidget);
    c.failure = r.ErrorCode.storageCorrupt;
    await tapText(t, '확인하고 계정 만들기');
    expect(find.textContaining('계정 저장 정보를 열 수 없어요'), findsOneWidget);
    expect(find.text('공개 목록 반영을 기다리고 있어요.'), findsNothing);
  });
  testWidgets('confirmation cancel calls exact flow', (t) async {
    final c = ScreenClient();
    final m = await mountAccount(t, c);
    await tapText(t, '새 계정 만들기');
    await tapText(t, '적어 두었어요');
    await tapText(t, '취소');
    expect(c.cancels, 1);
    expect(m.recoveryDisplay, isNull);
    expect(find.text('새 계정 만들기'), findsOneWidget);
  });
  testWidgets('recover warns before input; contacts are not restored', (
    t,
  ) async {
    final c = ScreenClient();
    await mountAccount(t, c);
    await tapText(t, '복구 문구로 복구');
    expect(find.text('계정 ID는 돌아오지만 내 연락처와 대화 기록은 돌아오지 않아요'), findsOneWidget);
    expect(find.textContaining('침해된 기기'), findsOneWidget);
    await t.enterText(find.byType(TextField), 'PUBLIC_INVALID_RECOVERY');
    await tapText(t, '기존 기기 교체에 동의해요');
    await tapText(t, '계정 복구');
    expect(c.recoveries, 1);
    expect(c.saved, isEmpty);
    expect(find.text('공개 목록 반영을 기다리고 있어요.'), findsOneWidget);
  });
  testWidgets('active does not offer overwrite recovery', (t) async {
    final c = ScreenClient()..phase = a.AccountPhase.active;
    await mountAccount(t, c);
    expect(find.text('계정 복구'), findsNothing);
    expect(find.text('새 계정 만들기'), findsNothing);
    expect(c.recoveries, 0);
  });
  testWidgets('mnemonic semantics excluded and 200 percent keyboard layout', (
    t,
  ) async {
    final c = ScreenClient();
    await mountAccount(t, c, scale: 2);
    await tapText(t, '새 계정 만들기');
    final h = t.ensureSemantics();
    expect(
      t.getSemantics(find.byType(Scaffold).last).toStringDeep(),
      isNot(contains('public-marker')),
    );
    await tapText(t, '적어 두었어요');
    await t.sendKeyEvent(LogicalKeyboardKey.tab);
    expect(FocusManager.instance.primaryFocus, isNotNull);
    await tapText(t, '취소');
    expect(t.takeException(), isNull);
    h.dispose();
  });
}
