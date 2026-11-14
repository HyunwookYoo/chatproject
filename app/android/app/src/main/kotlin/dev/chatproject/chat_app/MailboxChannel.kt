package dev.chatproject.chat_app

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
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
import io.flutter.embedding.android.FlutterActivity
import io.flutter.plugin.common.BinaryMessenger
import io.flutter.plugin.common.MethodCall
import io.flutter.plugin.common.MethodChannel

// `chat_app/mailbox` on Android; the Windows runner (mailbox_host.cpp) serves
// the same method names and argument shapes.
class MailboxChannel(private val activity: FlutterActivity, messenger: BinaryMessenger) {
    private val channel = MethodChannel(messenger, "chat_app/mailbox")
    private val notifications = activity.getSystemService(NotificationManager::class.java)
    private var pendingOpen: Int? = null
    private var openInFlight = false
    private var pendingPermission: MethodChannel.Result? = null

    init {
        channel.setMethodCallHandler { call, result ->
            // A tap that launched the app arrives before Dart listens; retry it
            // once Dart talks to this channel.
            sendOpen()
            handle(call, result)
        }
    }

    // A tapped message notification (see `notify`).
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
            "notify" -> {
                val map = call.arguments as? Map<*, *>
                val id = intValue(map?.get("id"))
                val title = map?.get("title") as? String
                val body = map?.get("body") as? String
                // Optional: a count update of a shown notification.
                val update = map?.get("update")
                if (id == null || title == null || body == null || (update != null && update !is Boolean)) {
                    return result.error("invalid_argument", null, null)
                }
                notify(id, title, body, update == true)
                result.success(null)
            }
            "cancelNotification" -> {
                val id = intValue(call.arguments) ?: return result.error("invalid_argument", null, null)
                notifications.cancel(MESSAGES_TAG, id)
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
                    "messagesImportance" to notifications.getNotificationChannel(MESSAGES_CHANNEL)?.importance,
                    "fgsType" to if (service != null && Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) service.foregroundServiceType else 0,
                    "activeMessages" to notifications.activeNotifications.filter { it.tag == MESSAGES_TAG }.map { it.id },
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
        val prefs = activity.getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        if (prefs.getBoolean(PERMISSION_ASKED, false)) return result.success(false)
        prefs.edit().putBoolean(PERMISSION_ASKED, true).apply()
        pendingPermission = result
        activity.requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), PERMISSION_REQUEST)
    }

    // update: a count change of the notification shown for `id`. It never
    // alerts and never brings back one the user dismissed.
    private fun notify(id: Int, title: String, body: String, update: Boolean) {
        if (update && notifications.activeNotifications.none { it.tag == MESSAGES_TAG && it.id == id }) return
        notifications.createNotificationChannel(
            NotificationChannel(MESSAGES_CHANNEL, "메시지", NotificationManager.IMPORTANCE_DEFAULT),
        )
        val open = PendingIntent.getActivity(
            activity, id,
            Intent(activity, MainActivity::class.java)
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP)
                .putExtra(EXTRA_CONVERSATION, id),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val notification = Notification.Builder(activity, MESSAGES_CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(title)
            .setContentText(body)
            .setContentIntent(open)
            .setAutoCancel(true)
            .setOnlyAlertOnce(update)
            .setCategory(Notification.CATEGORY_MESSAGE)
            .build()
        notifications.notify(MESSAGES_TAG, id, notification)
    }

    companion object {
        const val EXTRA_CONVERSATION = "chat_conversation"
        private const val MESSAGES_CHANNEL = "messages"
        // A tag keeps message ids apart from the service's own notification id.
        private const val MESSAGES_TAG = "message"
        private const val PERMISSION_REQUEST = 6001
        private const val PREFS = "chat_app_mailbox"
        private const val PERMISSION_ASKED = "notification_permission_asked"

        fun conversationOf(intent: Intent?): Int? =
            if (intent?.hasExtra(EXTRA_CONVERSATION) == true) intent.getIntExtra(EXTRA_CONVERSATION, 0) else null

        private fun intValue(value: Any?): Int? = when (value) {
            is Int -> value
            is Long -> if (value in Int.MIN_VALUE..Int.MAX_VALUE) value.toInt() else null
            else -> null
        }
    }
}
