import Foundation
import Security

// Runner only. NSE must never link this source or receive the account access group.
private let accountKeyLock = NSLock()
private let markerLength = 17

private func keyStatus(_ status: OSStatus) -> Int32 {
  switch status {
  case errSecSuccess: return 0
  case errSecInteractionNotAllowed, errSecAuthFailed: return 1
  case errSecItemNotFound: return 2
  case errSecDecode, errSecParam: return 3
  default: return 4
  }
}

private func keyQuery(_ id: UnsafePointer<UInt8>) -> [String: Any]? {
  guard let group = Bundle.main.object(forInfoDictionaryKey: "AccountKeychainAccessGroup") as? String,
    !group.contains("$("), !group.isEmpty else { return nil }
  let scope = UnsafeBufferPointer(start: id, count: 16).map { String(format: "%02x", $0) }.joined()
  return [kSecClass as String: kSecClassGenericPassword,
    kSecAttrService as String: "ChatProject.account",
    kSecAttrAccount as String: "ChatProject.account." + scope,
    kSecAttrAccessGroup as String: group,
    kSecAttrSynchronizable as String: false,
    kSecUseAuthenticationUI as String: kSecUseAuthenticationUIFail]
}

@_cdecl("cp_key_protect")
func cpKeyProtect(_ id: UnsafePointer<UInt8>?, _ input: UnsafePointer<UInt8>?, _ inputLength: Int,
  _ out: UnsafeMutablePointer<UInt8>?, _ capacity: Int, _ outLength: UnsafeMutablePointer<Int>?) -> Int32 {
  outLength?.pointee = 0
  guard let id, let input, let outLength, inputLength == 32 else { return 3 }
  guard let out, capacity >= markerLength, capacity <= 16384 else { return 5 }
  guard var query = keyQuery(id) else { return 4 }
  accountKeyLock.lock()
  defer { accountKeyLock.unlock() }
  var value = Data(count: markerLength + 32)
  defer { value.resetBytes(in: 0..<value.count) }
  let randomStatus = value.withUnsafeMutableBytes { (bytes: UnsafeMutableRawBufferPointer) -> OSStatus in
    bytes[0] = 1
    return SecRandomCopyBytes(kSecRandomDefault, 16, bytes.baseAddress!.advanced(by: 1))
  }
  guard randomStatus == errSecSuccess else { return 4 }
  value.replaceSubrange(markerLength..<value.count, with: UnsafeBufferPointer(start: input, count: 32))
  query[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
  query[kSecValueData as String] = value
  let status = SecItemAdd(query as CFDictionary, nil)
  // Never overwrite an existing account key, including after interrupted initialization.
  guard status == errSecSuccess else { return status == errSecDuplicateItem ? 1 : keyStatus(status) }
  value.copyBytes(to: out, count: markerLength)
  outLength.pointee = markerLength
  return 0
}

@_cdecl("cp_key_unprotect")
func cpKeyUnprotect(_ id: UnsafePointer<UInt8>?, _ input: UnsafePointer<UInt8>?, _ inputLength: Int,
  _ out: UnsafeMutablePointer<UInt8>?, _ capacity: Int, _ outLength: UnsafeMutablePointer<Int>?) -> Int32 {
  outLength?.pointee = 0
  guard let id, let input, let outLength, inputLength == markerLength, input[0] == 1 else { return 3 }
  guard let out, capacity >= 32, capacity <= 16384 else { return 5 }
  guard var query = keyQuery(id) else { return 4 }
  accountKeyLock.lock()
  defer { accountKeyLock.unlock() }
  query[kSecReturnData as String] = true
  query[kSecMatchLimit as String] = kSecMatchLimitOne
  var result: CFTypeRef?
  let status = SecItemCopyMatching(query as CFDictionary, &result)
  guard status == errSecSuccess else { return keyStatus(status) }
  guard var value = result as? Data, value.count == markerLength + 32 else { return 3 }
  defer { value.resetBytes(in: 0..<value.count) }
  var difference: UInt8 = 0
  for index in 0..<markerLength { difference |= value[index] ^ input[index] }
  guard difference == 0 else { return 3 }
  value.copyBytes(to: out, from: markerLength..<value.count)
  outLength.pointee = 32
  return 0
}

@_cdecl("cp_key_delete")
func cpKeyDelete(_ id: UnsafePointer<UInt8>?) -> Int32 {
  guard let id else { return 3 }
  guard let query = keyQuery(id) else { return 4 }
  accountKeyLock.lock()
  defer { accountKeyLock.unlock() }
  let status = SecItemDelete(query as CFDictionary)
  return status == errSecItemNotFound ? 0 : keyStatus(status)
}

enum AccountStorage {
  static func root(existing: String?) throws -> String {
    let manager = FileManager.default
    guard let group = manager.containerURL(forSecurityApplicationGroupIdentifier: "group.dev.chatproject.chatapp")
      else { throw NSError(domain: "AccountStorage", code: 4) }
    let artifacts = ["store.id", "key.wrap", "account.bin", ".store.id.tmp", ".key.wrap.tmp", ".account.bin.tmp"]
    var root = group.appendingPathComponent("account", isDirectory: true)
    if let existing {
      let old = URL(fileURLWithPath: existing, isDirectory: true)
      if artifacts.contains(where: { manager.fileExists(atPath: old.appendingPathComponent($0).path) }) { root = old }
    }
    try manager.createDirectory(at: root, withIntermediateDirectories: true)
    var flags = URLResourceValues()
    flags.isExcludedFromBackup = true
    try root.setResourceValues(flags)
    return root.path
  }
}
