import '../src/rust/api/chat.dart' as rust;

String messageErrorMessage(rust.ErrorCode code) => switch (code) {
  rust.ErrorCode.messageTooLarge => '본문은 UTF-8 기준 16KiB 이하여야 해요.',
  rust.ErrorCode.invalidMessage => '메시지 내용을 확인해 주세요.',
  rust.ErrorCode.messageConflict => '같은 메시지의 내용이 일치하지 않아요. 입력을 확인해 주세요.',
  rust.ErrorCode.messageNotFound => '메시지를 찾을 수 없어요. 다시 불러와 주세요.',
  rust.ErrorCode.contactRequired => '상호 연락처 등록이 필요해요.',
  rust.ErrorCode.unsupportedMessageVersion => '지원하지 않는 메시지 버전이에요.',
  rust.ErrorCode.storageFull => '저장 공간이 부족해요. 입력은 유지하고 공간을 확보한 뒤 다시 시도해 주세요.',
  rust.ErrorCode.storageUncertain => '저장 결과를 확인해야 해요. 입력을 유지하고 다시 열어 확인해 주세요.',
  rust.ErrorCode.storageLocked => '기기의 잠금을 해제한 뒤 다시 시도해 주세요.',
  rust.ErrorCode.storageMissing ||
  rust.ErrorCode.storageCorrupt => '계정 저장 정보를 열 수 없어요. 복구 방법을 확인해 주세요.',
  rust.ErrorCode.writerBusy => '다른 앱 창의 저장 작업이 끝난 뒤 다시 시도해 주세요.',
  rust.ErrorCode.accountRestricted ||
  rust.ErrorCode.authFailed ||
  rust.ErrorCode.recordConflict => '계정과 기기 상태를 확인한 뒤 다시 시도해 주세요.',
  _ => '메시지 작업을 완료할 수 없어요. 입력을 유지하고 다시 시도해 주세요.',
};
