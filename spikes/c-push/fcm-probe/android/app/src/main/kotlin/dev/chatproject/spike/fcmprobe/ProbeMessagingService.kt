package dev.chatproject.spike.fcmprobe

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.util.Log
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import org.json.JSONObject
import java.io.File

const val TAG = "FcmProbe"
private const val CHANNEL = "probe"

/** sender/send.mjs reads files/token.txt through `adb shell run-as`. */
fun saveToken(context: Context, token: String) {
    File(context.filesDir, "token.txt").writeText(token)
    Log.i(TAG, "token $token")
}

class ProbeMessagingService : FirebaseMessagingService() {
    override fun onNewToken(token: String) = saveToken(this, token)

    override fun onMessageReceived(message: RemoteMessage) {
        val recvMs = System.currentTimeMillis()
        val data = message.data
        val bytes = data.entries.sumOf { it.key.toByteArray().size + it.value.toByteArray().size }
        val priority = priorityName(message.priority)
        val original = priorityName(message.originalPriority)
        appendRecv(
            JSONObject()
                .put("recv_ms", recvMs)
                .put("seq", data["seq"]?.toLongOrNull())
                .put("sent_ms", data["sent_ms"]?.toLongOrNull())
                .put("payload_bytes", bytes)
                .put("priority", priority)
                .put("originalPriority", original)
                .toString(),
        )

        // A high-priority message that shows no notification gets later ones downgraded to normal.
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel(CHANNEL, "FCM probe", NotificationManager.IMPORTANCE_DEFAULT))
        val notification = Notification.Builder(this, CHANNEL)
            .setSmallIcon(android.R.drawable.stat_notify_chat)
            .setContentTitle("FCM probe seq ${data["seq"]}")
            .setContentText("$priority (sent as $original), $bytes B")
            .build()
        // tag + id = sent_ms + seq: a new notification per message, so none is a rate-limited update.
        nm.notify(data["sent_ms"], data["seq"]?.toIntOrNull() ?: 0, notification)
    }

    override fun onDeletedMessages() =
        appendRecv(JSONObject().put("deleted", true).put("recv_ms", System.currentTimeMillis()).toString())

    private fun appendRecv(line: String) {
        File(filesDir, "recv.jsonl").appendText(line + "\n")
        Log.i(TAG, "recv $line")
    }

    private fun priorityName(p: Int) = when (p) {
        RemoteMessage.PRIORITY_HIGH -> "high"
        RemoteMessage.PRIORITY_NORMAL -> "normal"
        else -> "unknown"
    }
}
