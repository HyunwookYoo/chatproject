import 'dart:async';
import 'package:chat_app/account/account_model.dart';
import 'package:chat_app/account/account_routes.dart';
import 'package:chat_app/account/invite_links.dart';
import 'package:chat_app/account/public_id.dart';
import 'package:chat_app/account/screens/pair_screen.dart';
import 'package:chat_app/account/screens/devices_screen.dart';
import 'package:chat_app/bootstrap.dart';
import 'package:chat_app/account/sensitive_screen.dart';
import 'package:flutter/scheduler.dart';
import 'package:chat_app/src/rust/api/chat.dart' as r;
import 'package:chat_app/src/rust/api/account.dart' as a;
import 'package:flutter/material.dart';
import 'package:flutter/foundation.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'account_onboarding_test.dart' show ScreenClient;

class PairClient extends ScreenClient {
  final pairReply = Completer<a.PairDisplay>();
  final pairCanceled = <List<int>>[];
  @override
  Future<a.PairDisplay> pairBegin(
    a.PairDirection direction,
    Uint8List? resume,
  ) => pairReply.future;
  @override
  Future<void> pairCancel(List<int> flow) async {
    pairCanceled.add(List.of(flow));
  }
}

void main() {
  test(
    'canonical public installation id is 52 lowercase base32 characters',
    () {
      final encoded = publicId(List.filled(32, 7));
      expect(encoded, 'a4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4dq');
      expect(encoded.length, 52);
      expect(publicId(List.filled(32, 255)), '${List.filled(51, '7').join()}q');
    },
  );
  expandedTests();
  setUp(() {
    TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
        .setMockMethodCallHandler(
          const MethodChannel('chat_app/account_storage'),
          (_) async => null,
        );
  });
  testWidgets('welcome enters pairing with the app model', (tester) async {
    final client = PairClient();
    final model = AccountModel(client);
    await model.open();
    await tester.pumpWidget(
      MaterialApp(
        home: AccountRoutes(model: model, child: const SizedBox()),
      ),
    );
    await tester.tap(find.text('다른 기기로 연결'));
    await tester.pumpAndSettle();
    expect(find.text('QR 표시'), findsOneWidget);
    await tester.pumpWidget(const SizedBox());
    await tester.pumpAndSettle();
    model.dispose();
    addTearDown(() => client.controller.close());
  });
  testWidgets('active account has a device management entry', (tester) async {
    final client = PairClient()..phase = a.AccountPhase.active;
    final model = AccountModel(client);
    await model.open();
    await tester.pumpWidget(
      MaterialApp(
        home: AccountRoutes(model: model, child: const SizedBox()),
      ),
    );
    expect(find.text('내 기기'), findsOneWidget);
    await tester.pumpWidget(const SizedBox());
    await tester.pumpAndSettle();
    model.dispose();
    addTearDown(() => client.controller.close());
  });
  test('late abandoned pair begin cancels its exact flow', () async {
    final client = PairClient();
    final model = AccountModel(client);
    await model.open();
    final pending = model.pairBegin(a.PairDirection.newDisplays);
    model.clearSecrets();
    final flow = Uint8List.fromList(List.filled(16, 4));
    client.pairReply.complete(
      a.PairDisplay(
        flow: flow,
        qr: 'PUBLIC_INVALID_PAIR_FIXTURE',
        expiresAt: BigInt.from(9999999999),
      ),
    );
    expect(await pending, isNull);
    expect(client.pairCanceled, [flow]);
    await model.close();
    await client.controller.close();
  });
}

class LinkClient extends ScreenClient {
  final parsed = <String>[];
  @override
  Future<a.InvitePreview> parseInvite(String input, BigInt now) async {
    parsed.add(input);
    return super.parseInvite(input, now);
  }
}

class UiPairClient extends ScreenClient {
  a.PairView? pairing;
  a.PairRole role = a.PairRole.existing;
  a.PairDirection? beganDirection;
  Uint8List? beganResume, scanTransaction;
  bool disclosed = false, remoteRevoke = false, expired = false;
  int approvals = 0, matches = 0, revokes = 0, discards = 0, refreshes = 0;
  List<a.DeviceView> devices = [];
  List<a.PairResumeView> resumes = [];
  Completer<void>? commandWait;
  Completer<a.PairDisplay>? beginWait;
  final canceled = <List<int>>[];
  @override
  a.AccountView view() {
    final base = super.view();
    return a.AccountView(
      revision: base.revision,
      phase: phase,
      publicPeerId: base.publicPeerId,
      installation: Uint8List(32),
      devices: devices,
      contacts: [],
      pair: pairing,
      publishIntents: [],
      publication: base.publication,
      disclosurePossible: disclosed,
      pendingPairTransactions: resumes.map((e) => e.transaction).toList(),
      pairResumes: resumes,
      pendingImportRequiresRemoteRevoke: remoteRevoke,
    );
  }

  void paired(a.PairPhase phase, a.PairDirection direction) {
    pairing = a.PairView(
      flow: flow,
      direction: direction,
      localRole: role,
      remoteInstallation: Uint8List.fromList(List.filled(32, 7)),
      remoteKind: a.DeviceKind.mobile,
      transaction: beganResume ?? scanTransaction,
      phase: phase,
      sas: 'ABCD-EFGH',
      expiresAt: BigInt.from(9999999999),
      disclosurePossible: disclosed,
    );
    emit();
  }

  @override
  Future<a.PairDisplay> pairBegin(
    a.PairDirection direction,
    Uint8List? resume,
  ) async {
    if (beginWait case final Completer<a.PairDisplay> wait) return wait.future;
    beganDirection = direction;
    beganResume = resume;
    paired(a.PairPhase.comparing, direction);
    return a.PairDisplay(
      flow: flow,
      qr: 'PUBLIC_INVALID_PAIR_FIXTURE',
      expiresAt: BigInt.from(expired ? 0 : 9999999999),
    );
  }

  @override
  Future<Uint8List> pairScan(String input) async {
    await commandWait?.future;
    paired(a.PairPhase.comparing, a.PairDirection.existingDisplays);
    return flow;
  }

  @override
  Future<void> pairConfirmCode(List<int> flow, bool value) async {
    await commandWait?.future;
    matches++;
    check();
  }

  @override
  Future<void> pairApprove(List<int> flow) async {
    await commandWait?.future;
    approvals++;
    disclosed = true;
    paired(a.PairPhase.disclosurePossible, beganDirection!);
  }

  @override
  Future<void> pairCancel(List<int> flow) async {
    canceled.add(List.of(flow));
    await cancelWait?.future;
    check();
    if (listEquals(pairing?.flow, flow)) pairing = null;
    emit();
  }

  @override
  Future<void> revokeDevice(List<int> id) async {
    revokes++;
    check();
  }

  @override
  Future<void> discardPendingImport() async {
    discards++;
    check();
    phase = a.AccountPhase.unconfigured;
    resumes = [];
    remoteRevoke = false;
    emit();
  }

  @override
  Future<void> refreshRecord(List<int> id) async {
    refreshes++;
  }
}

Future<AccountModel> mountPair(
  WidgetTester t,
  UiPairClient c, {
  a.PairResumeView? resume,
  bool devices = false,
  bool locked = false,
  bool pushed = false,
  double scale = 1,
}) async {
  final model = AccountModel(c);
  await model.open();
  await t.pumpWidget(
    locked
        ? AccountGate(
            model: model,
            child: const MaterialApp(home: Text('UNLOCKED')),
          )
        : MaterialApp(
            builder: (context, child) => MediaQuery(
              data: MediaQuery.of(
                context,
              ).copyWith(textScaler: TextScaler.linear(scale)),
              child: child!,
            ),
            home: devices
                ? DevicesScreen(model: model, onPair: (_) {})
                : pushed
                ? Builder(
                    builder: (context) => Scaffold(
                      body: TextButton(
                        onPressed: () => Navigator.of(context).push(
                          MaterialPageRoute<void>(
                            builder: (_) =>
                                PairScreen(model: model, resume: resume),
                          ),
                        ),
                        child: const Text('OPEN PAIR'),
                      ),
                    ),
                  )
                : PairScreen(model: model, resume: resume),
          ),
  );
  await t.pumpAndSettle();
  if (pushed) await hit(t, 'OPEN PAIR');
  addTearDown(() async {
    await t.pumpWidget(const SizedBox());
    await t.pumpAndSettle();
    if (!locked) model.dispose();
    await c.controller.close();
  });
  return model;
}

Future<void> hit(WidgetTester t, String text) async {
  await t.ensureVisible(find.text(text).last);
  await t.tap(find.text(text).last);
  await t.pumpAndSettle();
}

Future<void> keyboardActivate(
  WidgetTester t,
  String label, {
  bool last = false,
}) async {
  final target = last ? find.text(label).last : find.text(label);
  await t.ensureVisible(target);
  for (var i = 0; i < 40 && !Focus.of(t.element(target)).hasFocus; i++) {
    await t.sendKeyEvent(LogicalKeyboardKey.tab);
    await t.pumpAndSettle();
  }
  expect(
    Focus.of(t.element(target)).hasFocus,
    isTrue,
    reason: '$label reachable by Tab',
  );
  await t.sendKeyEvent(LogicalKeyboardKey.enter);
  await t.pumpAndSettle();
}

void expandedTests() {
  testWidgets(
    'successful SAS rejection clears owned cached and visible QR through disposal',
    (t) async {
      final c = UiPairClient()..phase = a.AccountPhase.active;
      final model = await mountPair(t, c, pushed: true);
      await hit(t, 'QR 표시');
      expect(model.pairDisplay, isNotNull);
      expect(find.text('PUBLIC_INVALID_PAIR_FIXTURE'), findsOneWidget);
      await hit(t, '코드가 달라요');
      expect(c.matches, 1);
      expect(model.pairDisplay, isNull);
      expect(find.text('PUBLIC_INVALID_PAIR_FIXTURE'), findsNothing);
      expect(find.text('ABCD-EFGH'), findsNothing);
      expect(c.canceled, isEmpty);
      await t.tap(find.byTooltip('취소'));
      await t.pumpAndSettle();
      await hit(t, '연결 취소');
      expect(find.byType(PairScreen), findsNothing);
      expect(find.text('OPEN PAIR'), findsOneWidget);
      expect(model.pairDisplay, isNull);
      expect(find.text('PUBLIC_INVALID_PAIR_FIXTURE'), findsNothing);
      expect(c.canceled, isEmpty);
    },
  );
  for (final role in a.PairRole.values) {
    testWidgets(
      'close warning covers delayed ${role.name} consent and disclosure',
      (t) async {
        final c = UiPairClient()
          ..phase = a.AccountPhase.active
          ..role = role;
        await mountPair(t, c, pushed: true);
        await hit(t, 'QR 표시');
        if (role == a.PairRole.existing) {
          c.paired(a.PairPhase.awaitingApproval, c.beganDirection!);
          await t.pumpAndSettle();
        }
        c.commandWait = Completer<void>();
        await hit(t, role == a.PairRole.existing ? '이 기기 연결 승인' : '양쪽 코드가 같아요');
        await t.tap(find.byTooltip('취소'));
        await t.pumpAndSettle();
        final warning = find.descendant(
          of: find.byType(AlertDialog),
          matching: find.textContaining('원격 사본이 지워졌다고'),
        );
        expect(warning, findsOneWidget);
        expect(find.textContaining('키 전달 전 연결을 취소'), findsNothing);
        c.commandWait!.complete();
        await t.pumpAndSettle();
        expect(warning, findsOneWidget);
        expect(c.canceled, isEmpty);
        await hit(t, '연결 취소');
        expect(c.canceled, [c.flow]);
        expect(find.text('OPEN PAIR'), findsOneWidget);
      },
    );
  }
  testWidgets('open close warning follows newly projected disclosure', (
    t,
  ) async {
    final c = UiPairClient()..phase = a.AccountPhase.active;
    await mountPair(t, c);
    await t.tap(find.byTooltip('취소'));
    await t.pumpAndSettle();
    c.disclosed = true;
    c.emit();
    await t.pumpAndSettle();
    expect(
      find.descendant(
        of: find.byType(AlertDialog),
        matching: find.textContaining('원격 사본이 지워졌다고'),
      ),
      findsOneWidget,
    );
  });
  testWidgets(
    'failed close stays visible and retries exact old flow without replacement',
    (t) async {
      final c = UiPairClient()..phase = a.AccountPhase.active;
      final model = await mountPair(t, c, pushed: true);
      await hit(t, 'QR 표시');
      c.failure = r.ErrorCode.notAvailable;
      c.cancelWait = Completer<void>();
      await t.tap(find.byTooltip('취소'));
      await t.pumpAndSettle();
      await hit(t, '연결 취소');
      expect(find.byType(PairScreen), findsOneWidget);
      expect(find.text('PUBLIC_INVALID_PAIR_FIXTURE'), findsNothing);
      // Another owner publishes a new flow while the old cancellation is pending.
      final replacement = a.PairView(
        flow: Uint8List.fromList(List.filled(16, 99)),
        direction: a.PairDirection.newDisplays,
        localRole: a.PairRole.new_,
        phase: a.PairPhase.comparing,
        sas: 'OTHER-FLOW',
        expiresAt: BigInt.from(9999999999),
        disclosurePossible: false,
      );
      c.pairing = replacement;
      c.emit();
      c.cancelWait!.complete();
      await t.pumpAndSettle();
      expect(find.byType(PairScreen), findsOneWidget);
      expect(find.textContaining('연결 취소를 확인하지 못했어요'), findsOneWidget);
      expect(find.text('QR 표시'), findsNothing);
      expect(find.text('OTHER-FLOW'), findsNothing);
      expect(model.error, isNull);
      c.failure = null;
      c.beginWait = Completer<a.PairDisplay>();
      final replacementBegin = model.pairBegin(a.PairDirection.newDisplays);
      await t.tap(find.byTooltip('취소'));
      await t.pumpAndSettle();
      await hit(t, '연결 취소');
      expect(c.canceled, [c.flow, c.flow]);
      expect(c.pairing, same(replacement));
      expect(model.snapshot!.pair!.flow, replacement.flow);
      expect(find.text('OPEN PAIR'), findsOneWidget);
      expect(model.error, isNull);
      c.beginWait!.complete(
        a.PairDisplay(
          flow: replacement.flow,
          qr: 'PUBLIC_INVALID_REPLACEMENT_FIXTURE',
          expiresAt: BigInt.from(9999999999),
        ),
      );
      expect(await replacementBegin, isNotNull);
      expect(c.canceled, [c.flow, c.flow]);
    },
  );
  testWidgets(
    'confirmed close waits for pending approval then reports cancel failure and retries',
    (t) async {
      final c = UiPairClient()..phase = a.AccountPhase.active;
      await mountPair(t, c, pushed: true);
      await hit(t, 'QR 표시');
      c.paired(a.PairPhase.awaitingApproval, c.beganDirection!);
      await t.pumpAndSettle();
      c.commandWait = Completer<void>();
      c.failure = r.ErrorCode.notAvailable;
      await hit(t, '이 기기 연결 승인');
      await t.tap(find.byTooltip('취소'));
      await t.pumpAndSettle();
      await hit(t, '연결 취소');
      expect(c.canceled, isEmpty);
      expect(find.byType(PairScreen), findsOneWidget);
      expect(find.text('QR 표시'), findsNothing);
      expect(find.text('PUBLIC_INVALID_PAIR_FIXTURE'), findsNothing);
      await t.tap(find.byTooltip('취소'));
      await t.pumpAndSettle();
      expect(find.byType(AlertDialog), findsNothing);
      c.commandWait!.complete();
      await t.pumpAndSettle();
      expect(c.approvals, 1);
      expect(c.disclosed, isTrue);
      expect(c.canceled, [c.flow]);
      expect(find.textContaining('연결 취소를 확인하지 못했어요'), findsOneWidget);
      expect(find.textContaining('원격 사본이 지워졌다고'), findsOneWidget);
      c.failure = null;
      c.cancelWait = Completer<void>();
      await hit(t, '연결 취소 다시 시도');
      expect(find.text('QR 표시'), findsNothing);
      await t.tap(find.byTooltip('취소'));
      await t.pumpAndSettle();
      await hit(t, '연결 취소');
      expect(c.canceled, [c.flow, c.flow]); // Close shares the in-flight retry.
      expect(find.byType(PairScreen), findsOneWidget);
      c.cancelWait!.complete();
      await t.pumpAndSettle();
      expect(c.canceled, [c.flow, c.flow]);
      expect(find.text('OPEN PAIR'), findsOneWidget);
    },
  );
  testWidgets(
    'pair and close dialog at 200 percent remain keyboard actionable',
    (t) async {
      t.view.physicalSize = const Size(360, 640);
      t.view.devicePixelRatio = 1;
      addTearDown(t.view.resetPhysicalSize);
      addTearDown(t.view.resetDevicePixelRatio);
      final c = UiPairClient()..phase = a.AccountPhase.active;
      await mountPair(t, c, scale: 2, pushed: true);
      await keyboardActivate(t, 'QR 표시');
      expect(find.text('ABCD-EFGH'), findsOneWidget);
      await keyboardActivate(t, '양쪽 코드가 같아요');
      expect(c.matches, 1);
      c.disclosed = true;
      c.emit();
      await t.pumpAndSettle();
      await t.tap(find.byTooltip('취소'));
      await t.pumpAndSettle();
      expect(t.takeException(), isNull);
      await keyboardActivate(t, '계속');
      expect(find.byType(AlertDialog), findsNothing);
      expect(c.canceled, isEmpty);
    },
  );
  for (final pending in [false, true]) {
    testWidgets(
      'devices and warning at 200 percent keyboard pending=$pending',
      (t) async {
        t.view.physicalSize = const Size(360, 640);
        t.view.devicePixelRatio = 1;
        addTearDown(t.view.resetPhysicalSize);
        addTearDown(t.view.resetDevicePixelRatio);
        final c = UiPairClient()
          ..phase = pending
              ? a.AccountPhase.pendingImport
              : a.AccountPhase.active;
        c.devices = List.generate(
          2,
          (i) => a.DeviceView(
            installationId: Uint8List.fromList(List.filled(32, i)),
            transportPublicKey: Uint8List(32),
            kind: a.DeviceKind.desktop,
          ),
        );
        await mountPair(t, c, devices: true, scale: 2);
        expect(t.takeException(), isNull);
        for (final device in c.devices) {
          final id = find.textContaining(publicId(device.installationId));
          await t.ensureVisible(id);
          await t.pumpAndSettle();
          expect(t.getSize(id).width, lessThanOrEqualTo(312));
          final tile = find.ancestor(of: id, matching: find.byType(ListTile));
          final title = find.descendant(
            of: tile,
            matching: find.textContaining('데스크톱'),
          );
          expect(t.getRect(title).bottom, lessThanOrEqualTo(t.getRect(id).top));
          expect(
            t.getRect(id).bottom,
            lessThanOrEqualTo(t.getRect(tile).bottom),
          );
        }
        await keyboardActivate(t, pending ? '중단된 연결 버리기' : '해제', last: true);
        expect(t.takeException(), isNull);
        expect(find.byType(AlertDialog), findsOneWidget);
        await keyboardActivate(t, pending ? '로컬 연결 버리기' : '기기 해제');
        expect(pending ? c.discards : c.revokes, 1);
        expect(find.byType(AlertDialog), findsNothing);
        expect(t.takeException(), isNull);
      },
    );
  }
  testWidgets(
    'foreground release waits for later raster and ignores stale timing',
    (t) async {
      final calls = <bool>[];
      var reads = 0;
      SensitiveScreen.frameNumberForTesting = () => reads++ == 0 ? 10 : 12;
      addTearDown(() => SensitiveScreen.frameNumberForTesting = null);
      TestDefaultBinaryMessengerBinding.instance.defaultBinaryMessenger
          .setMockMethodCallHandler(
            const MethodChannel('chat_app/account_storage'),
            (call) async {
              if (call.method == 'setSensitiveScreen') {
                calls.add(call.arguments as bool);
              }
              return null;
            },
          );
      await t.pumpWidget(
        const MaterialApp(home: SensitiveScreen(child: Text('PUBLIC FRAME'))),
      );
      await t.pumpAndSettle();
      await t.pumpWidget(const SizedBox());
      await t.pumpAndSettle();
      expect(calls, isNot(contains(false)));
      void timing(int frame) =>
          t.binding.platformDispatcher.onReportTimings?.call([
            FrameTiming(
              vsyncStart: 0,
              buildStart: 1,
              buildFinish: 2,
              rasterStart: 3,
              rasterFinish: 4,
              rasterFinishWallTime: 5,
              frameNumber: frame,
            ),
          ]);
      timing(10);
      await t.pump();
      expect(calls, isNot(contains(false)));
      timing(11); // Delayed intermediate timing after replacement was built.
      await t.pump();
      expect(calls, isNot(contains(false)));
      timing(12);
      await t.pump();
      expect(calls.last, isFalse);
    },
  );
  testWidgets(
    'public native link queues until configured then previews before explicit save',
    (t) async {
      const payload = 'chatproject:identity-invite/1/PUBLIC_INVALID_FIXTURE';
      InviteLinks.initial = payload;
      final c = LinkClient();
      final m = AccountModel(c);
      await m.open();
      await t.pumpWidget(
        MaterialApp(
          home: AccountRoutes(model: m, child: const SizedBox()),
        ),
      );
      await t.pumpAndSettle();
      expect(c.parsed, isEmpty);
      c.phase = a.AccountPhase.active;
      c.emit();
      await t.pumpAndSettle();
      expect(c.parsed, [payload]);
      expect(find.text('연락처 이름 확인'), findsOneWidget);
      expect(c.accepts, 0);
      await hit(t, '연락처 추가');
      expect(c.accepts, 1);
      const codec = StandardMethodCodec();
      await t.binding.defaultBinaryMessenger.handlePlatformMessage(
        'chat_app/account_storage',
        codec.encodeMethodCall(
          const MethodCall(
            'identityInvite',
            'chatproject:pair/1/PUBLIC_INVALID',
          ),
        ),
        (_) {},
      );
      await t.pumpAndSettle();
      expect(c.parsed, [payload]);
      await t.binding.defaultBinaryMessenger.handlePlatformMessage(
        'chat_app/account_storage',
        codec.encodeMethodCall(const MethodCall('identityInvite', payload)),
        (_) {},
      );
      await t.pumpAndSettle();
      expect(c.parsed, [payload, payload]);
      expect(c.accepts, 1);
      await t.pumpWidget(const SizedBox());
      await t.pumpAndSettle();
      m.dispose();
      addTearDown(() => c.controller.close());
    },
  );
  testWidgets(
    'negative frame identity fails closed and new owner invalidates release',
    (t) async {
      final calls = <bool>[];
      var frame = -1;
      SensitiveScreen.frameNumberForTesting = () => frame < 0 ? -1 : frame++;
      addTearDown(() => SensitiveScreen.frameNumberForTesting = null);
      t.binding.defaultBinaryMessenger.setMockMethodCallHandler(
        const MethodChannel('chat_app/account_storage'),
        (call) async {
          if (call.method == 'setSensitiveScreen') {
            calls.add(call.arguments as bool);
          }
          return null;
        },
      );
      Future<void> show(String label) async {
        await t.pumpWidget(
          MaterialApp(
            home: SensitiveScreen(key: ValueKey(label), child: Text(label)),
          ),
        );
        await t.pumpAndSettle();
      }

      void timing(int id) =>
          t.binding.platformDispatcher.onReportTimings?.call([
            FrameTiming(
              vsyncStart: 0,
              buildStart: 1,
              buildFinish: 2,
              rasterStart: 3,
              rasterFinish: 4,
              rasterFinishWallTime: 5,
              frameNumber: id,
            ),
          ]);
      await show('PUBLIC A');
      await t.pumpWidget(const SizedBox());
      await t.pumpAndSettle();
      timing(100);
      await t.pump();
      expect(calls, isNot(contains(false)));
      frame = 20;
      await show('PUBLIC B');
      await t.pumpWidget(const SizedBox());
      await t.pumpAndSettle();
      await show('PUBLIC C');
      timing(21);
      await t.pump();
      expect(calls, isNot(contains(false)));
      await t.pumpWidget(const SizedBox());
      await t.pumpAndSettle();
      timing(23);
      await t.pump();
      expect(calls.last, isFalse);
    },
  );
  testWidgets(
    'disposal during local confirmation cancels exact flow when command settles',
    (t) async {
      final c = UiPairClient()..phase = a.AccountPhase.active;
      await mountPair(t, c);
      await hit(t, 'QR 표시');
      c.commandWait = Completer<void>();
      await hit(t, '양쪽 코드가 같아요');
      await t.pumpWidget(const SizedBox());
      expect(c.canceled, isEmpty);
      c.commandWait!.complete();
      await t.pumpAndSettle();
      expect(c.matches, 1);
      expect(c.canceled, [c.flow]);
      expect(c.approvals, 0);
    },
  );
  testWidgets(
    'replacement public pair cannot display old route SAS or metadata',
    (t) async {
      final c = UiPairClient()..phase = a.AccountPhase.active;
      await mountPair(t, c);
      await hit(t, 'QR 표시');
      c.pairing = a.PairView(
        flow: Uint8List.fromList(List.filled(16, 99)),
        direction: a.PairDirection.existingDisplays,
        localRole: a.PairRole.existing,
        phase: a.PairPhase.comparing,
        sas: 'OTHER-FLOW',
        remoteInstallation: Uint8List(32),
        remoteKind: a.DeviceKind.desktop,
        expiresAt: BigInt.from(9999999999),
        disclosurePossible: false,
      );
      c.emit();
      await t.pumpAndSettle();
      expect(find.text('ABCD-EFGH'), findsNothing);
      expect(find.text('OTHER-FLOW'), findsNothing);
      expect(find.textContaining('상대 기기 유형:'), findsNothing);
    },
  );
  for (final role in a.PairRole.values) {
    testWidgets(
      'pair ${role.name} same SAS local match never auto approves or completes',
      (t) async {
        final c = UiPairClient()
          ..role = role
          ..phase = role == a.PairRole.existing
              ? a.AccountPhase.active
              : a.AccountPhase.candidate;
        await mountPair(t, c);
        await hit(t, 'QR 표시');
        expect(
          c.beganDirection,
          role == a.PairRole.existing
              ? a.PairDirection.existingDisplays
              : a.PairDirection.newDisplays,
        );
        expect(find.text('ABCD-EFGH'), findsOneWidget);
        expect(find.text('상대 기기 유형: 모바일'), findsOneWidget);
        expect(
          find.text(
            '상대 기기: a4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4dqobyha4dq',
          ),
          findsOneWidget,
        );
        expect(find.text('최신 목록에서 이 기기 활성 확인'), findsNothing);
        await hit(t, '양쪽 코드가 같아요');
        expect(c.matches, 1);
        expect(c.approvals, 0);
        if (role == a.PairRole.existing) {
          c.paired(a.PairPhase.awaitingApproval, c.beganDirection!);
          await t.pumpAndSettle();
          await hit(t, '이 기기 연결 승인');
          expect(c.approvals, 1);
          expect(find.text('최신 목록에서 이 기기 활성 확인'), findsNothing);
        } else {
          expect(find.text('이 기기 연결 승인'), findsNothing);
        }
      },
    );
  }
  for (final matching in [true, false]) {
    testWidgets(
      'selected resume scan validates exact public transaction matching=$matching',
      (t) async {
        final tx = Uint8List.fromList(List.filled(16, 8));
        final c = UiPairClient()
          ..role = a.PairRole.new_
          ..phase = a.AccountPhase.candidate
          ..scanTransaction = matching ? tx : Uint8List(16);
        await mountPair(
          t,
          c,
          resume: a.PairResumeView(transaction: tx, role: a.PairRole.new_),
        );
        await hit(t, '상대 QR 스캔 또는 붙여넣기');
        await t.enterText(
          find.byType(TextField),
          'PUBLIC_INVALID_RESUME_FIXTURE',
        );
        await hit(t, '입력 확인');
        if (matching) {
          expect(find.text('ABCD-EFGH'), findsOneWidget);
          expect(c.canceled, isEmpty);
          await hit(t, '양쪽 코드가 같아요');
          expect(c.matches, 1);
        } else {
          expect(find.text('ABCD-EFGH'), findsNothing);
          expect(c.canceled, [c.flow]);
          expect(c.matches, 0);
          expect(c.approvals, 0);
        }
      },
    );
  }
  testWidgets('Active N resume uses public role not Active phase', (t) async {
    final resume = a.PairResumeView(
      transaction: Uint8List(16),
      role: a.PairRole.new_,
    );
    final c = UiPairClient()
      ..phase = a.AccountPhase.active
      ..role = a.PairRole.new_;
    await mountPair(t, c, resume: resume);
    await hit(t, 'QR 표시');
    expect(c.beganDirection, a.PairDirection.newDisplays);
    expect(c.beganResume, resume.transaction);
    expect(c.matches, 0);
    expect(c.approvals, 0);
  });
  for (final disclosed in [false, true]) {
    testWidgets('cancel warning disclosure=$disclosed without current flow', (
      t,
    ) async {
      final c = UiPairClient()
        ..phase = a.AccountPhase.active
        ..disclosed = disclosed;
      await mountPair(t, c);
      await t.tap(find.byTooltip('취소'));
      await t.pumpAndSettle();
      expect(
        find.textContaining(disclosed ? '원격 사본이 지워졌다고' : '연결 취소를 요청해요'),
        findsWidgets,
      );
      expect(c.canceled, isEmpty);
    });
  }
  testWidgets('expired display cleared and exact flow canceled', (t) async {
    final c = UiPairClient()
      ..phase = a.AccountPhase.active
      ..expired = true;
    await mountPair(t, c);
    await hit(t, 'QR 표시');
    await t.pump(const Duration(seconds: 2));
    await t.pumpAndSettle();
    expect(find.text('PUBLIC_INVALID_PAIR_FIXTURE'), findsNothing);
    expect(c.canceled, [c.flow]);
    expect(find.textContaining('QR이 만료'), findsOneWidget);
  });
  testWidgets(
    'device cap current and last revoke denied; target warning explicit',
    (t) async {
      final c = UiPairClient()..phase = a.AccountPhase.active;
      c.devices = List.generate(
        5,
        (i) => a.DeviceView(
          installationId: Uint8List.fromList(List.filled(32, i)),
          transportPublicKey: Uint8List(32),
          kind: a.DeviceKind.desktop,
        ),
      );
      await mountPair(t, c, devices: true);
      expect(
        t
            .widget<FilledButton>(find.widgetWithText(FilledButton, '기기 연결'))
            .onPressed,
        isNull,
      );
      expect(
        t
            .widget<TextButton>(find.widgetWithText(TextButton, '해제').first)
            .onPressed,
        isNull,
      );
      await t.ensureVisible(find.widgetWithText(TextButton, '해제').at(1));
      await t.tap(find.widgetWithText(TextButton, '해제').at(1));
      await t.pumpAndSettle();
      expect(c.revokes, 0);
      expect(find.textContaining('해제할 기기:'), findsOneWidget);
      await hit(t, '기기 해제');
      expect(c.revokes, 1);
      c.devices = [
        c.devices[1],
      ]; // Last remaining device is not the current one.
      c.emit();
      await t.pumpAndSettle();
      expect(
        t.widget<TextButton>(find.widgetWithText(TextButton, '해제')).onPressed,
        isNull,
      );
    },
  );
  testWidgets('pending discard failure stays pending and refresh is explicit', (
    t,
  ) async {
    final c = UiPairClient()
      ..phase = a.AccountPhase.pendingImport
      ..remoteRevoke = true
      ..failure = r.ErrorCode.notAvailable;
    final model = await mountPair(t, c, devices: true);
    expect(c.refreshes, 0);
    await hit(t, '중단된 연결 버리기');
    await hit(t, '로컬 연결 버리기');
    expect(c.discards, 1);
    expect(model.snapshot!.phase, a.AccountPhase.pendingImport);
    expect(model.error, isNotNull);
    expect(find.text('중단된 연결 버리기'), findsOneWidget);
    await hit(t, '목록 다시 확인');
    expect(c.refreshes, 1);
  });
  testWidgets('Locked pending gate exposes explicit retry discard', (t) async {
    final c = UiPairClient()
      ..phase = a.AccountPhase.locked
      ..remoteRevoke = true
      ..failure = r.ErrorCode.storageLocked;
    final model = await mountPair(t, c, locked: true);
    await hit(t, '중단된 연결 상태 확인');
    await hit(t, '중단된 연결 버리기');
    await hit(t, '로컬 연결 버리기');
    expect(model.snapshot!.phase, a.AccountPhase.locked);
    expect(c.discards, 1);
    c.failure = null;
    await hit(t, '중단된 연결 버리기');
    await hit(t, '로컬 연결 버리기');
    expect(model.snapshot!.phase, a.AccountPhase.unconfigured);
    expect(c.discards, 2);
  });
  testWidgets('disposal during approval cancels exact flow after disclosure', (
    t,
  ) async {
    final c = UiPairClient()..phase = a.AccountPhase.active;
    await mountPair(t, c);
    await hit(t, 'QR 표시');
    c.paired(a.PairPhase.awaitingApproval, c.beganDirection!);
    await t.pumpAndSettle();
    c.commandWait = Completer<void>();
    await hit(t, '이 기기 연결 승인');
    await t.pumpWidget(const SizedBox());
    c.commandWait!.complete();
    await t.pumpAndSettle();
    expect(c.canceled, [c.flow]);
    expect(c.disclosed, isTrue);
  });
  test(
    'abandoned late scan returns no flow and cancels exact core reply',
    () async {
      final c = UiPairClient()..commandWait = Completer<void>();
      final m = AccountModel(c);
      await m.open();
      final future = m.pairScan('PUBLIC_INVALID_PAIR_FIXTURE');
      m.clearSecrets();
      c.commandWait!.complete();
      expect(await future, isNull);
      expect(c.canceled, [c.flow]);
      m.dispose();
      await c.controller.close();
    },
  );
}
