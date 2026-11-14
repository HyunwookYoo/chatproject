package dev.chatproject.chat_app

import android.app.Application
import android.util.Log
import uniffi.chat_ffi.nativeCoreVersion
import uniffi.chat_ffi.nativeSetNotifier
import uniffi.chat_ffi.uniffiEnsureInitialized

// Runs first in every process start: the launcher, a Firebase push with no activity (spec 5.4),
// or a WorkManager job. A binding that does not match the library fails here, before any
// component calls into it (M1a carry-forward 2).
class ChatApplication : Application() {
    override fun onCreate() {
        System.loadLibrary("chat_ffi")
        NativeInit.initialize(this)
        uniffiEnsureInitialized()
        // Before the core can start, so no notification event goes unshown (spec 5.5).
        nativeSetNotifier(Notifications(this))
        // The Release smoke reads this one line (tools/ci/release_smoke.dart, M1a carry-forward 1).
        Log.i(LOG_TAG, "ChatProject native ok ${nativeCoreVersion()}")
        super.onCreate()
        PushWorkers.scheduleRenewal(this)
    }

    companion object {
        const val LOG_TAG = "ChatProject"
    }
}
