package dev.chatproject.chat_app

import android.Manifest
import android.app.Notification
import android.app.NotificationManager
import android.os.Build
import android.os.SystemClock
import android.service.notification.StatusBarNotification
import android.util.Base64
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.nio.ByteBuffer
import java.security.SecureRandom
import uniffi.chat_ffi.NativeNotifyItem
import uniffi.chat_ffi.NativePushOutcome
import uniffi.chat_ffi.nativeCoreVersion
import uniffi.chat_ffi.nativeInstanceId
import uniffi.chat_ffi.nativeStart
import uniffi.chat_ffi.uniffiEnsureInitialized

// The push receiver without Flutter (spec 5.4, 5.5). The instrumentation process is the app's
// own, so ChatApplication.onCreate has run: library, JNI, binding check and native notifier.
// A push that the core opens into a conversation's text needs a second account; Task 14's
// e2e scenario covers that path. Only synthetic tags and payloads appear here.
@RunWith(AndroidJUnit4::class)
class PushReceiverTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context = instrumentation.targetContext
    private val manager = context.getSystemService(NotificationManager::class.java)

    @Before fun allowNotifications() {
        if (Build.VERSION.SDK_INT >= 33) {
            instrumentation.uiAutomation.grantRuntimePermission(context.packageName, Manifest.permission.POST_NOTIFICATIONS)
        }
        assertTrue(manager.areNotificationsEnabled())
    }

    private fun random(size: Int) = ByteArray(size).also { SecureRandom().nextBytes(it) }
    private fun base64Url(bytes: ByteArray) = Base64.encodeToString(bytes, Base64.URL_SAFE or Base64.NO_PADDING or Base64.NO_WRAP)
    private fun ours() = manager.activeNotifications.filter { it.tag == Notifications.TAG || it.tag == Notifications.GENERIC_TAG }
    private fun message(id: Int) = manager.activeNotifications.firstOrNull { it.tag == Notifications.TAG && it.id == id }
    private fun generic() = manager.activeNotifications.firstOrNull { it.tag == Notifications.GENERIC_TAG }
    private fun text(posted: StatusBarNotification, key: String) = posted.notification.extras.getCharSequence(key)?.toString()

    // NotificationManager posts asynchronously; what should show is waited for.
    private fun <T : Any> waitFor(what: () -> T?): T {
        val end = SystemClock.elapsedRealtime() + 5_000
        while (true) {
            what()?.let { return it }
            assertTrue("notification did not show", SystemClock.elapsedRealtime() < end)
            SystemClock.sleep(50)
        }
    }

    // What should not show is looked for after the native notifier thread had time to post.
    private fun settled() = SystemClock.sleep(1_000)

    @Test fun unknownTagPostsNothing() {
        manager.cancelAll()
        // A withdrawn or deleted conversation's tag (handles live 30 days): no notification.
        // With an account the core answers UNKNOWN_TAG; with none (CI's fresh emulator, or an
        // account that was reset) it refuses with AccountRestricted, and no tag can be ours.
        val tag = base64Url(random(16))
        for (data in listOf(mapOf("t" to tag, "e" to base64Url(random(200))), mapOf("t" to tag, "w" to "1"))) {
            val outcome = PushService.handle(context, data)
            assertTrue("$outcome", outcome == null || outcome == NativePushOutcome.UNKNOWN_TAG)
        }
        // The core started and opened its account folder all the same.
        assertNotNull(nativeInstanceId())
        settled()
        assertEquals(emptyList<StatusBarNotification>(), ours())
    }

    @Test fun storageRootSameFromNativeAndFlutter() {
        // M1a carry-forward 3: Dart's getApplicationSupportDirectory()/core and Kotlin's
        // filesDir/core reach one canonical root, in either form of the app's data path.
        val store = AccountKeyStore(context)
        val forms = listOf("/data/data/${context.packageName}", "/data/user/0/${context.packageName}")
        val root = NativeCore.storageRoot(context)
        assertEquals(root, store.storageRoot(File(context.filesDir, "core").path))
        for (form in forms) assertEquals(form, root, store.storageRoot("$form/files/core"))
        // A root that already holds account files stays the root, in either form.
        val legacy = File(context.filesDir, "push-root-test-${java.util.UUID.randomUUID()}").also { assertTrue(it.mkdir()) }
        try {
            File(legacy, "store.id").writeBytes(byteArrayOf(1))
            for (form in forms) assertEquals(form, legacy.canonicalPath, store.storageRoot("$form/files/${legacy.name}"))
        } finally { assertTrue(legacy.deleteRecursively()) }

        // The receiver's start and a second start, from either form: one core.
        val started = NativeCore.ensureStarted(context)
        assertEquals(nativeCoreVersion(), started.version)
        for (form in forms) {
            assertEquals(started.instanceId, nativeStart(store.storageRoot("$form/files/core"), null).instanceId)
        }
        assertEquals(started.instanceId, nativeInstanceId())
    }

    @Test fun brokenBindingFailsEnsureInitialized() {
        // M1a carry-forward 2: uniffi.chat_ffi_broken is the generated binding with one API
        // checksum changed (app/build.gradle.kts). Against the real library it must refuse at
        // uniffiEnsureInitialized, and every later call must fail too.
        repeat(2) { attempt ->
            val error = try {
                uniffi.chat_ffi_broken.uniffiEnsureInitialized()
                null
            } catch (thrown: Throwable) { thrown }
            assertNotNull("mismatched binding initialized (attempt $attempt)", error)
            if (attempt == 0) {
                assertTrue(error.toString(), generateSequence(error) { it.cause }
                    .any { it is RuntimeException && it.message?.contains("checksum mismatch") == true })
            }
        }
        // The app's own binding is unaffected.
        uniffiEnsureInitialized()
        assertTrue(nativeCoreVersion().isNotEmpty())
    }

    @Test fun notifierPostsUpdatesAndCancels() {
        manager.cancelAll()
        val notifier = Notifications(context)
        val conversation = random(32)
        // The conversation's first 4 bytes, big-endian (Dart's notificationId, Task 9).
        val id = ByteBuffer.wrap(conversation, 0, 4).int
        fun item(body: String, update: Boolean = false, silent: Boolean = false, cancel: Boolean = false) =
            NativeNotifyItem(id, if (cancel) "" else "앨리스", if (cancel) "" else body, update, silent, cancel, conversation)

        // A count update of a notification that is not shown posts nothing (M6 Ruling 31).
        notifier.notify(listOf(item("안녕하세요", update = true)))
        settled()
        assertNull(message(id))

        notifier.notify(listOf(item("안녕하세요")))
        val posted = waitFor { message(id) }
        assertEquals(Notifications.CHANNEL, posted.notification.channelId)
        assertEquals("${Char(0x2068)}앨리스${Char(0x2069)}", text(posted, Notification.EXTRA_TITLE))
        assertEquals("안녕하세요", text(posted, Notification.EXTRA_TEXT))
        assertNotNull(posted.notification.contentIntent)

        // A later text of the same conversation changes it silently.
        notifier.notify(listOf(item("두 번째 외 1개", update = true)))
        val updated = waitFor { message(id)?.takeIf { text(it, Notification.EXTRA_TEXT) == "두 번째 외 1개" } }
        assertNotEquals(0, updated.notification.flags and Notification.FLAG_ONLY_ALERT_ONCE)

        // Read on another of my devices: the core's cancel after a read clear (spec 7).
        notifier.notify(listOf(item("", cancel = true)))
        waitFor { if (message(id) == null) Unit else null }

        // Final review 1: the text a dismissed generic notification alerted for is posted
        // without alerting (Ruling R13), even though nothing is shown.
        notifier.notify(listOf(item("늦게 온 글", silent = true)))
        val silent = waitFor { message(id) }
        assertEquals("늦게 온 글", text(silent, Notification.EXTRA_TEXT))
        assertEquals(Notification.GROUP_ALERT_SUMMARY, silent.notification.groupAlertBehavior)
        manager.cancelAll()
    }

    @Test fun pushTheCoreCannotTakeStillNotifies() {
        manager.cancelAll()
        // A high-priority push the core refuses still ends in a visible notification.
        assertNull(PushService.handle(context, mapOf("t" to base64Url(random(16)), "e" to "not base64!")))
        assertEquals("새 메시지", text(waitFor { generic() }, Notification.EXTRA_TITLE))
        manager.cancelAll()
        // A read clear is normal priority: nothing to show.
        assertNull(PushService.handle(context, mapOf("t" to base64Url(random(16)), "r" to "2")))
        settled()
        assertEquals(emptyList<StatusBarNotification>(), ours())
    }

    // A separate run: CI stopped the app, then sent a push the core refuses to the debug
    // TestPushReceiver, which started the process with no activity (pushPhase=cold).
    @Test fun coldPushShowedNotification() {
        assumeTrue("Cold push needs CI to stop the app and send the push first",
            InstrumentationRegistry.getArguments().getString("pushPhase") == "cold")
        assertEquals("새 메시지", text(waitFor { generic() }, Notification.EXTRA_TITLE))
        manager.cancelAll()
    }
}
