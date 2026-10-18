import 'dart:convert';
import 'package:flutter/material.dart';
import '../../src/rust/api/account.dart';

String isolatedName(String value) => '\u2068$value\u2069';

class ContactPreviewScreen extends StatefulWidget {
  const ContactPreviewScreen({
    super.key,
    required this.preview,
    required this.busy,
    required this.onAdd,
  });
  final InvitePreview preview;
  final bool busy;
  final Future<void> Function(String) onAdd;
  @override
  State<ContactPreviewScreen> createState() => _ContactPreviewScreenState();
}

class _ContactPreviewScreenState extends State<ContactPreviewScreen> {
  late final _name = TextEditingController(
    text: widget.preview.displayName ?? '',
  );
  String? _error;
  @override
  void dispose() {
    _name.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      Text('연락처 이름 확인', style: Theme.of(context).textTheme.headlineSmall),
      const Text('서명은 초대 내용을 확인해요. 상대의 실제 신원을 보장하지 않아요.'),
      if (widget.preview.displayName != null)
        Text('제안된 이름: ${isolatedName(widget.preview.displayName!)}'),
      const SizedBox(height: 16),
      TextField(
        controller: _name,
        textDirection: TextDirection.ltr,
        decoration: InputDecoration(labelText: '내가 부를 이름', errorText: _error),
      ),
      FilledButton(
        onPressed: widget.busy
            ? null
            : () {
                if (utf8.encode(_name.text).length > 64) {
                  setState(() => _error = '이름은 UTF-8 64바이트까지 입력할 수 있어요.');
                  return;
                }
                setState(() => _error = null);
                widget.onAdd(_name.text);
              },
        child: const Text('연락처 추가'),
      ),
    ],
  );
}
