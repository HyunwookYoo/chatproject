# JNA looks classes and fields up by name, and Flutter's release build turns on R8.
-keep class com.sun.jna.** { *; }
-keep class * extends com.sun.jna.** { *; }
-keep class uniffi.** { *; }
-keep class dev.chatproject.chat_app.AccountKeyStore { *; }
-dontwarn java.awt.**
-keepclasseswithmembernames class dev.chatproject.chat_app.MainActivity {
    native <methods>;
}
# ML Kit discovers these registrars reflectively; the pinned Firebase consumer
# rule keeps classes but omits their no-argument constructors under R8 full mode.
-keep class * implements com.google.firebase.components.ComponentRegistrar {
    void <init>();
}
