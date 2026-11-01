package dev.chatproject.chat_app

import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyInfo
import android.security.keystore.KeyProperties
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.StrongBoxUnavailableException
import android.security.keystore.UserNotAuthenticatedException
import java.io.File
import java.security.KeyStore
import java.security.UnrecoverableKeyException
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.SecretKeyFactory
import javax.crypto.spec.GCMParameterSpec

class AccountKeyStore(private val context: Context) {
    private fun store() = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    private fun alias(id: ByteArray) = "ChatProject.account." + id.joinToString("") { "%02x".format(it.toInt() and 255) }

    private fun createKey(alias: String): SecretKey {
        fun generate(strongBox: Boolean): SecretKey {
            val spec = KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setKeySize(256).setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setRandomizedEncryptionRequired(true)
            if (Build.VERSION.SDK_INT >= 28) spec.setIsStrongBoxBacked(strongBox)
            return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").run {
                init(spec.build()); generateKey()
            }
        }
        if (Build.VERSION.SDK_INT >= 28 && context.packageManager.hasSystemFeature(PackageManager.FEATURE_STRONGBOX_KEYSTORE)) {
            try { return generate(true) }
            catch (_: StrongBoxUnavailableException) { /* Only unsupported StrongBox may fall back. */ }
        }
        return generate(false)
    }

    private inline fun guarded(block: () -> Int): Int = try { block() }
        catch (_: UserNotAuthenticatedException) { 1 }
        catch (_: KeyPermanentlyInvalidatedException) { 2 }
        catch (_: UnrecoverableKeyException) { 2 }
        catch (_: AEADBadTagException) { 3 }
        catch (_: Exception) { 4 }

    @Synchronized
    fun protect(id: ByteArray, input: ByteArray, out: ByteArray, length: IntArray): Int {
        if (length.size != 1) return 3
        length[0] = 0
        if (id.size != 16 || input.size != 32) return 3
        if (out.size < 61 || out.size > 16384) return 5
        return guarded {
            val name = alias(id)
            val key = store().getKey(name, null) as SecretKey? ?: createKey(name)
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.ENCRYPT_MODE, key)
            cipher.updateAAD(id)
            val encrypted = cipher.doFinal(input)
            if (cipher.iv.size != 12 || encrypted.size != 48) return@guarded 4
            out[0] = 1
            cipher.iv.copyInto(out, 1)
            encrypted.copyInto(out, 13)
            length[0] = 61
            0
        }
    }

    @Synchronized
    fun unprotect(id: ByteArray, input: ByteArray, out: ByteArray, length: IntArray): Int {
        if (length.size != 1) return 3
        length[0] = 0
        if (id.size != 16 || input.size != 61 || input[0] != 1.toByte()) return 3
        if (out.size < 32 || out.size > 16384) return 5
        return guarded {
            // Reading never creates a key. A missing alias is a recoverable storage error.
            val key = store().getKey(alias(id), null) as SecretKey? ?: return@guarded 2
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(128, input, 1, 12))
            cipher.updateAAD(id)
            val plaintext = cipher.doFinal(input, 13, 48)
            try {
                if (plaintext.size != 32) return@guarded 3
                plaintext.copyInto(out)
                length[0] = 32
                0
            } finally { plaintext.fill(0) }
        }
    }

    @Synchronized
    fun delete(id: ByteArray): Int {
        if (id.size != 16) return 3
        return guarded { store().deleteEntry(alias(id)); 0 }
    }

    // Reports measured properties of an existing key, never the device marketing claim.
    fun protectionLevel(id: ByteArray): String {
        require(id.size == 16)
        val key = store().getKey(alias(id), null) as SecretKey? ?: return "missing"
        val info = SecretKeyFactory.getInstance(key.algorithm, "AndroidKeyStore").getKeySpec(key, KeyInfo::class.java) as KeyInfo
        return if (Build.VERSION.SDK_INT >= 31) {
            when (info.securityLevel) {
                KeyProperties.SECURITY_LEVEL_STRONGBOX -> "strongbox"
                KeyProperties.SECURITY_LEVEL_TRUSTED_ENVIRONMENT -> "trusted_environment"
                KeyProperties.SECURITY_LEVEL_SOFTWARE -> "software"
                else -> "unknown"
            }
        } else {
            @Suppress("DEPRECATION")
            if (info.isInsideSecureHardware) "hardware" else "software"
        }
    }

    fun storageRoot(existing: String?): String {
        // Preserve the path; Rust alone validates incomplete SQL material.
        val artifacts = listOf("store.id", "key.wrap", "account.bin", ".store.id.tmp", ".key.wrap.tmp", ".account.bin.tmp",
            "db.sqlite3", "db.sqlite3-wal", "db.sqlite3-shm", "db.keys", "db.keys.pending",
            ".db.keys.tmp", ".db.keys.pending.tmp", "storage.mode", ".storage.mode.tmp",
            "migration.prep", ".migration.prep.tmp", "migration.checkpoint", ".migration.checkpoint.tmp")
        val old = existing?.let(::File)
        val root = if (old != null && artifacts.any { File(old, it).exists() }) old else File(context.noBackupFilesDir, "account")
        check(root.isDirectory || root.mkdirs())
        return root.canonicalPath
    }
}
