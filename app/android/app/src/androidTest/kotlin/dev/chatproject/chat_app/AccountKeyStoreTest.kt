package dev.chatproject.chat_app

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import java.security.SecureRandom

@RunWith(AndroidJUnit4::class)
class AccountKeyStoreTest {
    private external fun nativeWorkerRoundTrip(id: ByteArray): Boolean
    private val context = InstrumentationRegistry.getInstrumentation().targetContext
    private fun id() = ByteArray(16).also { SecureRandom().nextBytes(it) }
    private fun ownedPathFixture(): java.io.File =
        java.io.File(context.cacheDir, "owned-path-test-${java.util.UUID.randomUUID()}").also { assertTrue(it.mkdir()) }
    private fun deniedOwnedPath(files: java.io.File, path: String) {
        try {
            ownedTestDataDir(files, path)
            fail("Escaping or non-child test directory accepted")
        } catch (_: IllegalStateException) { /* Expected containment refusal. */ }
    }
    @Test fun ownedTestPathAcceptsOnlyAbsoluteChildren() {
        val root = ownedPathFixture()
        val files = java.io.File(root, "files").also { assertTrue(it.mkdir()) }
        val child = java.io.File(files, "e2e/run/store")
        assertEquals(child.canonicalPath, ownedTestDataDir(files, child.path))
        deniedOwnedPath(files, "e2e/run/store")
        deniedOwnedPath(files, java.io.File(files, "e2e").path)
        deniedOwnedPath(files, java.io.File(files, "e2e-other/store").path)
    }
    @Test fun ownedTestPathRejectsChildSymlinkEscape() {
        val root = ownedPathFixture()
        val files = java.io.File(root, "files").also { assertTrue(it.mkdir()) }
        val owned = java.io.File(files, "e2e").also { assertTrue(it.mkdir()) }
        val outside = java.io.File(root, "outside").also { assertTrue(it.mkdir()) }
        android.system.Os.symlink(outside.path, java.io.File(owned, "link").path)
        deniedOwnedPath(files, java.io.File(owned, "link/store").path)
    }
    @Test fun ownedTestPathRejectsRootSymlinkEscape() {
        val root = ownedPathFixture()
        val files = java.io.File(root, "files").also { assertTrue(it.mkdir()) }
        val outside = java.io.File(root, "outside").also { assertTrue(it.mkdir()) }
        android.system.Os.symlink(outside.path, java.io.File(files, "e2e").path)
        deniedOwnedPath(files, java.io.File(files, "e2e/run/store").path)
    }

    @Test fun nativeWorkerUsesCachedHelperAndThreadLocalEnvironment() {
        System.loadLibrary("chat_ffi")
        lateinit var activity: MainActivity
        InstrumentationRegistry.getInstrumentation().runOnMainSync { activity = MainActivity() }
        val network = MainActivity::class.java.getDeclaredMethod("initializeNetworkContext", android.content.Context::class.java)
        network.isAccessible = true
        assertEquals(true, network.invoke(activity, context))
        val initialize = MainActivity::class.java.getDeclaredMethod("initializeAccountKeyStore", AccountKeyStore::class.java)
        initialize.isAccessible = true
        assertEquals(true, initialize.invoke(activity, AccountKeyStore(context)))
        assertTrue(nativeWorkerRoundTrip(id()))
    }
    private fun wrap(store: AccountKeyStore, id: ByteArray): ByteArray {
        val output = ByteArray(16384)
        val length = intArrayOf(-1)
        assertEquals(0, store.protect(id, ByteArray(32) { 0x3a }, output, length))
        return output.copyOf(length[0])
    }

    @Test fun roundTripScopeTamperAndDelete() {
        val store = AccountKeyStore(context)
        val id = id()
        try {
            val first = wrap(store, id)
            val level = store.protectionLevel(id)
            assertTrue(level in setOf("strongbox", "trusted_environment", "software", "hardware", "unknown"))
            println("Measured account wrapping protection: $level")
            assertFalse(first.contentEquals(wrap(store, id)))
            val output = ByteArray(32)
            val length = intArrayOf(-1)
            assertEquals(0, AccountKeyStore(context).unprotect(id, first, output, length))
            assertEquals(32, length[0])
            assertArrayEquals(ByteArray(32) { 0x3a }, output)
            val other = id()
            try {
                wrap(store, other)
                assertEquals(3, store.unprotect(other, first, output, length))
                assertEquals(0, length[0])
            } finally { store.delete(other) }
            first[first.lastIndex] = (first.last().toInt() xor 1).toByte()
            assertEquals(3, store.unprotect(id, first, output, length))
            val valid = wrap(store, id)
            assertEquals(0, store.delete(id))
            assertEquals(0, store.delete(id))
            assertEquals(2, store.unprotect(id, valid, output, length))
            assertEquals("missing", store.protectionLevel(id))
            assertEquals(0, length[0])
        } finally { store.delete(id) }
    }

    @Test fun noBackupRootPreservesLegacyIncompleteState() {
        val store = AccountKeyStore(context)
        val legacy = java.io.File(context.noBackupFilesDir, "legacy-path-test-${java.util.UUID.randomUUID()}")
        assertTrue(legacy.mkdir())
        try {
            assertEquals(java.io.File(context.noBackupFilesDir, "account").canonicalPath, store.storageRoot(legacy.path))
            for (name in listOf("store.id", "key.wrap", "account.bin", ".store.id.tmp", ".key.wrap.tmp", ".account.bin.tmp",
                "db.sqlite3", "db.sqlite3-wal", "db.sqlite3-shm", "db.keys", "db.keys.pending",
                ".db.keys.tmp", ".db.keys.pending.tmp", "storage.mode", ".storage.mode.tmp",
                "migration.prep", ".migration.prep.tmp", "migration.checkpoint", ".migration.checkpoint.tmp")) {
                val artifact = java.io.File(legacy, name)
                artifact.writeBytes(byteArrayOf(1))
                try {
                    assertEquals("Preserve existing path for $name", legacy.canonicalPath, store.storageRoot(legacy.path))
                } finally { assertTrue(artifact.delete()) }
            }
        } finally { assertTrue(legacy.delete()) }
    }

    @Test fun invalidInputsAndCapacityDoNotCreateKeys() {
        val store = AccountKeyStore(context)
        val id = id()
        val length = intArrayOf(99)
        assertEquals(5, store.protect(id, ByteArray(32), ByteArray(1), length))
        assertEquals(0, length[0])
        assertEquals(3, store.unprotect(id, ByteArray(16385), ByteArray(32), length))
        assertEquals(3, store.protect(ByteArray(15), ByteArray(32), ByteArray(16384), length))
        assertEquals(3, store.protect(id, ByteArray(31), ByteArray(16384), length))
        assertEquals(2, store.unprotect(id, byteArrayOf(1) + ByteArray(60), ByteArray(32), length))
    }

    // Separate instrumentation runs prove persistence across process death. Only synthetic keys.
    @Test fun restartPersistence() {
        val phase = InstrumentationRegistry.getArguments().getString("accountPhase")
        assumeTrue("Restart persistence requires separate write/read test processes", phase != null)
        assertTrue("accountPhase must be write or read", phase == "write" || phase == "read")
        val file = java.io.File(context.noBackupFilesDir, "account-test-restart")
        val store = AccountKeyStore(context)
        if (phase == "write") {
            val id = id()
            file.writeBytes(id + wrap(store, id))
        } else {
            val data = file.readBytes()
            val id = data.copyOfRange(0, 16)
            try {
                val output = ByteArray(32)
                assertEquals(0, store.unprotect(id, data.copyOfRange(16, data.size), output, intArrayOf(0)))
                assertArrayEquals(ByteArray(32) { 0x3a }, output)
            } finally { store.delete(id); file.delete() }
        }
    }
}
