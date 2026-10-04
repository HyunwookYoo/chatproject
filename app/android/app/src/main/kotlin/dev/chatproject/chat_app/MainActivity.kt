package dev.chatproject.chat_app

import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import uniffi.chat_ffi.nativeInstanceId
import uniffi.chat_ffi.uniffiEnsureInitialized

class MainActivity : FlutterActivity() {
    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
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
