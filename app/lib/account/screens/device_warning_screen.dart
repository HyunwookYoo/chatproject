import 'package:flutter/material.dart';

class DeviceWarningScreen extends StatelessWidget {
  const DeviceWarningScreen({
    super.key,
    required this.disclosurePossible,
    this.target,
  });
  final bool disclosurePossible;
  final String? target;
  @override
  Widget build(BuildContext context) => Column(
    crossAxisAlignment: CrossAxisAlignment.stretch,
    children: [
      if (target != null) Text('해제할 기기: $target'),
      Text(
        disclosurePossible
            ? '이 기기가 계정 키 사본을 받았을 수 있어요. 연결을 취소하거나 목록에서 해제해도 원격 사본이 지워졌다고 보장할 수 없어요.'
            : '연결 취소를 요청해요. 취소는 이미 전달된 키 사본을 지우지 않아요. 새로 연결할 때는 새 QR과 확인 코드가 필요해요.',
      ),
      if (disclosurePossible)
        const Text(
          '키 노출이 의심되면 새 아이덴티티를 사용해 주세요. 같은 복구 문구로 복구해도 기존 키 사본은 무효화되지 않아요.',
        ),
    ],
  );
}
