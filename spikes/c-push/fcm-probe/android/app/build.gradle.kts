plugins {
    id("com.android.application")
    id("com.google.gms.google-services")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

// google-services.json (gitignored): copy the real one from spikes/c-push/secrets if it exists,
// otherwise write a DUMMY so the build can be verified. FCM does not work with the DUMMY.
val realGoogleServices = file("../../../secrets/google-services.json")
if (realGoogleServices.exists()) {
    realGoogleServices.copyTo(file("google-services.json"), overwrite = true)
    logger.lifecycle("google-services.json: REAL (copied from secrets/)")
} else {
    file("google-services.json").writeText(
        """
        {
          "_comment": "DUMMY written by app/build.gradle.kts because secrets/google-services.json is missing. FCM will NOT work.",
          "project_info": {
            "project_number": "000000000000",
            "project_id": "dummy-not-a-real-project",
            "storage_bucket": "dummy-not-a-real-project.appspot.com"
          },
          "client": [
            {
              "client_info": {
                "mobilesdk_app_id": "1:000000000000:android:0000000000000000",
                "android_client_info": { "package_name": "dev.chatproject.spike.fcmprobe" }
              },
              "oauth_client": [],
              "api_key": [ { "current_key": "AIzaDUMMY_NOT_A_REAL_KEY_00000000000000" } ],
              "services": { "appinvite_service": { "other_platform_oauth_client": [] } }
            }
          ],
          "configuration_version": "1"
        }
        """.trimIndent(),
    )
    logger.warn("google-services.json: DUMMY (secrets/google-services.json missing) - FCM will NOT work")
}

android {
    namespace = "dev.chatproject.spike.fcmprobe"
    compileSdk = flutter.compileSdkVersion
    ndkVersion = flutter.ndkVersion

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    defaultConfig {
        // TODO: Specify your own unique Application ID (https://developer.android.com/studio/build/application-id.html).
        applicationId = "dev.chatproject.spike.fcmprobe"
        // You can update the following values to match your application needs.
        // For more information, see: https://flutter.dev/to/review-gradle-config.
        minSdk = 26 // notification channels without compat code
        targetSdk = flutter.targetSdkVersion
        versionCode = flutter.versionCode
        versionName = flutter.versionName
    }

    buildTypes {
        release {
            // TODO: Add your own signing config for the release build.
            // Signing with the debug keys for now, so `flutter run --release` works.
            signingConfig = signingConfigs.getByName("debug")
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17
    }
}

flutter {
    source = "../.."
}

dependencies {
    implementation(platform("com.google.firebase:firebase-bom:34.19.0"))
    implementation("com.google.firebase:firebase-messaging")
}
