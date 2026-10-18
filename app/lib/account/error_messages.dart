import '../src/rust/api/chat.dart' as rust;

String accountErrorMessage(rust.ErrorCode code) => switch (code) {
  rust.ErrorCode.invalidRecovery => '복구 문구의 단어와 순서를 확인해 주세요.',
  rust.ErrorCode.storageLocked => '기기의 잠금을 해제한 뒤 다시 시도해 주세요.',
  rust.ErrorCode.storageMissing => '계정 보호 키가 없어요. 복구 문구 또는 다른 기기를 통한 복구가 필요해요.',
  rust.ErrorCode.storageCorrupt => '계정 저장 정보를 열 수 없어요. 복구 방법을 확인해 주세요.',
  rust.ErrorCode.writerBusy => '다른 앱 창에서 계정 저장 공간을 사용하고 있어요.',
  rust.ErrorCode.invalidInvite => '서명을 확인할 수 없는 초대예요.',
  rust.ErrorCode.inviteExpired => '초대가 만료됐어요. 새 초대를 받아 주세요.',
  rust.ErrorCode.deviceLimit => '연결할 수 있는 기기는 최대 5대예요.',
  rust.ErrorCode.pairMismatch => '확인 코드가 달라요. 새 QR로 다시 연결해 주세요.',
  rust.ErrorCode.pairExpired => '연결 시간이 지났어요. 새 QR로 다시 연결해 주세요.',
  rust.ErrorCode.recordConflict => '다른 기기에서 목록이 바뀌었어요. 현재 목록을 확인해 주세요.',
  rust.ErrorCode.recordUnavailable => '기기 목록을 확인할 수 없어요. 연결 상태를 확인해 주세요.',
  rust.ErrorCode.notAvailable => '이 기기에서 계정 보호 기능을 사용할 수 없어요.',
  _ => '계정 작업을 완료할 수 없어요. 다시 시도해 주세요.',
};
