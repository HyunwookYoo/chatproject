package dev.chatproject.chat_app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder

// Mailbox mode (spec 8.2): a remoteMessaging foreground service that only keeps
// this process alive. The core it keeps running is the one Dart already started
// in this process; the service starts no engine and no core of its own.
class MailboxService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (pendingStarts > 0) pendingStarts--
        // Every startForegroundService must reach startForeground, even one that
        // is already being turned off again: stopping before it crashes the app.
        val notification = connectionNotification(this)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_REMOTE_MESSAGING)
        } else {
            // Before API 34 there is no remoteMessaging type; this overload uses
            // the manifest's type from API 29 and none before.
            startForeground(NOTIFICATION_ID, notification)
        }
        running = this
        if (!wanted && pendingStarts == 0) stopSelf()
        // A restart without the app would run with no core; the mailbox comes
        // back when the user opens the app (no boot or sticky restart).
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        if (running === this) running = null
        super.onDestroy()
    }

    companion object {
        const val CHANNEL_ID = "connection_status"
        private const val NOTIFICATION_ID = 1

        // Main thread only: the method channel and the service callbacks.
        private var wanted = false
        private var pendingStarts = 0

        var running: MailboxService? = null
            private set

        // Android allows this only while the app is on screen; it throws otherwise.
        fun start(context: Context) {
            context.startForegroundService(Intent(context, MailboxService::class.java))
            wanted = true
            pendingStarts++
        }

        fun stop(context: Context) {
            wanted = false
            // A pending start stops itself right after startForeground.
            if (pendingStarts == 0) context.stopService(Intent(context, MailboxService::class.java))
        }

        private fun connectionChannel(context: Context) {
            val channel = NotificationChannel(CHANNEL_ID, "연결 상태", NotificationManager.IMPORTANCE_MIN).apply {
                setSound(null, null)
                enableVibration(false)
                enableLights(false)
                setShowBadge(false)
            }
            context.getSystemService(NotificationManager::class.java).createNotificationChannel(channel)
        }

        private fun connectionNotification(context: Context): Notification {
            connectionChannel(context)
            val open = PendingIntent.getActivity(
                context, 0, Intent(context, MainActivity::class.java),
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
            return Notification.Builder(context, CHANNEL_ID)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle("이 기기를 메일박스로 쓰는 중")
                .setContentIntent(open)
                .setOngoing(true)
                .setShowWhen(false)
                .setCategory(Notification.CATEGORY_SERVICE)
                .build()
        }
    }
}
