package dev.chatproject.chat_app

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.pm.ApplicationInfo
import kotlin.concurrent.thread

// Debug builds only (src/debug; tools/ci/check_release_hooks.py rejects it in a Release APK): a
// fake FCM data message for tests without Firebase (spec 11). Its string extras t, e, w and r
// become the data map PushService takes, and it goes through the same handling.
//   adb shell am broadcast -n dev.chatproject.chat_app/.TestPushReceiver \
//     -a dev.chatproject.chat_app.TEST_PUSH --es t <tag> --es e <ciphertext>
class TestPushReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if ((context.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) == 0) return
        if (intent.action != ACTION) return
        val data = KEYS.mapNotNull { key -> intent.getStringExtra(key)?.let { key to it } }.toMap()
        // The core call blocks: off the main thread, with the broadcast kept open until it ends.
        val pending = goAsync()
        thread(name = "test-push") {
            try {
                PushService.handle(context.applicationContext, data)
            } finally {
                pending.finish()
            }
        }
    }

    private companion object {
        const val ACTION = "dev.chatproject.chat_app.TEST_PUSH"
        val KEYS = listOf("t", "e", "w", "r")
    }
}
