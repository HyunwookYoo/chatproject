package dev.chatproject.chat_app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.util.Log
import uniffi.chat_ffi.NativeNotifier
import uniffi.chat_ffi.NativeNotifyItem

// Message notifications on Android, posted from the core's events with or without Flutter
// (spec 5.5): the M6 channel, tag, id rule and tap intent. The core builds the text (Task 8)
// and leaves out the conversation on screen; Dart posts none on Android. Without notification
// permission it posts nothing.
class Notifications(context: Context) : NativeNotifier {
    private val context = context.applicationContext
    private val manager = this.context.getSystemService(NotificationManager::class.java)

    init {
        // From the first process start, so FCM's default channel (AndroidManifest.xml) exists.
        manager.createNotificationChannel(NotificationChannel(CHANNEL, "메시지", NotificationManager.IMPORTANCE_DEFAULT))
    }

    // On the core's `native-notifier` thread.
    override fun notify(items: List<NativeNotifyItem>) {
        for (item in items) {
            try {
                if (item.cancel) cancel(item.id) else show(item.id, item.title, item.body, item.update, item.silent)
            } catch (error: RuntimeException) {
                // The system refused this one (e.g. its rate limit); the next event shows the count.
                Log.w(ChatApplication.LOG_TAG, "Notification not posted: ${error.javaClass.simpleName}")
            }
        }
    }

    // update: a count change of the notification shown for `id`. It never alerts and never
    // brings back one the user dismissed (M6 Ruling 31). silent: new texts a generic push
    // notification alerted for already (Ruling R13): posted without alerting, also when the
    // user dismissed that one, so a high-priority push still ends visible (spec 2).
    private fun show(id: Int, title: String, body: String, update: Boolean, silent: Boolean) {
        if (!manager.areNotificationsEnabled()) return
        if (update && manager.activeNotifications.none { it.tag == TAG && it.id == id }) return
        post(TAG, id, isolated(title), body, update, open(id), silent)
    }

    private fun cancel(id: Int) {
        if (!manager.areNotificationsEnabled()) return
        manager.cancel(TAG, id)
    }

    // A high-priority push the core could not take (spec 5.3): its conversation is unknown
    // here, so one app-wide "새 메시지" that opens the app. Gone once the app is on screen.
    fun showGeneric() {
        if (!manager.areNotificationsEnabled()) return
        // Its own action: a conversation whose id is also GENERIC_ID keeps its tap intent.
        post(GENERIC_TAG, GENERIC_ID, GENERIC_TITLE, null, false, PendingIntent.getActivity(
            context, GENERIC_ID,
            Intent(context, MainActivity::class.java).setAction(Intent.ACTION_MAIN).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        ))
    }

    fun cancelGeneric() = manager.cancel(GENERIC_TAG, GENERIC_ID)

    private fun open(id: Int) = PendingIntent.getActivity(
        context, id,
        Intent(context, MainActivity::class.java)
            .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP)
            .putExtra(EXTRA_CONVERSATION, id),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    private fun post(tag: String, id: Int, title: String, body: String?, update: Boolean, open: PendingIntent, silent: Boolean = false) {
        val notification = Notification.Builder(context, CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(title)
            .apply { if (body != null) setContentText(body) }
            .setContentIntent(open)
            .setAutoCancel(true)
            .setOnlyAlertOnce(update || silent)
            // What Builder.setSilent (API 31) does: a member of a group that alerts only for its
            // summary, which is never posted, never alerts.
            .apply { if (silent) setGroup(SILENT_GROUP).setGroupAlertBehavior(Notification.GROUP_ALERT_SUMMARY) }
            .setCategory(Notification.CATEGORY_MESSAGE)
            .build()
        manager.notify(tag, id, notification)
    }

    companion object {
        const val CHANNEL = "messages"
        // A tag keeps message ids apart from the mailbox service's own notification id.
        const val TAG = "message"
        const val GENERIC_TAG = "push"
        const val GENERIC_ID = 0
        private const val SILENT_GROUP = "silent"
        const val EXTRA_CONVERSATION = "chat_conversation"
        // The core's title when a contact has no alias (Task 9 native_items).
        private const val GENERIC_TITLE = "새 메시지"

        fun conversationOf(intent: Intent?): Int? =
            if (intent?.hasExtra(EXTRA_CONVERSATION) == true) intent.getIntExtra(EXTRA_CONVERSATION, 0) else null

        // A contact's alias, isolated like Dart's isolatedName (FIRST STRONG ISOLATE ... POP
        // DIRECTIONAL ISOLATE) so its direction cannot spill.
        private val ISOLATE = Char(0x2068)
        private val POP_ISOLATE = Char(0x2069)
        private fun isolated(title: String) = if (title == GENERIC_TITLE) title else "$ISOLATE$title$POP_ISOLATE"
    }
}
