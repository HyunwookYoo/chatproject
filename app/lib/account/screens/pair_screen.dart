import 'dart:async';
import 'package:flutter/material.dart';
import 'package:flutter/foundation.dart';
import '../../src/rust/api/account.dart' as a;
import '../account_model.dart';
import '../qr_input.dart';
import '../public_id.dart';
import '../sensitive_screen.dart';
import 'device_warning_screen.dart';

class PairScreen extends StatefulWidget {
  const PairScreen({super.key, required this.model, this.resume});
  final AccountModel model;
  final a.PairResumeView? resume;
  @override
  State<PairScreen> createState() => _PairScreenState();
}

class _PairScreenState extends State<PairScreen> with WidgetsBindingObserver {
  a.PairDisplay? _display;
  List<int>? _flow;
  List<int>? _cancelFlow;
  Future<bool>? _cancellation;
  Future<Object?>? _pending;
  int _ticket = 0, _input = 0;
  bool _scan = false, _confirmed = false, _leaving = false;
  bool _hidden = false;
  bool _consentMayDisclose = false, _closing = false;
  int _protectionTicket = 0;
  String? _error;
  Timer? _expiry;
  AccountModel get model => widget.model;
  late final a.PairRole _entryRole;
  a.PairRole get _role => _pair?.localRole ?? _entryRole;
  a.PairView? get _pair {
    final pair = model.snapshot?.pair;
    return pair != null && listEquals(pair.flow, _flow) ? pair : null;
  }

  bool get _disclosed =>
      _consentMayDisclose ||
      model.snapshot?.disclosurePossible == true ||
      model.snapshot?.pendingImportRequiresRemoteRevoke == true ||
      _pair?.disclosurePossible == true;
  @override
  void initState() {
    super.initState();
    _entryRole =
        widget.resume?.role ??
        ([
              a.AccountPhase.unconfigured,
              a.AccountPhase.candidate,
            ].contains(model.snapshot?.phase)
            ? a.PairRole.new_
            : a.PairRole.existing);
    model.addListener(_changed);
    WidgetsBinding.instance.addObserver(this);
    _expiry = Timer.periodic(const Duration(seconds: 1), (_) {
      final expires = _display?.expiresAt ?? _pair?.expiresAt;
      if (expires != null &&
          BigInt.from(DateTime.now().millisecondsSinceEpoch ~/ 1000) >=
              expires &&
          !_leaving) {
        unawaited(_abandon());
        if (mounted) setState(() => _error = 'QR이 만료됐어요. 새 QR로 다시 연결해 주세요.');
      }
    });
  }

  void _changed() {
    if (mounted) setState(() {});
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state != AppLifecycleState.resumed &&
        (_pending != null || _flow != null)) {
      unawaited(_abandon());
    }
  }

  Future<bool> _abandon({bool notify = true}) async {
    if (_cancellation case final Future<bool> cancellation) return cancellation;
    final ticket = ++_ticket;
    final flow = _cancelFlow ?? _flow, pending = _pending;
    final ownsOperation = _flow != null || pending != null;
    _cancelFlow = flow == null ? null : List.of(flow);
    _flow = null;
    _display = null;
    _pending = null;
    _confirmed = false;
    _scan = false;
    _error = null;
    // A retry owns only its public flow, not a newer route's pending secrets.
    if (ownsOperation) model.clearSecrets();
    if (mounted && notify) setState(() {});
    if (flow != null) {
      final cancellation = model.cancelAbandonedPair(
        flow,
        afterOperation: pending,
      );
      _cancellation = cancellation;
      final ok = await cancellation;
      if (ticket != _ticket) return false;
      _cancellation = null;
      if (ok) {
        _cancelFlow = null;
      } else {
        _error = '연결 취소를 확인하지 못했어요. 취소를 다시 시도해 주세요.';
      }
      if (mounted && notify) setState(() {});
      return ok;
    }
    return true;
  }

  Future<void> _begin() async {
    if (model.busy || _pending != null || _cancelFlow != null) return;
    final ticket = ++_ticket;
    final direction = _role == a.PairRole.existing
        ? a.PairDirection.existingDisplays
        : a.PairDirection.newDisplays;
    final future = model.pairBegin(
      direction,
      resume: widget.resume?.transaction,
    );
    _pending = future;
    final reply = await future;
    if (!mounted || ticket != _ticket) return;
    setState(() {
      _error = null;
      _consentMayDisclose = false;
      _pending = null;
      _display = reply;
      _flow = reply == null ? null : List.of(reply.flow);
      _confirmed = false;
    });
  }

  Future<void> _receive(String payload) async {
    if (_cancelFlow != null) return;
    final ticket = ++_ticket;
    final future = model.pairScan(
      payload,
      expectedResume: widget.resume?.transaction,
    );
    _pending = future;
    final flow = await future;
    if (!mounted || ticket != _ticket) return;
    setState(() {
      _error = null;
      _consentMayDisclose = false;
      _pending = null;
      _scan = false;
      _flow = flow;
      if (flow == null && widget.resume != null) {
        _error = '선택한 연결을 확인하지 못했어요. 상대 기기의 새 QR로 다시 시도해 주세요.';
      }
    });
  }

  Future<void> _command(bool approve, {bool matches = true}) async {
    final flow = _flow;
    if (flow == null || model.busy) return;
    final ticket = _ticket;
    // Consent can let the other side disclose before our public view catches up.
    if (approve || (_role == a.PairRole.new_ && matches)) {
      _consentMayDisclose = true;
    }
    final future = approve
        ? model.pairApprove(flow)
        : model.pairConfirmCode(flow, matches);
    _pending = future;
    final ok = await future;
    if (!mounted || ticket != _ticket) return;
    setState(() {
      _pending = null;
      if (ok && !approve) _confirmed = matches;
    });
    if (ok && !matches) {
      // Clear this ceremony before forgetting its ownership.
      model.clearSecrets();
      _flow = null;
      await _abandon();
    }
  }

  Future<void> _close() async {
    if (_leaving || _closing) return;
    _closing = true;
    final yes = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('연결 화면을 닫을까요?'),
        content: SingleChildScrollView(
          child: AnimatedBuilder(
            animation: model,
            builder: (_, _) =>
                DeviceWarningScreen(disclosurePossible: _disclosed),
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('계속'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('연결 취소'),
          ),
        ],
      ),
    );
    if (yes != true || !mounted) {
      _closing = false;
      return;
    }
    final canceled = await _abandon();
    _closing = false;
    if (mounted && canceled) {
      setState(() => _leaving = true);
      Navigator.pop(context);
    }
  }

  @override
  void dispose() {
    _expiry?.cancel();
    WidgetsBinding.instance.removeObserver(this);
    model.removeListener(_changed);
    unawaited(_abandon(notify: false));
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final pair = _pair;
    final busy = model.busy || _pending != null || _cancellation != null;
    final content = SingleChildScrollView(
      padding: const EdgeInsets.all(24),
      child: Center(
        child: ConstrainedBox(
          constraints: const BoxConstraints(maxWidth: 600),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              Text(
                _role == a.PairRole.existing ? '기존 기기에서 연결' : '새 기기에서 연결',
                style: Theme.of(context).textTheme.headlineSmall,
              ),
              const Text(
                '양쪽 화면의 확인 코드가 모두 같아야 해요. 기존 기기에서 별도로 승인하기 전에는 연결이 완료되지 않아요.',
              ),
              if (widget.resume != null)
                const Text('새 QR, 새 확인 코드와 양쪽 확인, 기존 기기의 새 승인이 필요해요.'),
              if (_disclosed)
                const DeviceWarningScreen(disclosurePossible: true),
              if (_flow == null && _cancelFlow == null && !busy) ...[
                if (defaultTargetPlatform == TargetPlatform.windows)
                  const Text(
                    'Windows에서는 기존 기기에 QR을 표시하고 새 기기로 스캔하는 반대 방향을 권해요.',
                  ),
                FilledButton(onPressed: _begin, child: const Text('QR 표시')),
                TextButton(
                  onPressed: () => setState(() {
                    _scan = true;
                    ++_input;
                  }),
                  child: const Text('상대 QR 스캔 또는 붙여넣기'),
                ),
              ],
              if (_scan)
                QrInput(key: ValueKey(_input), busy: busy, onPayload: _receive),
              if (_display case final a.PairDisplay display)
                QrPayload(payload: display.qr),
              if (pair != null) ...[
                Text(switch (pair.phase) {
                  a.PairPhase.waitingQr => 'QR 연결 대기',
                  a.PairPhase.comparing => '양쪽 확인 코드 비교 대기',
                  a.PairPhase.awaitingApproval => '기존 기기의 로컬 승인 대기',
                  a.PairPhase.approved => '승인됨 · 전달 대기',
                  a.PairPhase.disclosurePossible => '키 전달 가능 · 보관 확인 대기',
                  a.PairPhase.receiptReady => '이 기기에 보관됨 · 목록 게시 대기',
                  a.PairPhase.receiptVerified => '상대 기기의 보관 확인됨 · 목록 게시 대기',
                  a.PairPhase.publishPending => '연결됨 · 목록 게시 대기',
                  a.PairPhase.active => '최신 목록에서 이 기기 활성 확인',
                  a.PairPhase.observed => '공개 목록 반영 확인',
                }),
                if (pair.remoteInstallation case final List<int> id)
                  SelectableText(
                    '상대 기기: ${publicId(id)}',
                    textDirection: TextDirection.ltr,
                  ),
                if (pair.remoteKind case final a.DeviceKind kind)
                  Text(
                    '상대 기기 유형: ${kind == a.DeviceKind.desktop ? '데스크톱' : '모바일'}',
                  ),
                if (pair.sas case final String sas) ...[
                  SelectableText(
                    sas,
                    textDirection: TextDirection.ltr,
                    style: Theme.of(context).textTheme.headlineMedium,
                  ),
                  FilledButton(
                    onPressed: busy || _confirmed
                        ? null
                        : () => _command(false),
                    child: const Text('양쪽 코드가 같아요'),
                  ),
                  TextButton(
                    onPressed: busy
                        ? null
                        : () => _command(false, matches: false),
                    child: const Text('코드가 달라요'),
                  ),
                ],
                if (pair.localRole == a.PairRole.existing)
                  FilledButton(
                    onPressed:
                        busy || pair.phase != a.PairPhase.awaitingApproval
                        ? null
                        : () => _command(true),
                    child: const Text('이 기기 연결 승인'),
                  ),
              ],
              if (_flow != null)
                TextButton(
                  onPressed: busy ? null : _abandon,
                  child: const Text('새 QR로 다시 시작'),
                ),
              if (_cancelFlow != null)
                TextButton(
                  onPressed: busy ? null : _abandon,
                  child: const Text('연결 취소 다시 시도'),
                ),
              if (_error ?? model.error case final String error)
                Semantics(liveRegion: true, child: Text(error)),
            ],
          ),
        ),
      ),
    );
    return PopScope(
      canPop: _leaving,
      onPopInvokedWithResult: (didPop, _) {
        if (!didPop) unawaited(_close());
      },
      child: Scaffold(
        appBar: AppBar(
          title: const Text('기기 연결'),
          leading: IconButton(
            onPressed: _close,
            tooltip: '취소',
            icon: const Icon(Icons.arrow_back),
          ),
        ),
        body: SafeArea(
          child: _hidden
              ? Center(
                  child: Column(
                    mainAxisSize: MainAxisSize.min,
                    children: [
                      const Text('민감한 내용을 숨겼어요. 새 연결로 다시 시작해 주세요.'),
                      TextButton(
                        onPressed: () => setState(() {
                          _hidden = false;
                          ++_protectionTicket;
                        }),
                        child: const Text('연결 다시 시작'),
                      ),
                    ],
                  ),
                )
              : SensitiveScreen(
                  key: ValueKey(_protectionTicket),
                  onHidden: () {
                    setState(() => _hidden = true);
                    unawaited(_abandon());
                  },
                  onProtectionFailure: () {
                    setState(() => _hidden = true);
                    unawaited(_abandon());
                  },
                  child: content,
                ),
        ),
      ),
    );
  }
}
