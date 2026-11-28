import '../src/rust/api/chat.dart' as chat;
import '../src/rust/api/mailbox.dart' as rust;
import '../src/rust/api/queue.dart' as queue;

export '../src/rust/api/mailbox.dart' show MailboxStateView;
export '../src/rust/api/queue.dart' show QueueMode, QueueStateView, Verdict;

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

  /// The "오프라인 메시지" setting and the mailbox verdict (M8 spec 4).
  Future<queue.QueueStateView> queueState();
  Future<void> queueSetMode(queue.QueueMode mode);
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
  @override
  Future<queue.QueueStateView> queueState() => queue.queueState();
  @override
  Future<void> queueSetMode(queue.QueueMode mode) =>
      queue.queueSetMode(mode: mode);
}
