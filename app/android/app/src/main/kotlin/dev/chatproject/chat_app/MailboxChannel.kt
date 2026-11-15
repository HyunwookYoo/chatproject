package dev.chatproject.chat_app

import android.Manifest
import android.app.NotificationManager
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.PowerManager
import android.provider.Settings
import androidx.lifecycle.Lifecycle
import com.google.firebase.FirebaseApp
import com.google.firebase.messaging.FirebaseMessaging
import io.flutter.embedding.android.FlutterActivity
import io.flutter.plugin.common.BinaryMessenger
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel
import uniffi.chat_ffi.nativeSetOpenConversation

// `chat_app/mailbox` on Android; the Windows runner (mailbox_host.cpp) serves
// the same method names and argument shapes.
class MailboxChannel(private val activity: FlutterActivity, messenger: BinaryMessenger) {
    private val channel = MethodChannel(messenger, "chat_app/mailbox")
    private val notifications = activity.getSystemService(NotificationManager::class.java)
    private var pendingOpen: Int? = null
    private var openInFlight = false
    private var pendingPermission: MethodChannel.Result? = null
    // The notification id of the conversation Dart shows, if any, and whether the activity is
    // on screen: the native notifier leaves that conversation out only then (spec 5.5).
    private var shownConversation: Int? = null
    private var resumed = false

    init {
        channel.setMethodCallHandler { call, result ->
            // A tap that launched the app arrives before Dart listens; retry it
            // once Dart talks to this channel.
            sendOpen()
            handle(call, result)
        }
    }

    // A tapped message notification (Notifications).
    fun openConversation(id: Int) {
        pendingOpen = id
        sendOpen()
    }

    private fun sendOpen() {
        val id = pendingOpen ?: return
        if (openInFlight) return
        openInFlight = true
        channel.invokeMethod("openConversation", id, object : MethodChannel.Result {
            override fun success(result: Any?) = opened(id)
            override fun error(code: String, message: String?, details: Any?) = opened(id)
            // No Dart handler yet: keep the tap for Dart's next call.
            override fun notImplemented() { openInFlight = false }
        })
    }

    private fun opened(id: Int) {
        openInFlight = false
        if (pendingOpen == id) pendingOpen = null else sendOpen()
    }

    fun setResumed(on: Boolean) {
        resumed = on
        nativeSetOpenConversation(if (on) shownConversation else null)
    }

    fun onRequestPermissionsResult(requestCode: Int, grantResults: IntArray) {
        if (requestCode != PERMISSION_REQUEST) return
        val result = pendingPermission ?: return
        pendingPermission = null
        result.success(grantResults.firstOrNull() == PackageManager.PERMISSION_GRANTED)
    }

    private fun handle(call: MethodCall, result: MethodChannel.Result) {
        when (call.method) {
            "setMailbox" -> {
                val on = call.arguments as? Boolean ?: return result.error("invalid_argument", null, null)
                result.success(setMailbox(on))
            }
            "requestBatteryExemption" -> {
                requestBatteryExemption()
                result.success(null)
            }
            "requestNotificationPermission" -> requestNotificationPermission(result)
            // M7 push (spec 8): the permission screen and settings ask without prompting.
            "notificationPermission" -> result.success(notificationPermission())
            "markNotificationPermissionAsked" -> {
                prefs().edit().putBoolean(PERMISSION_ASKED, true).apply()
                result.success(null)
            }
            "openNotificationSettings" -> {
                openNotificationSettings()
                result.success(null)
            }
            "pushToken" -> pushToken(result)
            // Windows posts these for Dart. On Android the core's events reach Notifications
            // directly, with or without Flutter (spec 5.5); posting here too would alert twice.
            "notify" -> {
                val map = call.arguments as? Map<*, *>
                val update = map?.get("update")
                if (intValue(map?.get("id")) == null || map?.get("title") !is String || map["body"] !is String ||
                    (update != null && update !is Boolean)) {
                    return result.error("invalid_argument", null, null)
                }
                result.success(null)
            }
            "cancelNotification" -> {
                intValue(call.arguments) ?: return result.error("invalid_argument", null, null)
                result.success(null)
            }
            "setOpenConversation" -> {
                val id = call.arguments
                if (id != null && intValue(id) == null) return result.error("invalid_argument", null, null)
                shownConversation = intValue(id)
                if (resumed) nativeSetOpenConversation(shownConversation)
                result.success(null)
            }
            "mailboxTest" -> {
                if ((activity.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) == 0) {
                    return result.notImplemented()
                }
                val service = MailboxService.running
                result.success(mapOf(
                    "serviceRunning" to (service != null),
                    "channelImportance" to notifications.getNotificationChannel(MailboxService.CHANNEL_ID)?.importance,
                    "messagesImportance" to notifications.getNotificationChannel(Notifications.CHANNEL)?.importance,
                    "fgsType" to if (service != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) service.foregroundServiceType else 0,
                    "activeMessages" to notifications.activeNotifications.filter { it.tag == Notifications.TAG }.map { it.id },
                ))
            }
            else -> result.notImplemented()
        }
    }

    // On: only while the app is on screen, where Android allows a foreground
    // service start (spec 8.2). Returns false when the service was not started.
    private fun setMailbox(on: Boolean): Boolean {
        if (!on) {
            MailboxService.stop(activity)
            return true
        }
        if (activity.lifecycle.currentState != Lifecycle.State.RESUMED) return false
        return try {
            MailboxService.start(activity)
            true
        } catch (_: IllegalStateException) {
            // ForegroundServiceStartNotAllowedException on API 31+.
            false
        } catch (_: SecurityException) {
            false
        }
    }

    private fun requestBatteryExemption() {
        val power = activity.getSystemService(PowerManager::class.java)
        if (power.isIgnoringBatteryOptimizations(activity.packageName)) return
        try {
            activity.startActivity(Intent(
                Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                Uri.parse("package:${activity.packageName}"),
            ))
        } catch (_: ActivityNotFoundException) {
            // Some devices have no such screen; the mailbox still runs.
        }
    }

    private fun requestNotificationPermission(result: MethodChannel.Result) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) {
            return result.success(notifications.areNotificationsEnabled())
        }
        if (activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED) {
            return result.success(true)
        }
        // One system prompt at a time.
        if (pendingPermission != null) return result.success(false)
        // Asked once, on the first run with an active account; later runs
        // answer the current state without a prompt.
        val prefs = prefs()
        if (prefs.getBoolean(PERMISSION_ASKED, false)) return result.success(false)
        prefs.edit().putBoolean(PERMISSION_ASKED, true).apply()
        pendingPermission = result
        activity.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), PERMISSION_REQUEST)
    }

    private fun prefs() = activity.getSharedPreferences(PREFS, Context.MODE_PRIVATE)

    // "granted", "denied", or "notDetermined" while Android 13+ has not asked yet.
    private fun notificationPermission(): String = when {
        notifications.areNotificationsEnabled() -> "granted"
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED &&
            !prefs().getBoolean(PERMISSION_ASKED, false) -> "notDetermined"
        else -> "denied"
    }

    private fun openNotificationSettings() {
        try {
            activity.startActivity(Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                .putExtra(Settings.EXTRA_APP_PACKAGE, activity.packageName))
        } catch (_: ActivityNotFoundException) {
            // No such screen on this device.
        }
    }

    // The current FCM token, for Dart to hand to the core: PushService.onNewToken drops one that
    // arrives before the permission (spec 8). Null without Firebase (no google-services.json:
    // FirebaseMessaging.getInstance() would throw) or without notification permission.
    private fun pushToken(result: MethodChannel.Result) {
        if (FirebaseApp.getApps(activity).isEmpty() || !notifications.areNotificationsEnabled()) {
            return result.success(null)
        }
        FirebaseMessaging.getInstance().token.addOnCompleteListener { task ->
            result.success(if (task.isSuccessful) task.result else null)
        }
    }

    companion object {
        private const val PERMISSION_REQUEST = 6001
        private const val PREFS = "chat_app_mailbox"
        private const val PERMISSION_ASKED = "notification_permission_asked"

        private fun intValue(value: Any?): Int? = when (value) {
            is Int -> value
            is Long -> if (value in Int.MIN_VALUE..Int.MAX_VALUE) value.toInt() else null
            else -> null
        }
    }
}
