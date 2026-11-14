import Foundation
import UserNotifications

/// The notification service extension (M7 spec 5.6, design 12.3). A push carries `t` (the tag of
/// one of this device's handles) and, for a text that fits, `e` (its MLS ciphertext without the
/// group id). `chat_nse` decrypts `e` read-only against the app's state file of the tag; the
/// extension never writes group state, it only appends `e` to the inbox the app takes later.
///
/// * No `t`: the push's own content ("새 메시지", the app's `NEW_MSG`).
/// * No state file for `t` (a withdrawn tag, a conversation gone): an empty content, so
///   nothing is shown (Review Focus 1).
/// * Otherwise the alias and the start of the text, or "새 메시지" when it cannot be decrypted
///   (no `e`, a state older than the text, before the first unlock), grouped by `t`.
/// * Out of time: "새 메시지".
final class NotificationService: UNNotificationServiceExtension {
  private let lock = NSLock()
  private var contentHandler: ((UNNotificationContent) -> Void)?
  private var fallback: UNNotificationContent?

  override func didReceive(_ request: UNNotificationRequest,
                           withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void) {
    let info = request.content.userInfo
    let t = info["t"] as? String
    var fallback = request.content
    if let t, let grouped = request.content.mutableCopy() as? UNMutableNotificationContent {
      grouped.threadIdentifier = t
      fallback = grouped
    }
    lock.lock()
    self.contentHandler = contentHandler
    self.fallback = fallback
    lock.unlock()

    guard let t, let directory = PushInbox.directory() else { return deliver(fallback) }
    let e = info["e"] as? String ?? ""
    let notice = nseOpen(pushDir: directory.path, tagB64: t, strippedB64: e)
    if notice.body.isEmpty { return deliver(UNNotificationContent()) }
    if !e.isEmpty { PushInbox.append(directory: directory, t: t, e: e) }
    guard let content = fallback.mutableCopy() as? UNMutableNotificationContent else {
      return deliver(fallback)
    }
    content.title = notice.title
    content.body = notice.body
    deliver(content)
  }

  override func serviceExtensionTimeWillExpire() {
    lock.lock()
    let content = fallback
    lock.unlock()
    if let content { deliver(content) }
  }

  /// Calls the handler at most once, whichever path gets here first.
  private func deliver(_ content: UNNotificationContent) {
    lock.lock()
    let handler = contentHandler
    contentHandler = nil
    lock.unlock()
    handler?(content)
  }
}
