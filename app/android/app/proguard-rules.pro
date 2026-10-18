# JNA looks classes and fields up by name, and Flutter's release build turns on R8.
-keep class com.sun.jna.** { *; }
-keep class * extends com.sun.jna.** { *; }
-keep class uniffi.** { *; }
-keep class dev.chatproject.chat_app.AccountKeyStore { *; }
-dontwarn java.awt.**
-keepclasseswithmembernames class dev.chatproject.chat_app.MainActivity {
    native <methods>;
}
