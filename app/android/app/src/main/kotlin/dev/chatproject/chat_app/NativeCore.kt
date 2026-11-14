package dev.chatproject.chat_app

import android.content.Context
import java.io.File
import uniffi.chat_ffi.NativeException
import uniffi.chat_ffi.NativeInfo
import uniffi.chat_ffi.nativeCoreVersion
import uniffi.chat_ffi.nativeInstanceId
import uniffi.chat_ffi.nativeStart

// The core for Kotlin without Flutter: the push receiver and its workers (spec 5.4). There is
// one core per process; Dart's coreStart and this reach the same one (Task 9), so the app's
// later start attaches to a core the receiver began, and the receiver to one Dart began.
object NativeCore {
    private var info: NativeInfo? = null

    // Dart's root: accountStorageRoot(getApplicationSupportDirectory()/core) in core_client.dart.
    fun storageRoot(context: Context): String {
        val app = context.applicationContext
        return AccountKeyStore(app).storageRoot(File(app.filesDir, "core").path)
    }

    // Starts the core and opens the account once per process; blocks, so never on the main thread.
    @Synchronized
    fun ensureStarted(context: Context): NativeInfo {
        info?.let { return it }
        val started = try {
            nativeStart(storageRoot(context), null)
        } catch (error: NativeException.Core) {
            // Debug runs may start Dart's core in another folder (--data-dir): use that one.
            val running = nativeInstanceId()
            if (error.code != "AlreadyStartedDifferentConfig" || running == null) throw error
            NativeInfo(nativeCoreVersion(), running)
        }
        info = started
        return started
    }
}
