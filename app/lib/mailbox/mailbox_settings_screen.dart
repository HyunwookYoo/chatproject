import 'package:flutter/material.dart';

import 'mailbox_model.dart';

/// Settings of this device's mailbox role (M6 spec 4). Not offered on iOS.
class MailboxSettingsScreen extends StatelessWidget {
  const MailboxSettingsScreen({super.key, required this.model});
  final MailboxModel model;

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: const Text('메일박스')),
    body: SafeArea(
      child: ListenableBuilder(
        listenable: model,
        builder: (context, _) {
          final ready = model.loaded && !model.busy;
          return ListView(
            padding: const EdgeInsets.symmetric(vertical: 16),
            children: [
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
