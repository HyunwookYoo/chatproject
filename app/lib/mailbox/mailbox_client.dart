import '../src/rust/api/mailbox.dart' as rust;

export '../src/rust/api/mailbox.dart' show MailboxStateView;

/// The core's mailbox calls (M6 spec 7, 8), behind an interface for widget tests.
abstract interface class MailboxClient {
  Future<rust.MailboxStateView> mailboxState();
  Future<void> setMailbox(bool enabled);
  Future<void> markFirstLinkShown();

  /// The app came to the foreground: sync with my other devices.
  Future<void> mailboxForeground();

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
  Future<int> mailboxBackground() => rust.mailboxBackground();
}
