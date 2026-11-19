package dev.chatproject.chat_app

import android.content.Context

// The JNI half of the Rust core (app/rust/src/android_jni.rs): the VM and application context
// for iroh's DNS, and the Keystore-backed account key protector. ChatApplication calls it once
// per process, before any activity, push receiver or worker reaches the core. Repeated calls
// answer true. R8 keeps these native method names (proguard-rules.pro).
object NativeInit {
    private external fun initializeNetworkContext(context: Context): Boolean
    private external fun initializeAccountKeyStore(store: AccountKeyStore): Boolean

    fun initialize(context: Context) {
        check(initializeNetworkContext(context.applicationContext)) { "Network context initialization failed" }
        check(initializeAccountKeyStore(AccountKeyStore(context.applicationContext))) {
            "Account key protection initialization failed"
        }
    }
}
