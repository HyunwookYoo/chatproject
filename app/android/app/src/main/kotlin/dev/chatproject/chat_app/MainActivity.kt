package dev.chatproject.chat_app

import android.content.Context
import android.content.Intent
import android.content.pm.ApplicationInfo
import android.os.Bundle
import android.view.WindowManager
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import uniffi.chat_ffi.nativeInstanceId
import uniffi.chat_ffi.uniffiEnsureInitialized

// Only the debug entrypoint below accepts this owned fixture directory.
internal fun ownedTestDataDir(filesDir: java.io.File, requestedPath: String): String {
    // Resolve the app parent, but not an e2e leaf that could itself be a symlink.
    val ownedRoot = java.io.File(filesDir.canonicalFile, "e2e")
    val requested = java.io.File(requestedPath)
    val canonical = requested.canonicalFile
    check(requested.isAbsolute && canonical.path.startsWith(ownedRoot.path + java.io.File.separator)) {
        "Invalid owned test data directory"
    }
    return canonical.path
}

class MainActivity : FlutterActivity() {
    private external fun initializeNetworkContext(context: Context): Boolean
    private external fun initializeAccountKeyStore(store: AccountKeyStore): Boolean
    private lateinit var accountKeys: AccountKeyStore
    private var accountChannel: MethodChannel? = null
    private var pendingInvite: String? = null
    private fun receiveInvite(intent: Intent?) {
        val value = intent?.dataString ?: return
        if (!value.startsWith("chatproject:identity-invite/")) return
        pendingInvite = value
        accountChannel?.invokeMethod("identityInvite", value, object : MethodChannel.Result {
            override fun success(result: Any?) { if (result == true && pendingInvite == value) pendingInvite = null }
            override fun error(code: String, message: String?, details: Any?) {}
            override fun notImplemented() {}
        })
    }
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        receiveInvite(intent)
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        System.loadLibrary("chat_ffi")
        check(initializeNetworkContext(applicationContext)) { "Network context initialization failed" }
        accountKeys = AccountKeyStore(applicationContext)
        check(initializeAccountKeyStore(accountKeys)) { "Account key protection initialization failed" }
        receiveInvite(intent)
        super.onCreate(savedInstanceState)
    }

    override fun getDartEntrypointArgs(): List<String>? {
        val args = super.getDartEntrypointArgs()
        if ((applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) != 0) {
            val controlUrl = intent.getStringExtra("chat_control_url")
            val name = intent.getStringExtra("chat_name")
            val requestedDir = intent.getStringExtra("chat_data_dir")
            val dataDir = requestedDir?.let { ownedTestDataDir(filesDir, it) }
            if (controlUrl != null || name != null || dataDir != null) {
                return (args ?: emptyList()) + buildList {
                    if (controlUrl != null) addAll(listOf("--control-url", controlUrl))
                    if (name != null) addAll(listOf("--name", name))
                    if (dataDir != null) addAll(listOf("--data-dir", dataDir))
                }
            }
        }
        return args
    }

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        accountChannel = MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "chat_app/account_storage")
        accountChannel?.setMethodCallHandler { call, result ->
                try {
                    when (call.method) {
                        "takeIdentityInvite" -> { result.success(pendingInvite); pendingInvite = null }
                        "storageRoot" -> result.success(accountKeys.storageRoot(call.argument<String>("existing")))
                        "protectionLevel" -> {
                            val hex = call.argument<String>("storeId") ?: ""
                            require(hex.matches(Regex("[0-9a-f]{32}")))
                            val id = hex.chunked(2).map { it.toInt(16).toByte() }.toByteArray()
                            result.success(accountKeys.protectionLevel(id))
                        }
                        "setSensitiveScreen" -> {
                            val sensitive = call.arguments as? Boolean ?: throw IllegalArgumentException()
                            if (sensitive) window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
                            else window.clearFlags(WindowManager.LayoutParams.FLAG_SECURE)
                            result.success(null)
                        }
                        else -> result.notImplemented()
                    }
                } catch (_: Exception) {
                    result.error("storage_unavailable", "계정 저장 공간을 열 수 없어요.", null)
                }
            }
        if ((applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) != 0) {
            // Lets Dart check that Kotlin reaches the same Rust core (design 12.3).
            MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "chat_app/native_core")
                .setMethodCallHandler { call, result ->
                    when (call.method) {
                        "instanceId" -> {
                            // The generated functions never run UniFFI's contract-version and checksum check; this does.
                            uniffiEnsureInitialized()
                            result.success(nativeInstanceId())
                        }
                        else -> result.notImplemented()
                    }
                }
        }
    }
}
