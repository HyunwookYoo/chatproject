plugins {
    id("com.android.application")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

android {
    namespace = "dev.chatproject.chat_app"
    compileSdk = flutter.compileSdkVersion
    ndkVersion = flutter.ndkVersion

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    defaultConfig {
        // TODO: Specify your own unique Application ID (https://developer.android.com/studio/build/application-id.html).
        applicationId = "dev.chatproject.chat_app"
        // You can update the following values to match your application needs.
        // For more information, see: https://flutter.dev/to/review-gradle-config.
        minSdk = 26
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
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

// Firebase's app id comes from google-services.json, which the user provides (M7 spec 13) and git
// never holds. Without it the app builds and runs: Firebase stays uninitialized, PushService gets
// no token, and pushes arrive only through the debug TestPushReceiver.
if (file("google-services.json").exists()) {
    apply(plugin = "com.google.gms.google-services")
}

// PushReceiverTest's mismatched binding (M1a carry-forward 2): the generated UniFFI binding in a
// package of its own, with its first API checksum off by one. Against the real library its
// uniffiEnsureInitialized must refuse.
abstract class BrokenBindingTask : DefaultTask() {
    @get:InputFile
    abstract val binding: RegularFileProperty

    @get:OutputDirectory
    abstract val outputDir: DirectoryProperty

    @TaskAction
    fun write() {
        val text = binding.get().asFile.readText()
        val checksum = Regex("""and 0xFFFF\) != (\d+)\)""").find(text)
            ?: throw GradleException("No UniFFI API checksum in the binding")
        val value = checksum.groups[1]!!
        val broken = text.replaceRange(value.range, ((value.value.toInt() + 1) % 65536).toString())
        val packageLine = Regex("""^package uniffi\.chat_ffi(\r?)$""", RegexOption.MULTILINE)
        if (packageLine.findAll(broken).count() != 1) throw GradleException("Unexpected UniFFI binding package")
        val out = outputDir.get().asFile.resolve("uniffi/chat_ffi_broken/chat_ffi.kt")
        out.parentFile.mkdirs()
        out.writeText(broken.replace(packageLine, "package uniffi.chat_ffi_broken$1"))
    }
}

val brokenBinding = tasks.register<BrokenBindingTask>("brokenUniffiBinding") {
    binding.set(layout.projectDirectory.file("src/main/kotlin/uniffi/chat_ffi/chat_ffi.kt"))
    outputDir.set(layout.buildDirectory.dir("generated/brokenBinding"))
}

androidComponents {
    onVariants { variant ->
        variant.androidTest?.sources?.kotlin?.addGeneratedSourceDirectory(brokenBinding, BrokenBindingTask::outputDir)
    }
}

dependencies {
    implementation("net.java.dev.jna:jna:5.17.0@aar")
    // FCM data messages for PushService, without a Flutter plugin (M7 spec 5.4).
    implementation("com.google.firebase:firebase-messaging:26.0.2")
    // The push_renew and push_fetch workers.
    implementation("androidx.work:work-runtime-ktx:2.12.0")
    androidTestImplementation("androidx.test:runner:1.3.0")
    androidTestImplementation("androidx.test.ext:junit:1.1.2")
}
