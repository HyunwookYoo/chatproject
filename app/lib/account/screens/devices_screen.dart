import 'package:flutter/material.dart';
import 'package:flutter/foundation.dart';
import '../../src/rust/api/account.dart' as a;
import '../account_model.dart';
import '../public_id.dart';
import '../record_review.dart';
import '../sensitive_screen.dart';
import 'device_warning_screen.dart';

class DevicesScreen extends StatefulWidget {
  const DevicesScreen({super.key, required this.model, required this.onPair});
  final AccountModel model;
  final void Function(a.PairResumeView?) onPair;
  @override
  State<DevicesScreen> createState() => _DevicesScreenState();
}

class _DevicesScreenState extends State<DevicesScreen>
    with WidgetsBindingObserver {
  AccountModel get model => widget.model;
  int _lifecycle = 0;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state != AppLifecycleState.resumed) ++_lifecycle;
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    super.dispose();
  }

  bool _current(int lifecycle) =>
      mounted &&
      lifecycle == _lifecycle &&
      (WidgetsBinding.instance.lifecycleState == null ||
          WidgetsBinding.instance.lifecycleState == AppLifecycleState.resumed);

  Future<void> _republish(
    BuildContext context,
    a.RecordView record,
    List<int> hash,
  ) async {
    final displayedHash = List<int>.unmodifiable(hash);
    final lifecycle = _lifecycle;
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (dialogContext) => SensitiveScreen(
        onHidden: () => ++_lifecycle,
        onProtectionFailure: () => ++_lifecycle,
        child: AnimatedBuilder(
          animation: model,
          builder: (context, _) => AlertDialog(
            title: const Text('저장된 목록 다시 게시'),
            content: SingleChildScrollView(
              child: Column(
                crossAxisAlignment: CrossAxisAlignment.start,
                mainAxisSize: MainAxisSize.min,
                children: [
                  const Text(
                    '저장된 기기 목록을 그대로 다시 게시합니다. 기존 충돌은 유지되며, 이후 공개 목록을 다시 확인하고 충돌 해결을 완료해야 합니다.',
                  ),
                  Text('세대: ${publicId(record.generation)}'),
                  Text('목록 해시: ${publicId(displayedHash)}'),
                  for (final device in record.devices)
                    Text(
                      '${device.kind == a.DeviceKind.desktop ? '데스크톱' : '모바일'} · ${publicId(device.installationId)}',
                    ),
                ],
              ),
            ),
            actions: [
              TextButton(
                onPressed: () => Navigator.pop(dialogContext, false),
                child: const Text('취소'),
              ),
              FilledButton(
                onPressed: model.busy
                    ? null
                    : () => Navigator.pop(dialogContext, true),
                child: const Text('다시 게시'),
              ),
            ],
          ),
        ),
      ),
    );
    if (confirmed != true || !_current(lifecycle) || !context.mounted) return;
    final sent = await model.republishAcceptedRecord(displayedHash);
    if (sent && _current(lifecycle) && context.mounted) {
      ScaffoldMessenger.of(context).showSnackBar(
        const SnackBar(content: Text('목록을 다시 보냈어요. 공개 목록 확인이 필요해요.')),
      );
    }
  }

  Future<void> _revoke(BuildContext context, a.DeviceView device) async {
    final target = List<int>.of(device.installationId);
    final confirmed = await showDialog<bool>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('이 기기를 해제할까요?'),
        content: SingleChildScrollView(
          child: DeviceWarningScreen(
            disclosurePossible: true,
            target: publicId(target),
          ),
        ),
        actions: [
          TextButton(
            onPressed: () => Navigator.pop(context, false),
            child: const Text('유지'),
          ),
          FilledButton(
            onPressed: () => Navigator.pop(context, true),
            child: const Text('기기 해제'),
          ),
        ],
      ),
    );
    if (confirmed == true) await model.revokeDevice(target);
  }

  @override
  Widget build(BuildContext context) => AnimatedBuilder(
    animation: model,
    builder: (context, _) {
      final view = model.snapshot;
      if (view == null) return const SizedBox();
      final pending =
          view.phase == a.AccountPhase.pendingImport ||
          view.phase == a.AccountPhase.candidate;
      return Scaffold(
        appBar: AppBar(title: const Text('내 기기')),
        body: SafeArea(
          child: SingleChildScrollView(
            padding: const EdgeInsets.all(24),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                Text(
                  '${view.devices.length} / 5대',
                  style: Theme.of(context).textTheme.headlineSmall,
                ),
                Text(switch (view.publication.state) {
                  a.PublicationState.localBlocked => '이 기기에서 차단됨',
                  a.PublicationState.pending => '공개 목록 갱신 대기',
                  a.PublicationState.observed => '공개 목록 반영 확인',
                  a.PublicationState.conflict => '목록 충돌',
                }),
                if (view.disclosurePossible ||
                    view.pendingImportRequiresRemoteRevoke)
                  const DeviceWarningScreen(disclosurePossible: true),
                if (view.pendingImportRequiresRemoteRevoke)
                  const Text('로컬에서 버려도 공개 목록은 바뀌지 않아요. 기존 기기에서 해제해 주세요.'),
                if (view.publication.conflict
                    case final a.RecordView conflict) ...[
                  if (!conflict.devices.any(
                    (d) => listEquals(d.installationId, view.installation),
                  ))
                    const Text(
                      '이 기기가 현재 목록에 없어요. 목록 확인으로 권한을 되찾을 수 없어요. 신뢰하는 기기를 통해 다시 연결하거나 복구 방법을 확인해 주세요.',
                    ),
                  if (view.publication.conflictHash case final List<int> hash)
                    RecordReview(
                      record: conflict,
                      hash: hash,
                      enabled:
                          !model.busy &&
                          conflict.devices.any(
                            (d) =>
                                listEquals(d.installationId, view.installation),
                          ),
                      generationChanged: !listEquals(
                        conflict.generation,
                        view.publication.generation,
                      ),
                      onConfirm: model.confirmRecordConflict,
                    ),
                ],
                if (view.phase == a.AccountPhase.conflict &&
                    view.publication.state == a.PublicationState.conflict &&
                    view.publication.canRepublishAccepted &&
                    view.publication.accepted != null &&
                    view.publication.acceptedHash != null)
                  TextButton(
                    onPressed: model.busy
                        ? null
                        : () => _republish(
                            context,
                            view.publication.accepted!,
                            view.publication.acceptedHash!,
                          ),
                    child: const Text('저장된 목록 다시 게시'),
                  ),
                Text('게시 대기 작업: ${view.publication.pendingCount}'),
                const Text('이전에 수용한 기기 목록'),
                for (final device in view.devices)
                  ListTile(
                    isThreeLine: true,
                    title: Text(
                      '${device.kind == a.DeviceKind.desktop ? '데스크톱' : '모바일'}${listEquals(device.installationId, view.installation) ? ' · 현재 기기' : ''}',
                    ),
                    subtitle: Text(
                      '${publicId(device.installationId)}\n마지막 확인 정보 없음',
                    ),
                    trailing: TextButton(
                      onPressed:
                          model.busy ||
                              view.devices.length <= 1 ||
                              listEquals(
                                device.installationId,
                                view.installation,
                              )
                          ? null
                          : () => _revoke(context, device),
                      child: const Text('해제'),
                    ),
                  ),
                if (view.devices.length >= 5)
                  const Text('연결할 수 있는 기기는 최대 5대예요.'),
                FilledButton(
                  onPressed: model.busy || view.devices.length >= 5 || pending
                      ? null
                      : () => widget.onPair(null),
                  child: const Text('기기 연결'),
                ),
                for (final resume in view.pairResumes)
                  TextButton(
                    onPressed: model.busy ? null : () => widget.onPair(resume),
                    child: Text(
                      '중단된 기기 연결 재개 · ${resume.role == a.PairRole.existing ? '기존 기기' : '새 기기'} · ${publicId(resume.transaction)}',
                    ),
                  ),
                if (view.publicPeerId != null)
                  TextButton(
                    onPressed: model.busy
                        ? null
                        : () => model.refreshRecord(view.publicPeerId!),
                    child: const Text('목록 다시 확인'),
                  ),
                if (pending ||
                    (view.phase == a.AccountPhase.locked &&
                        view.pendingImportRequiresRemoteRevoke))
                  TextButton(
                    onPressed: model.busy
                        ? null
                        : () async {
                            final yes = await showDialog<bool>(
                              context: context,
                              builder: (context) => AlertDialog(
                                title: const Text('중단된 연결 버리기'),
                                content: const SingleChildScrollView(
                                  child: DeviceWarningScreen(
                                    disclosurePossible: true,
                                  ),
                                ),
                                actions: [
                                  TextButton(
                                    onPressed: () =>
                                        Navigator.pop(context, false),
                                    child: const Text('유지'),
                                  ),
                                  FilledButton(
                                    onPressed: () =>
                                        Navigator.pop(context, true),
                                    child: const Text('로컬 연결 버리기'),
                                  ),
                                ],
                              ),
                            );
                            if (yes == true) await model.discardPendingImport();
                          },
                    child: const Text('중단된 연결 버리기'),
                  ),
                if (model.error case final String error)
                  Semantics(liveRegion: true, child: Text(error)),
              ],
            ),
          ),
        ),
      );
    },
  );
}
