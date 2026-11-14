import 'package:flutter/material.dart';

import 'mailbox_model.dart';

/// Shows [FirstLinkScreen] once over [child] when the Windows PC has not
/// shown it yet (spec 8.1: right after linking the PC, or the first run after
/// the update).
class FirstLinkGate extends StatefulWidget {
  const FirstLinkGate({super.key, required this.model, required this.child});
  final MailboxModel model;
  final Widget child;
  @override
  State<FirstLinkGate> createState() => _FirstLinkGateState();
}

class _FirstLinkGateState extends State<FirstLinkGate> {
  bool _shown = false;

  @override
  void initState() {
    super.initState();
    widget.model.addListener(_check);
    _check();
  }

  void _check() {
    if (_shown || !widget.model.firstLinkDue) return;
    _shown = true;
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!mounted) return;
      Navigator.of(context).push(
        MaterialPageRoute<void>(
          builder: (_) => FirstLinkScreen(model: widget.model),
        ),
      );
    });
    WidgetsBinding.instance.scheduleFrame();
  }

  @override
  void dispose() {
    widget.model.removeListener(_check);
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => widget.child;
}

/// The two Windows switches, both on by default; changed later in settings.
class FirstLinkScreen extends StatefulWidget {
  const FirstLinkScreen({super.key, required this.model});
  final MailboxModel model;
  @override
  State<FirstLinkScreen> createState() => _FirstLinkScreenState();
}

class _FirstLinkScreenState extends State<FirstLinkScreen> {
  bool _mailbox = true;
  bool _autostart = true;

  Future<void> _confirm() async {
    final done = await widget.model.confirmFirstLink(
      mailbox: _mailbox,
      autostart: _autostart,
    );
    if (done && mounted) Navigator.of(context).pop();
  }

  @override
  Widget build(BuildContext context) => PopScope(
    canPop: false,
    child: Scaffold(
      appBar: AppBar(
        title: const Text('이 PC 설정'),
        automaticallyImplyLeading: false,
      ),
      body: SafeArea(
        child: ListenableBuilder(
          listenable: widget.model,
          builder: (context, _) => ListView(
            padding: const EdgeInsets.all(16),
            children: [
              const Text('이 PC가 켜져 있으면 휴대폰이 꺼져 있는 동안 온 메시지를 받아 두었다가 넘겨줘요.'),
              const SizedBox(height: 16),
              SwitchListTile(
                title: const Text('이 PC를 메일박스로 쓰기'),
                subtitle: const Text('창을 닫아도 트레이에 남아 계속 받아요.'),
                value: _mailbox,
                onChanged: (on) => setState(() => _mailbox = on),
              ),
              SwitchListTile(
                title: const Text('Windows 시작 시 자동 실행'),
                subtitle: const Text('로그인하면 창 없이 트레이에서 시작해요.'),
                value: _autostart,
                onChanged: (on) => setState(() => _autostart = on),
              ),
              const SizedBox(height: 8),
              const Text('나중에 설정의 메일박스에서 바꿀 수 있어요.'),
              const SizedBox(height: 16),
              FilledButton(
                onPressed: widget.model.busy ? null : _confirm,
                child: const Text('확인'),
              ),
            ],
          ),
        ),
      ),
    ),
  );
}
