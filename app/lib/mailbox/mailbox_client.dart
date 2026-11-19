import '../src/rust/api/chat.dart' as chat;
import '../src/rust/api/mailbox.dart' as rust;

export '../src/rust/api/mailbox.dart' show MailboxStateView;

/// The core's mailbox calls (M6 spec 7, 8), behind an interface for widget tests.
abstract interface class MailboxClient {
  Future<rust.MailboxStateView> mailboxState();
  Future<void> setMailbox(bool enabled);
  Future<void> markFirstLinkShown();

  /// The app came to the foreground with an Active account: the network
  /// starts and my other devices sync.
  Future<void> mailboxForeground();

  /// The account became Active with the app on screen: the network starts
  /// (nothing else; the sync waits for the next resume).
  Future<void> startNetwork();

  /// The app goes to the background or exits: hand undelivered messages to my mailboxes.
  Future<int> mailboxBackground();
}

class FrbMailboxClient implements MailboxClient {
  const FrbMailboxClient();
  @override
  Future<rust.MailboxStateView> mailboxState() => rust.mailboxState();
  @override
  Future<void> setMailbox(bool enabled) => rust.setMailbox(enabled: enabled);
  @override
  Future<void> markFirstLinkShown() => rust.markFirstLinkShown();
  @override
  Future<void> mailboxForeground() => rust.mailboxForeground();
  @override
  Future<void> startNetwork() => chat.networkStart();
  @override
  Future<int> mailboxBackground() => rust.mailboxBackground();
}
