import Foundation
import UserNotifications

/// M1b probe (design 12.3, roadmap M1b): decrypts the push's MLS ciphertext `e` against the
/// bundled 200-leaf group state and appends its own memory use to the App Group file
/// `nse_memory.jsonl`. M7 replaces it with the real extension.
final class NotificationService: UNNotificationServiceExtension {
    private static let appGroup = "group.dev.chatproject.chatapp"
    private static let logName = "nse_memory.jsonl"

    private let lock = NSLock()
    private var contentHandler: ((UNNotificationContent) -> Void)?
    private var original: UNNotificationContent?

    override func didReceive(_ request: UNNotificationRequest,
                             withContentHandler contentHandler: @escaping (UNNotificationContent) -> Void) {
        lock.lock()
        self.contentHandler = contentHandler
        original = request.content
        lock.unlock()

        let info = request.content.userInfo
        let seq = (info["seq"] as? NSNumber)?.intValue ?? -1
        let pid = Int(getpid())
        // Written before any work, so a record exists even if iOS kills the extension.
        Self.append([
            "phase": "start",
            "seq": seq,
            "sent_at": (info["sent_at"] as? NSNumber)?.int64Value ?? -1,
            "received_at": Int64(Date().timeIntervalSince1970 * 1000),
            "pid": pid,
            "footprint": Memory.sample()?.footprint ?? 0,
        ])

        guard let content = request.content.mutableCopy() as? UNMutableNotificationContent else {
            return deliver(request.content)
        }
        var end: [String: Any] = ["phase": "end", "seq": seq, "pid": pid]
        do {
            guard let e = info["e"] as? String, let ciphertext = Data(base64Encoded: e) else {
                throw ProbeError.noCiphertext
            }
            guard let state = Bundle.main.path(forResource: "nse_state", ofType: "bin") else {
                throw ProbeError.noState
            }
            let outcome = try probeDecrypt(statePath: state, ciphertext: ciphertext)
            end["ok"] = true
            end["plaintext_len"] = outcome.plaintextLen
            end["load_us"] = outcome.loadMicros
            end["decrypt_us"] = outcome.decryptMicros
            content.body = "#\(seq) \(outcome.preview)"
        } catch {
            end["ok"] = false
            end["error"] = "\(error)"
        }
        let sample = Memory.sample()
        end["footprint"] = sample?.footprint ?? 0
        end["peak"] = sample?.peak ?? -1
        end["available"] = os_proc_available_memory() // 0 when iOS does not report it for extensions
        Self.append(end)
        if let peak = sample?.peak {
            content.title = String(format: "NSE 최대 %.1f MB", Double(peak) / 1_048_576)
        }
        deliver(content)
    }

    override func serviceExtensionTimeWillExpire() {
        lock.lock()
        let content = original
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

    /// Appends one JSON line with O_APPEND, so lines from concurrent notifications do not interleave.
    private static func append(_ record: [String: Any]) {
        guard let dir = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: appGroup),
              var line = try? JSONSerialization.data(withJSONObject: record, options: [.sortedKeys]) else { return }
        line.append(0x0A)
        let fd = open(dir.appendingPathComponent(logName).path, O_WRONLY | O_CREAT | O_APPEND, 0o644)
        guard fd >= 0 else { return }
        defer { close(fd) }
        _ = line.withUnsafeBytes { write(fd, $0.baseAddress, $0.count) }
    }
}

private enum ProbeError: Error { case noCiphertext, noState }

private enum Memory {
    struct Sample { let footprint: UInt64; let peak: Int64? }

    /// TASK_VM_INFO: phys_footprint now, and the lifetime peak (rev 3, iOS 13+).
    static func sample() -> Sample? {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size)
        let kr = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
            }
        }
        guard kr == KERN_SUCCESS else { return nil }
        // The kernel reports how many words it filled; the peak field needs revision 3.
        let peakEnd = (MemoryLayout<task_vm_info_data_t>.offset(of: \task_vm_info_data_t.ledger_phys_footprint_peak)!
                       + MemoryLayout<Int64>.size) / MemoryLayout<natural_t>.size
        return Sample(footprint: info.phys_footprint,
                      peak: Int(count) >= peakEnd ? info.ledger_phys_footprint_peak : nil)
    }
}
