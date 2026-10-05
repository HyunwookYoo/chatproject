# JNA looks classes and fields up by name, and Flutter's release build turns on R8.
-keep class com.sun.jna.** { *; }
-keep class * extends com.sun.jna.** { *; }
-keep class uniffi.** { *; }
-dontwarn java.awt.**
