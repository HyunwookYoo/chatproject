import 'package:flutter/material.dart';

import 'push_model.dart';
import 'push_platform.dart';

/// This device's push setting (M7 spec 4, 8.5). Android and iOS only.
class NotificationSettingsScreen extends StatelessWidget {
  const NotificationSettingsScreen({super.key, required this.model});
  final PushModel model;

  @override
  Widget build(BuildContext context) => Scaffold(
    appBar: AppBar(title: const Text('알림')),
    body: SafeArea(
      child: ListenableBuilder(
        listenable: model,
        builder: (context, _) {
          final permitted = model.permission == NotificationPermission.granted;
          return ListView(
            padding: const EdgeInsets.symmetric(vertical: 16),
            children: [
              SwitchListTile(
                title: const Text('푸시 알림'),
                subtitle: permitted ? null : const Text('휴대폰 설정에서 알림이 꺼져 있어요'),
                value: permitted && model.enabled,
                onChanged: model.loaded && !model.busy
                    ? model.setEnabled
                    : null,
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
