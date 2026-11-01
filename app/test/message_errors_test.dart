import 'package:chat_app/message/error_messages.dart';
import 'package:chat_app/src/rust/api/chat.dart' as rust;
import 'package:flutter_test/flutter_test.dart';

void main() {
  test(
    'message errors provide safe Korean recovery without native details',
    () {
      for (final code in rust.ErrorCode.values) {
        final text = messageErrorMessage(code);
        expect(text, matches(RegExp('[가-힣]')));
        expect(text, isNot(contains(code.name)));
      }
      expect(messageErrorMessage(rust.ErrorCode.storageFull), contains('입력'));
      expect(
        messageErrorMessage(rust.ErrorCode.messageTooLarge),
        contains('16'),
      );
      expect(
        messageErrorMessage(rust.ErrorCode.storageUncertain),
        contains('확인'),
      );
      expect(
        messageErrorMessage(rust.ErrorCode.contactRequired),
        contains('상호'),
      );
    },
  );
}
