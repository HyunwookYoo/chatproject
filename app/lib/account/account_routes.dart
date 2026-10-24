import 'record_review.dart';
import 'dart:async';
import 'package:flutter/material.dart';
import 'package:flutter/foundation.dart' show listEquals;
import '../src/rust/api/account.dart' as a;
import 'account_model.dart';
import 'sensitive_screen.dart';
import 'invite_links.dart';
import 'screens/welcome_screen.dart';
import 'screens/pair_screen.dart';
import 'screens/devices_screen.dart';
import 'screens/recovery_words_screen.dart';
import 'screens/recovery_confirm_screen.dart';
import 'screens/recover_screen.dart';
import 'screens/invite_screen.dart';
import 'screens/contact_preview_screen.dart';
import 'screens/safety_screen.dart';

enum _Page { home, words, confirm, recover, invite, preview, contact, safety }

/// One application model; only short-lived display state belongs to this route.
class AccountRoutes extends StatefulWidget {
  const AccountRoutes({
    super.key,
    required this.model,
    required this.child,
    this.onPair,
    this.onOpenConversation,
  });
  final AccountModel model;
  final Widget child;
  final VoidCallback? onPair;
  final ValueChanged<List<int>>? onOpenConversation;
  @override
  State<AccountRoutes> createState() => _AccountRoutesState();
}

class _AccountRoutesState extends State<AccountRoutes>
    with WidgetsBindingObserver {
  _Page _page = _Page.home;
  a.RecoveryDisplay? _words;
  List<int>? _flow;
  Future<bool>? _confirmation;
  List<int> _positions = [];
  String? _invite;
  a.InvitePreview? _preview;
  a.ContactView? _contact;
  ({a.SafetyDisplay display, List<int> local, List<int> peer})? _safety;
  final _links = InviteLinks();
  String? _incomingInvite;
  int _ticket = 0;
  bool _working = false;
  String? _routeError;
  AccountModel get model => widget.model;
  @override
  void initState() {
    super.initState();
    model.addListener(_changed);
    WidgetsBinding.instance.addObserver(this);
    unawaited(
      _links.start((payload) {
        _incomingInvite = payload;
        _consumeInvite();
      }),
    );
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    // Before a display exists there is no SensitiveScreen observer yet.
    if (state != AppLifecycleState.resumed && _page == _Page.home && _working) {
      unawaited(_cancel());
    }
  }

  void _consumeInvite() {
    if (!mounted ||
        model.busy ||
        _working ||
        ![_Page.home, _Page.invite].contains(_page) ||
        ![
          a.AccountPhase.active,
          a.AccountPhase.localReady,
          a.AccountPhase.publishPending,
          a.AccountPhase.conflict,
        ].contains(model.snapshot?.phase)) {
      return;
    }
    final payload = _incomingInvite;
    if (payload == null) return;
    _incomingInvite = null;
    final ticket = _ticket;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (_current(ticket) && [_Page.home, _Page.invite].contains(_page)) {
        unawaited(_parse(payload));
      }
    });
    WidgetsBinding.instance.scheduleFrame();
  }

  void _changed() {
    if (!mounted) return;
    _consumeInvite();
    setState(() {
      final safety = _safety;
      if (safety != null &&
          !listEquals(safety.local, model.snapshot?.publicPeerId)) {
        ++_ticket;
        _safety = null;
        if (_page == _Page.safety) _page = _Page.contact;
      }
      final selected = _contact;
      if (selected != null) {
        for (final contact in model.snapshot?.contacts ?? <a.ContactView>[]) {
          if (listEquals(contact.peerId, selected.peerId)) {
            _contact = contact;
            break;
          }
        }
      }
    });
  }

  bool _current(int ticket) => mounted && ticket == _ticket;
  void _drop() {
    _words = null;
    _positions = [];
    model.clearSecrets();
  }

  Future<void> _cancel() async {
    final ticket = ++_ticket;
    final flow = _flow;
    final confirmation = _confirmation;
    _flow = null;
    _confirmation = null;
    _drop();
    if (mounted) {
      setState(() {
        _page = _Page.home;
        _working = false;
      });
    }
    if (flow != null) {
      final ok = await _cancelFlow(flow, confirmation);
      if (_current(ticket) && !ok) {
        setState(
          () => _routeError = '계정 생성을 취소하지 못했어요. 현재 상태를 확인한 뒤 다시 시도해 주세요.',
        );
      }
    }
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    model.removeListener(_changed);
    _links.dispose();
    _incomingInvite = null;
    ++_ticket;
    final flow = _flow;
    final confirmation = _confirmation;
    _flow = null;
    _confirmation = null;
    _drop();
    if (flow != null) unawaited(_cancelFlow(flow, confirmation));
    super.dispose();
  }

  Future<bool> _cancelFlow(List<int> flow, Future<bool>? confirmation) =>
      confirmation == null
      ? model.cancelCreate(flow)
      : model.cancelAbandonedCreate(flow, afterConfirmation: confirmation);

  Future<void> _begin() async {
    if (_working || model.busy) return;
    final ticket = ++_ticket;
    setState(() {
      _working = true;
      _routeError = null;
    });
    final display = await model.beginCreate();
    if (!_current(ticket)) return;
    setState(() {
      _working = false;
      if (display != null) {
        _words = display;
        _flow = List.of(display.flow);
        _positions = List.of(display.positions);
        _page = _Page.words;
      }
    });
  }

  void _confirmPage() {
    setState(() {
      _words = null;
      model.clearSecrets();
      _page = _Page.confirm;
    });
  }

  Future<void> _confirm(List<String> answers) async {
    final flow = _flow;
    if (flow == null || _working || model.busy) return;
    final ticket = _ticket;
    _working = true;
    final confirmation = model.confirmCreate(flow, answers);
    _confirmation = confirmation;
    final ok = await confirmation;
    if (identical(_confirmation, confirmation)) _confirmation = null;
    if (!_current(ticket)) return;
    setState(() {
      _working = false;
      if (ok) {
        _flow = null;
        _drop();
        _page = _Page.home;
      }
    });
  }

  Future<void> _recover(String words) async {
    if (_working || model.busy) return;
    final ticket = _ticket;
    _working = true;
    final ok = await model.recover(words, true);
    if (!_current(ticket)) return;
    setState(() {
      _working = false;
      if (ok) _page = _Page.home;
    });
  }

  Future<void> _parse(String input) async {
    final ticket = _ticket;
    final preview = await model.parseInvite(
      input,
      BigInt.from(DateTime.now().millisecondsSinceEpoch ~/ 1000),
    );
    if (_current(ticket) && preview != null) {
      setState(() {
        _invite = input;
        _preview = preview;
        _page = _Page.preview;
      });
    }
  }

  Future<void> _add(String name) async {
    final input = _invite;
    if (input == null) return;
    final ticket = _ticket;
    final contact = await model.acceptInvite(input, name);
    if (_current(ticket) && contact != null) {
      setState(() {
        _contact = null;
        _preview = null;
        _invite = null;
        _page = _Page.home;
      });
    }
  }

  Future<void> _showSafety() async {
    final local = model.snapshot?.publicPeerId;
    final peer = _contact?.peerId;
    if (local == null || peer == null) return;
    final ticket = _ticket;
    final requestLocal = List<int>.of(local);
    final requestPeer = List<int>.of(peer);
    final display = await model.safetyDisplay(requestLocal, requestPeer);
    if (_current(ticket) &&
        display != null &&
        listEquals(requestLocal, model.snapshot?.publicPeerId) &&
        listEquals(requestPeer, _contact?.peerId)) {
      setState(() {
        _safety = (display: display, local: requestLocal, peer: requestPeer);
        _page = _Page.safety;
      });
    }
  }

  Future<void> _verify() async {
    final safety = _safety;
    if (safety == null ||
        !listEquals(safety.local, model.snapshot?.publicPeerId) ||
        !listEquals(safety.peer, _contact?.peerId)) {
      return;
    }
    final ticket = _ticket;
    final ok = await model.confirmSafety(safety.peer, safety.display.digest);
    if (_current(ticket) && ok) {
      setState(() {
        _safety = null;
        _page = _Page.home;
      });
    }
  }

  void _go(_Page page) {
    ++_ticket;
    setState(() {
      _page = page;
      _routeError = null;
    });
  }

  void _pair([a.PairResumeView? resume]) => Navigator.of(context).push(
    MaterialPageRoute<void>(
      builder: (_) => PairScreen(model: model, resume: resume),
    ),
  );
  void _devices() => Navigator.of(context).push(
    MaterialPageRoute<void>(
      builder: (_) => DevicesScreen(model: model, onPair: _pair),
    ),
  );
  Widget _contactDetail() {
    final selectedPeer = List<int>.of(_contact!.peerId);
    final remote = model.snapshot?.remoteRecords
        .where((r) => listEquals(r.peer, selectedPeer))
        .firstOrNull;
    return Column(
      crossAxisAlignment: CrossAxisAlignment.stretch,
      children: [
        Text(
          isolatedName(_contact!.alias),
          style: Theme.of(context).textTheme.headlineSmall,
        ),
        Text(_contact!.verified ? '안전번호 확인됨' : '안전번호 미확인'),
        if (_contact!.newDevice) const Text('새 기기가 있어요'),
        if (remote?.cached == true)
          const Text('저장된 기기 목록으로 인증했어요. 최신 공개 목록은 확인하지 못했어요.'),
        if (remote?.conflict case final a.RecordView conflict)
          if (remote?.conflictHash case final List<int> hash)
            RecordReview(
              record: conflict,
              hash: hash,
              enabled: !model.busy,
              generationChanged: !listEquals(
                conflict.generation,
                remote?.acceptedGeneration,
              ),
              onConfirm: (displayedHash) =>
                  model.confirmRemoteRecord(selectedPeer, displayedHash),
            ),
        if (_contact!.deviceUnverified) const Text('기기 목록을 확인하지 못했어요'),
        if (!_contact!.deviceUnverified) const Text('현재 기기 인증됨'),
        if (widget.onOpenConversation != null)
          FilledButton(
            onPressed: model.busy
                ? null
                : () => widget.onOpenConversation!(selectedPeer),
            child: const Text('대화 시작'),
          ),
        FilledButton(
          onPressed: model.busy
              ? null
              : () => model.connectAccount(List<int>.of(_contact!.peerId)),
          child: const Text('현재 기기 인증'),
        ),
        FilledButton(
          onPressed: model.busy ? null : _showSafety,
          child: const Text('안전번호 확인'),
        ),
      ],
    );
  }

  Widget _contacts(a.AccountView view) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      TextButton(onPressed: _devices, child: const Text('내 기기')),
      Text('연락처', style: Theme.of(context).textTheme.headlineMedium),
      const SizedBox(height: 16),
      if (view.phase == a.AccountPhase.publishPending ||
          view.phase == a.AccountPhase.localReady)
        const Text('공개 목록 반영을 기다리고 있어요.'),
      if (view.phase == a.AccountPhase.conflict)
        const Text('다른 기기에서 목록이 바뀌었어요. 현재 목록을 확인해 주세요.'),
      if (view.contacts.isEmpty)
        const Padding(
          padding: EdgeInsets.symmetric(vertical: 24),
          child: Text('아직 연락처가 없어요. 초대를 확인하고 직접 추가해 주세요.'),
        ),
      for (final contact in view.contacts)
        ListTile(
          title: Text(isolatedName(contact.alias)),
          subtitle: Text(contact.verified ? '안전번호 확인됨' : '안전번호 미확인'),
          onTap: () {
            ++_ticket;
            setState(() {
              _contact = contact;
              _safety = null;
              _page = _Page.contact;
            });
          },
        ),
      FilledButton(
        onPressed: () => _go(_Page.invite),
        child: const Text('초대 붙여넣기'),
      ),
      TextButton(
        onPressed: () => Navigator.of(
          context,
        ).push(MaterialPageRoute<void>(builder: (_) => widget.child)),
        child: const Text('대화 및 네트워크 진단'),
      ),
    ],
  );
  Widget _bounded(Widget child) => Align(
    alignment: Alignment.topCenter,
    child: SingleChildScrollView(
      padding: const EdgeInsets.all(24),
      child: ConstrainedBox(
        constraints: const BoxConstraints(maxWidth: 600),
        child: FocusTraversalGroup(
          policy: ReadingOrderTraversalPolicy(),
          child: Column(
            crossAxisAlignment: CrossAxisAlignment.stretch,
            children: [
              child,
              if (_routeError ?? model.error case final String error) ...[
                const SizedBox(height: 16),
                Semantics(liveRegion: true, child: Text(error)),
              ],
            ],
          ),
        ),
      ),
    ),
  );
  @override
  Widget build(BuildContext context) {
    final view = model.snapshot;
    final unconfigured =
        view == null ||
        view.phase == a.AccountPhase.unconfigured ||
        view.phase == a.AccountPhase.candidate;
    final busy = _working || model.busy;
    final content = switch (_page) {
      _Page.words => RecoveryWordsScreen(
        words: _words?.words ?? [],
        onContinue: _confirmPage,
      ),
      _Page.confirm => RecoveryConfirmScreen(
        positions: _positions,
        busy: busy,
        onConfirm: _confirm,
      ),
      _Page.recover => RecoverScreen(busy: busy, onRecover: _recover),
      _Page.invite => InviteScreen(
        busy: busy,
        onParse: _parse,
        onCreate: model.createInvite,
      ),
      _Page.preview => ContactPreviewScreen(
        preview: _preview!,
        busy: busy,
        onAdd: _add,
      ),
      _Page.contact => _contactDetail(),
      _Page.safety => SafetyScreen(
        display: _safety!.display,
        busy: busy,
        onConfirm: _verify,
      ),
      _Page.home =>
        unconfigured
            ? Column(
                crossAxisAlignment: CrossAxisAlignment.stretch,
                children: [
                  if (view?.pendingPairTransactions.isNotEmpty == true) ...[
                    const Text('중단된 기기 연결이 있어요. 새 QR로 재개하거나 로컬 연결을 버릴 수 있어요.'),
                    TextButton(onPressed: _devices, child: const Text('내 기기')),
                  ] else ...[
                    WelcomeScreen(
                      busy: busy,
                      onCreate: _begin,
                      onRecover: () => _go(_Page.recover),
                      onPair: widget.onPair ?? _pair,
                    ),
                    if (view?.phase == a.AccountPhase.candidate)
                      TextButton(
                        onPressed: _devices,
                        child: const Text('내 기기'),
                      ),
                  ],
                ],
              )
            : _contacts(view),
    };
    final sensitive = [
      _Page.words,
      _Page.confirm,
      _Page.recover,
    ].contains(_page);
    final body = _bounded(content);
    return PopScope(
      canPop: _page == _Page.home,
      onPopInvokedWithResult: (didPop, result) {
        if (!didPop) unawaited(_cancel());
      },
      child: Scaffold(
        appBar: AppBar(
          title: const Text('ChatProject'),
          leading: _page == _Page.home
              ? null
              : IconButton(
                  tooltip: '취소',
                  onPressed: _cancel,
                  icon: const Icon(Icons.arrow_back),
                ),
          actions: [
            if (_page != _Page.home)
              TextButton(onPressed: _cancel, child: const Text('취소')),
          ],
        ),
        body: SafeArea(
          child: sensitive
              ? SensitiveScreen(
                  key: const ValueKey('account-sensitive'),
                  onHidden: () => unawaited(_cancel()),
                  onProtectionFailure: () {
                    unawaited(_cancel());
                    if (mounted) {
                      setState(
                        () =>
                            _routeError = '민감한 화면을 보호할 수 없어요. 잠시 뒤 다시 시도해 주세요.',
                      );
                    }
                  },
                  child: body,
                )
              : LayoutBuilder(
                  builder: (context, constraints) {
                    if (!unconfigured &&
                        constraints.maxWidth >= 900 &&
                        [_Page.contact, _Page.safety].contains(_page)) {
                      return Row(
                        crossAxisAlignment: CrossAxisAlignment.start,
                        children: [
                          Expanded(child: _bounded(_contacts(view))),
                          const VerticalDivider(),
                          Expanded(child: body),
                        ],
                      );
                    }
                    return body;
                  },
                ),
        ),
      ),
    );
  }
}
