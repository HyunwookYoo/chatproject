import XCTest
import Security
@testable import Runner

final class AccountKeyStoreTests: XCTestCase {
  typealias Operation = (UnsafePointer<UInt8>?, UnsafePointer<UInt8>?, Int,
    UnsafeMutablePointer<UInt8>?, Int, UnsafeMutablePointer<Int>?) -> Int32

  private func id() -> [UInt8] { Array(UUID().uuidString.utf8.prefix(16)) }

  private func call(_ operation: Operation, _ id: [UInt8], _ input: [UInt8], capacity: Int = 16384)
    -> (Int32, [UInt8], Int) {
    var output = [UInt8](repeating: 0, count: capacity)
    var length = 99
    let status = id.withUnsafeBufferPointer { scope in
      input.withUnsafeBufferPointer { source in
        output.withUnsafeMutableBufferPointer { target in
          operation(scope.baseAddress, source.baseAddress, input.count, target.baseAddress,
            capacity, &length)
        }
      }
    }
    return (status, Array(output.prefix(max(0, length))), length)
  }

  func testRoundTripScopeTamperAndDelete() {
    let first = id(), second = id()
    defer { _ = cpKeyDelete(first); _ = cpKeyDelete(second) }
    let key = [UInt8](repeating: 0x45, count: 32)
    let wrapped = call(cpKeyProtect, first, key)
    XCTAssertEqual(wrapped.0, 0)
    XCTAssertNotEqual(wrapped.1, key)
    XCTAssertEqual(call(cpKeyProtect, second, key).0, 0)
    let opened = call(cpKeyUnprotect, first, wrapped.1)
    XCTAssertEqual(opened.0, 0)
    XCTAssertEqual(opened.1, key)
    XCTAssertEqual(call(cpKeyProtect, first, [UInt8](repeating: 7, count: 32)).0, 1)
    XCTAssertEqual(call(cpKeyUnprotect, first, wrapped.1).1, key)
    XCTAssertEqual(call(cpKeyUnprotect, second, wrapped.1).0, 3)
    var corrupt = wrapped.1
    guard !corrupt.isEmpty else { return }
    corrupt[corrupt.count - 1] ^= 1
    XCTAssertEqual(call(cpKeyUnprotect, first, corrupt).0, 3)
    XCTAssertEqual(cpKeyDelete(first), 0)
    XCTAssertEqual(cpKeyDelete(first), 0)
    let missing = call(cpKeyUnprotect, first, wrapped.1)
    XCTAssertEqual(missing.0, 2)
    XCTAssertEqual(missing.2, 0)
  }

  func testKeychainAccessibilityAndBackupPolicy() throws {
    let scope = id()
    defer { _ = cpKeyDelete(scope) }
    XCTAssertEqual(call(cpKeyProtect, scope, [UInt8](repeating: 8, count: 32)).0, 0)
    let alias = "ChatProject.account." + scope.map { String(format: "%02x", $0) }.joined()
    var result: CFTypeRef?
    let status = SecItemCopyMatching([kSecClass as String: kSecClassGenericPassword,
      kSecAttrService as String: "ChatProject.account", kSecAttrAccount as String: alias,
      kSecReturnAttributes as String: true] as CFDictionary, &result)
    XCTAssertEqual(status, errSecSuccess)
    let attributes = try XCTUnwrap(result as? [String: Any])
    XCTAssertEqual(attributes[kSecAttrAccessible as String] as? String,
      kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly as String)
    XCTAssertEqual(attributes[kSecAttrSynchronizable as String] as? Bool, false)
    XCTAssertEqual(attributes[kSecAttrAccessGroup as String] as? String,
      Bundle.main.object(forInfoDictionaryKey: "AccountKeychainAccessGroup") as? String)
    let root = URL(fileURLWithPath: try AccountStorage.root(existing: nil))
    XCTAssertEqual(root.lastPathComponent, "account")
    XCTAssertEqual(try root.resourceValues(forKeys: [.isExcludedFromBackupKey]).isExcludedFromBackup, true)
  }

  func testInvalidCapacityLeavesNoKeyAndZeroLength() {
    let scope = id()
    defer { _ = cpKeyDelete(scope) }
    let result = call(cpKeyProtect, scope, [UInt8](repeating: 1, count: 32), capacity: 1)
    XCTAssertEqual(result.0, 5)
    XCTAssertEqual(result.2, 0)
    XCTAssertEqual(call(cpKeyProtect, scope, [UInt8](repeating: 1, count: 31)).0, 3)
    XCTAssertEqual(call(cpKeyUnprotect, scope, [UInt8](repeating: 1, count: 16385)).0, 3)
    XCTAssertEqual(cpKeyDelete(nil), 3)
  }

  func testRootPreservesIncompleteLegacyOrSqlState() throws {
    let manager = FileManager.default
    let owned = manager.temporaryDirectory.appendingPathComponent("account-path-\(UUID().uuidString)", isDirectory: true)
    try manager.createDirectory(at: owned, withIntermediateDirectories: false)
    defer { try? manager.removeItem(at: owned) }
    for name in ["store.id", "key.wrap", "account.bin", ".store.id.tmp", ".key.wrap.tmp", ".account.bin.tmp",
      "db.sqlite3", "db.sqlite3-wal", "db.sqlite3-shm", "db.keys", "db.keys.pending",
      ".db.keys.tmp", ".db.keys.pending.tmp", "storage.mode", ".storage.mode.tmp",
      "migration.prep", ".migration.prep.tmp", "migration.checkpoint", ".migration.checkpoint.tmp"] {
      let artifact = owned.appendingPathComponent(name)
      try Data([1]).write(to: artifact)
      defer { try? manager.removeItem(at: artifact) }
      XCTAssertEqual(try AccountStorage.root(existing: owned.path), owned.path, "Preserve existing path for \(name)")
    }
  }

  func testOwnedKeyBufferErasesRetainedAliasOnSuccessAndFailure() throws {
    for status in [Int32(0), 1, 2, 3, 4] {
      var retainedQuery: [String: Any] = [:]
      let returned = withAccountKeyBuffer { value in
        value.mutableBytes.initializeMemory(as: UInt8.self, repeating: 0x5a, count: value.length)
        // Retain the same reference through the container used by SecItemAdd.
        retainedQuery[kSecValueData as String] = value
        XCTAssertTrue((retainedQuery[kSecValueData as String] as? NSMutableData) === value)
        return status
      }
      XCTAssertEqual(returned, status)
      let alias = try XCTUnwrap(retainedQuery[kSecValueData as String] as? NSMutableData)
      XCTAssertEqual(alias.length, 49)
      let bytes = UnsafeBufferPointer(start: alias.bytes.assumingMemoryBound(to: UInt8.self), count: alias.length)
      XCTAssertTrue(bytes.allSatisfy { $0 == 0 }, "Retained application buffer must be cleared on every return")
    }
  }

  // Run in two separate xcodebuild invocations with ACCOUNT_KEY_TEST_PHASE=write/read.
  // The fixture contains only a random StoreId and opaque marker, never the key.
  func testRestartPersistence() throws {
    guard let phase = ProcessInfo.processInfo.environment["ACCOUNT_KEY_TEST_PHASE"] else {
      throw XCTSkip("Restart persistence requires separate write/read test processes")
    }
    guard phase == "write" || phase == "read" else {
      XCTFail("ACCOUNT_KEY_TEST_PHASE must be write or read")
      return
    }
    let file = FileManager.default.temporaryDirectory.appendingPathComponent("account-key-test-restart")
    let key = [UInt8](repeating: 0x45, count: 32)
    if phase == "write" {
      let scope = id()
      let wrapped = call(cpKeyProtect, scope, key)
      XCTAssertEqual(wrapped.0, 0)
      try Data(scope + wrapped.1).write(to: file)
    } else {
      let data = Array(try Data(contentsOf: file))
      let scope = Array(data.prefix(16))
      defer { _ = cpKeyDelete(scope); try? FileManager.default.removeItem(at: file) }
      let opened = call(cpKeyUnprotect, scope, Array(data.dropFirst(16)))
      XCTAssertEqual(opened.0, 0)
      XCTAssertEqual(opened.1, key)
    }
  }
}
