import 'package:chat_app/account/account_model.dart';
import 'package:chat_app/src/rust/api/account.dart' as account;
import 'package:chat_app/src/rust/api/chat.dart' as rust;
import 'package:flutter_test/flutter_test.dart';
import 'account_model_test.dart' show FakeAccountClient, view;

void main() {
  test('secret_replies_not_in_events', () async {
    final client = FakeAccountClient();
    final model = AccountModel(client);
    final events = <rust.ChatEvent>[];
    final sub = client.events().listen(events.add);
    await model.open();
    final snapshot = view(BigInt.from(3));
    client.controller.add(
      rust.ChatEvent.accountChanged(
        revision: snapshot.revision,
        view: snapshot,
      ),
    );
    expect(model.snapshot!.revision, snapshot.revision);
    expect(events.single, isA<rust.ChatEvent_AccountChanged>());
    expect(
      (events.single as rust.ChatEvent_AccountChanged).view,
      isA<account.AccountView>(),
    );
    await sub.cancel();
    await model.close();
    await client.controller.close();
  });
}
