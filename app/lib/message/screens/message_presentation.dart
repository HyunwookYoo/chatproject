import 'package:flutter/foundation.dart';

import '../../account/screens/contact_preview_screen.dart';
import '../message_bridge.dart';
import '../message_model.dart';

String deliveryLabel(DeliveryState state) => switch (state) {
  DeliveryState.queued => '보내기 대기',
  DeliveryState.sentDirect => '전송됨',
  DeliveryState.delivered => '전달됨',
  DeliveryState.read => '읽음',
  DeliveryState.blocked => '전송 보류',
  DeliveryState.failed => '전송 실패',
  DeliveryState.cancelled => '전송 취소',
};

String messageIdentity(MessageKey key) =>
    '${key.conversation.join(',')}/${key.sender.join(',')}/${key.installation.join(',')}/${key.id.join(',')}';

String peerLabel(MessageModel model, List<int> peer) {
  for (final contact in model.account.snapshot?.contacts ?? []) {
    if (listEquals(contact.peerId, peer)) return isolatedName(contact.alias);
  }
  return '상대 ${peer.take(4).map((byte) => byte.toRadixString(16).padLeft(2, '0')).join()}';
}

// SQL i64 timestamps can exceed both DateTime and JavaScript's exact integers.
// Bound before conversion; keep unread/ordering/revision values as BigInt.
String messageTime(BigInt milliseconds) {
  if (milliseconds < BigInt.zero ||
      milliseconds > BigInt.from(253402300799999)) {
    return '시각 확인 불가';
  }
  final date = DateTime.fromMillisecondsSinceEpoch(milliseconds.toInt());
  String two(int value) => value.toString().padLeft(2, '0');
  return '${date.year}.${two(date.month)}.${two(date.day)} ${two(date.hour)}:${two(date.minute)}';
}

String blockLabel(BlockReason? reason) => switch (reason) {
  BlockReason.contactRequired => '양쪽에서 상호 연락처를 등록한 뒤 다시 시도해 주세요.',
  BlockReason.account => '계정과 기기 상태를 확인한 뒤 다시 시도해 주세요.',
  BlockReason.storage => '기기 잠금과 저장 공간을 확인한 뒤 다시 시도해 주세요.',
  null => '전송 조건을 확인한 뒤 다시 시도해 주세요.',
};
