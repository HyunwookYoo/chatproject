import 'dart:async';

import 'package:flutter/material.dart';

import 'mailbox_client.dart';
import 'mailbox_model.dart';

/// The verdict as the settings show it (M8 spec 4), with this device's uptime
/// while its mailbox role is on.
String verdictLabel(QueueStateView state, {required bool mailboxOn}) {
  final verdict = switch (state.verdict) {
    Verdict.hasMailbox => '메일박스 있음',
    Verdict.noMailbox => '메일박스 없음',
  };
  if (!mailboxOn) return verdict;
  final device = MailboxModel.windows ? '이 PC' : '이 기기';
  return '$verdict · $device 7일 가동률 ${state.uptime7DPermille ~/ 10}%';
}

/// Settings of this device's mailbox role (M6 spec 4; not on iOS) and its
/// "오프라인 메시지" server queue (M8 spec 4; every platform).
class MailboxSettingsScreen extends StatefulWidget {
  const MailboxSettingsScreen({super.key, required this.model});
  final MailboxModel model;

  @override
  State<MailboxSettingsScreen> createState() => _MailboxSettingsScreenState();
}

class _MailboxSettingsScreenState extends State<MailboxSettingsScreen> {
  @override
  void initState() {
    super.initState();
    unawaited(widget.model.refreshQueue());
  }

  @override
  Widget build(BuildContext context) {
    final model = widget.model;
    return Scaffold(
      appBar: AppBar(title: const Text('메일박스')),
      body: SafeArea(
        child: ListenableBuilder(
          listenable: model,
          builder: (context, _) {
            final ready = model.loaded && !model.busy;
            final queue = model.queue;
            return ListView(
              padding: const EdgeInsets.symmetric(vertical: 16),
              children: [
                if (MailboxModel.supported)
                  SwitchListTile(
                    title: Text(
                      MailboxModel.windows ? '이 PC를 메일박스로 쓰기' : '이 기기를 메일박스로 쓰기',
                    ),
                    subtitle: Text(
                      MailboxModel.windows
                          ? '창을 닫아도 트레이에 남아 내 다른 기기로 오는 메시지를 받아 둬요.'
                          : '"연결 상태" 알림을 띄워 둔 채 내 다른 기기로 오는 메시지를 받아 둬요.',
                    ),
                    value: model.enabled,
                    onChanged: ready ? model.setEnabled : null,
                  ),
                if (MailboxModel.windows)
                  SwitchListTile(
                    title: const Text('Windows 시작 시 자동 실행'),
                    subtitle: const Text('로그인하면 창 없이 트레이에서 시작해요.'),
                    value: model.autostart,
                    onChanged: ready ? model.setAutostart : null,
                  ),
                ListTile(
                  title: const Text('오프라인 메시지'),
                  subtitle: queue == null
                      ? null
                      : Text(verdictLabel(queue, mailboxOn: model.enabled)),
                ),
                RadioGroup<QueueMode>(
                  groupValue: queue?.mode,
                  onChanged: (mode) {
                    if (mode != null) model.setQueueMode(mode);
                  },
                  child: Column(
                    children: [
                      RadioListTile<QueueMode>(
                        value: QueueMode.auto,
                        enabled: ready,
                        title: const Text('자동'),
                        subtitle: const Text('메일박스가 없을 때만 서버가 대신 받아 둬요.'),
                      ),
                      RadioListTile<QueueMode>(
                        value: QueueMode.always,
                        enabled: ready,
                        title: const Text('항상 서버 사용'),
                        subtitle: const Text('메일박스가 있어도 서버가 받아 둬요.'),
                      ),
                      RadioListTile<QueueMode>(
                        value: QueueMode.never,
                        enabled: ready,
                        title: const Text('서버 사용 안 함'),
                        subtitle: const Text('서버에 아무것도 남기지 않아요. 꺼져 있던 동안 온 메시지는 메일박스나 상대가 다시 보낼 때 받아요.'),
                      ),
                    ],
                  ),
                ),
                if (model.error case final String error)
                  Padding(
                    padding: const EdgeInsets.all(16),
                    child: Semantics(liveRegion: true, child: Text(error)),
                  ),
              ],
            );
          },
        ),
      ),
    );
  }
}
