package dev.chatproject.chat_app

import android.app.NotificationManager
import android.content.Context
import android.os.SystemClock
import android.util.Log
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import uniffi.chat_ffi.NativeException
import uniffi.chat_ffi.NativePushOutcome
import uniffi.chat_ffi.nativePushDeleted
import uniffi.chat_ffi.nativePushReceived
import uniffi.chat_ffi.nativePushToken

// The FCM receiver (spec 5.4). Firebase runs these callbacks on its own background thread, with
// no Flutter engine; the core is started here when the app is not running. No token, tag,
// payload or message text is logged.
class PushService : FirebaseMessagingService() {
    override fun onMessageReceived(message: RemoteMessage) {
        // Exactly as received: the core takes only the maps the proxy builds.
        handle(applicationContext, message.data)
    }

    override fun onDeletedMessages() {
        try {
            NativeCore.ensureStarted(applicationContext)
            nativePushDeleted()
        } catch (error: Exception) {
            refused("Deleted pushes", error)
        }
    }

    // firebase-messaging 26 deprecates this for register()/onRegistered, which an app opts into
    // with a manifest flag. Without the flag (this app) tokens still arrive only here.
    @Suppress("OVERRIDE_DEPRECATION")
    override fun onNewToken(token: String) {
        // Without notification permission push stays off: the core never hears of a token.
        if (!getSystemService(NotificationManager::class.java).areNotificationsEnabled()) return
        try {
            NativeCore.ensureStarted(applicationContext)
            nativePushToken(token)
        } catch (error: Exception) {
            refused("Token", error)
        }
    }

    companion object {
        // Within FCM's processing time: the core waits up to this and Rust adds at most 2 s.
        private const val RECEIVE_DEADLINE_MS = 8_000
        // A wake that returns this close to its deadline did not see its text arrive (the core
        // lingers 1 s after it does), so the fetch goes on in push_fetch. Lossy: a text that
        // arrives in this last stretch also triggers it, and the re-run wake replaces that
        // text's notification with the generic count, silently. Exact needs the FFI to say
        // whether the fetch completed.
        private const val FETCH_LINGER_MS = 1_500

        // One received push, from PushService or the debug TestPushReceiver; blocks. Returns the
        // core's outcome, or null when the core did not take it. A `cipher` or `wake` push is
        // high priority and ends in a visible notification unless its tag is unknown or its
        // text was read already (spec 5.3).
        fun handle(context: Context, data: Map<String, String>): NativePushOutcome? {
            val wake = "w" in data
            // The receive call alone: its deadline is what a finished fetch returns before. A
            // cold core start before it does not count.
            var started = 0L
            val outcome = try {
                NativeCore.ensureStarted(context)
                started = SystemClock.elapsedRealtime()
                // The core shows the notification through Notifications, or the wake's
                // "새 메시지" first and its text once fetched.
                nativePushReceived(data, RECEIVE_DEADLINE_MS.toUInt())
            } catch (error: Exception) {
                refused("Push", error)
                // No active account here (never made, reset, or still being set up): no
                // conversation owns the tag, as with UNKNOWN_TAG, so nothing shows.
                val noAccount = error is NativeException.Core && error.code == "AccountRestricted"
                if ((wake || "e" in data) && !noAccount) Notifications(context).showGeneric()
                return null
            }
            val elapsed = SystemClock.elapsedRealtime() - started
            if (wake && outcome == NativePushOutcome.NOTIFIED && elapsed >= RECEIVE_DEADLINE_MS - FETCH_LINGER_MS) {
                PushWorkers.scheduleFetch(context, data)
            }
            return outcome
        }

        // The push_fetch worker: the rest of a wake's fetch, within FETCH_MAX_MS. Its generic
        // notification shows already; this one only changes it silently.
        fun fetch(context: Context, data: Map<String, String>) {
            try {
                NativeCore.ensureStarted(context)
                nativePushReceived(data, PushWorkers.FETCH_DEADLINE_MS.toUInt())
            } catch (error: Exception) {
                refused("Wake fetch", error)
            }
        }

        // Only the failure's kind, and the core's ErrorCode name, which carries nothing more.
        private fun refused(what: String, error: Exception) {
            val code = if (error is NativeException) " ${error.message}" else ""
            Log.w(ChatApplication.LOG_TAG, "$what not taken: ${error.javaClass.simpleName}$code")
        }
    }
}
