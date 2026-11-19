import Foundation

/// The App Group folder the app and the extension share for pushes (M7 spec 5.6): the app's
/// state files `<hex(tag)>.state` (read here, never written) and `inbox.jsonl`, the ciphertexts
/// the extension received, for the app to take at its next launch or return to the foreground
/// (`push_ingest_inbox`).
enum PushInbox {
  static let appGroup = "group.dev.chatproject.chatapp"
  /// The app reads at most this much of the inbox at a time; a fuller one takes no more lines
  /// (the texts still come by the normal paths).
  private static let maxBytes = 8 << 20

  /// `<App Group>/push`, or nil without the App Group (a wrong entitlement or profile).
  static func directory() -> URL? {
    FileManager.default
      .containerURL(forSecurityApplicationGroupIdentifier: appGroup)?
      .appendingPathComponent("push", isDirectory: true)
  }

  /// Adds `{"t":…,"e":…}` as one line unless the same line is there already (the same
  /// ciphertext pushed twice while the app has not taken the inbox). One `write` on a
  /// descriptor opened with O_APPEND, so lines of concurrent extensions do not interleave; a
  /// line an earlier extension did not finish gets a newline first, so it cannot run into this
  /// one. The file has the app's default data protection class. Failures are silent: the
  /// notification still shows, and the text still comes by the normal paths.
  static func append(directory: URL, t: String, e: String) {
    // `t` and `e` are base64url, so the line needs no JSON escaping.
    let alphabet = CharacterSet(charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_")
    guard !t.isEmpty, !e.isEmpty, t.unicodeScalars.allSatisfy({ alphabet.contains($0) }),
          e.unicodeScalars.allSatisfy({ alphabet.contains($0) })
    else { return }
    let entry = Data("{\"t\":\"\(t)\",\"e\":\"\(e)\"}".utf8)
    let url = directory.appendingPathComponent("inbox.jsonl")
    try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    if let existing = try? Data(contentsOf: url, options: .alwaysMapped) {
      if existing.count >= maxBytes || existing.range(of: entry) != nil { return }
    }
    let fd = open(url.path, O_RDWR | O_CREAT | O_APPEND, 0o600)
    guard fd >= 0 else { return }
    defer { close(fd) }
    var line = Data()
    var info = stat()
    if fstat(fd, &info) == 0, info.st_size > 0 {
      var last: UInt8 = 0
      if pread(fd, &last, 1, info.st_size - 1) == 1, last != 0x0A { line.append(0x0A) }
    }
    line.append(entry)
    line.append(0x0A)
    _ = line.withUnsafeBytes { write(fd, $0.baseAddress, $0.count) }
  }
}
