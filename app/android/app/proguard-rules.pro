# JNA looks classes and fields up by name, and Flutter's release build turns on R8.
-keep class com.sun.jna.** { *; }
-keep class * extends com.sun.jna.** { *; }
-keep class uniffi.** { *; }
-keep class dev.chatproject.chat_app.AccountKeyStore { *; }
-dontwarn java.awt.**
# JNI binds NativeInit's native methods by class and method name (app/rust/src/android_jni.rs).
-keepclasseswithmembernames class dev.chatproject.chat_app.NativeInit {
    native <methods>;
}
# Firebase starts the push receiver by its manifest name, and WorkManager makes workers by the
# class names its database keeps across app updates.
-keep class dev.chatproject.chat_app.PushService
-keep class * extends androidx.work.ListenableWorker {
    public <init>(android.content.Context, androidx.work.WorkerParameters);
}
# ML Kit discovers these registrars reflectively; the pinned Firebase consumer
# rule keeps classes but omits their no-argument constructors under R8 full mode.
-keep class * implements com.google.firebase.components.ComponentRegistrar {
    void <init>();
}
