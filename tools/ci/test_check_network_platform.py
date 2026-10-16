import pathlib
import plistlib
import tempfile
import unittest

from check_network_platform import check


ACTIVITY = """class MainActivity : FlutterActivity() {
    private external fun initializeNetworkContext(context: Context): Boolean
    override fun onCreate(savedInstanceState: Bundle?) {
        System.loadLibrary("chat_ffi")
        check(initializeNetworkContext(applicationContext)) { "Network context initialization failed" }
        super.onCreate(savedInstanceState)
    }
    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)
        if ((applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) != 0) {
            MethodChannel(flutterEngine.dartExecutor.binaryMessenger, "chat_app/native_core")
        }
    }
    override fun getDartEntrypointArgs(): List<String>? {
        if ((applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) != 0) {
            val controlUrl = intent.getStringExtra("chat_control_url")
            val name = intent.getStringExtra("chat_name")
            return listOf("--control-url", controlUrl, "--name", name)
        }
        return super.getDartEntrypointArgs()
    }
}
"""
JNI = """struct ContextState { vm: JavaVM, context: GlobalRef }
static JVM_PTR: OnceLock<usize> = OnceLock::new();
static CONTEXT: OnceLock<ContextState> = OnceLock::new();
static INIT: Mutex<()> = Mutex::new(());
pub extern "system" fn JNI_OnLoad(vm: *mut jni::sys::JavaVM, _reserved: *mut c_void) -> jint {
    if vm.is_null() { return JNI_ERR; }
    JVM_PTR.set(vm as usize);
    JNI_VERSION_1_6
}
pub extern "system" fn Java_dev_chatproject_chat_1app_MainActivity_initializeNetworkContext(
    env: JNIEnv, _this: JObject, context: JObject,
) -> jboolean {
    let _init = INIT.lock();
    if CONTEXT.get().is_some() { return JNI_TRUE; }
    if context.is_null() { return JNI_FALSE; }
    let ptr = JVM_PTR.get();
    let vm = JavaVM::from_raw(ptr);
    let global = env.new_global_ref(context);
    iroh::dns::install_android_jni_context(vm.get_java_vm_pointer().cast(), global.as_obj().as_raw().cast());
    CONTEXT.set(ContextState { vm, context: global });
    JNI_TRUE
}
"""
MANIFEST = '<manifest xmlns:android="http://schemas.android.com/apk/res/android"><uses-permission android:name="android.permission.INTERNET"/><uses-permission android:name="android.permission.ACCESS_NETWORK_STATE"/><application/></manifest>'
KEEP = '-keepclasseswithmembernames class dev.chatproject.chat_app.MainActivity { native <methods>; }'
PATHS = {
    "activity": "app/android/app/src/main/kotlin/dev/chatproject/chat_app/MainActivity.kt",
    "manifest": "app/android/app/src/main/AndroidManifest.xml",
    "keep": "app/android/app/proguard-rules.pro",
    "jni": "app/rust/src/android_jni.rs",
    "lib": "app/rust/src/lib.rs",
    "plist": "app/ios/Runner/Info.plist",
}


class NetworkPlatformTest(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = pathlib.Path(self.temp.name)
        for name, text in {
            "activity": ACTIVITY, "manifest": MANIFEST, "keep": KEEP,
            "jni": JNI, "lib": '#[cfg(target_os = "android")]\nmod android_jni;',
        }.items():
            self.write(name, text)
        self.write_plist({"NSLocalNetworkUsageDescription": "개발용 연결 시험에서 같은 네트워크의 기기에 접속합니다."})

    def write(self, name, text):
        path = self.root / PATHS[name]
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(text, encoding="utf-8")

    def write_plist(self, value):
        path = self.root / PATHS["plist"]
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(plistlib.dumps(value))

    def reject(self, name, text, diagnostic):
        self.write(name, text)
        self.assertTrue(any(diagnostic in error for error in check(self.root)), check(self.root))

    def test_valid_platform_contract(self):
        self.assertEqual(check(self.root), [])

    def test_missing_main_internet_even_with_comment(self):
        self.reject("manifest", MANIFEST.replace('<uses-permission android:name="android.permission.INTERNET"/>', '<!-- android.permission.INTERNET -->'), "INTERNET")

    def test_missing_network_state_permission_for_system_dns(self):
        self.reject("manifest", MANIFEST.replace('<uses-permission android:name="android.permission.ACCESS_NETWORK_STATE"/>', ''), "ACCESS_NETWORK_STATE")

    def test_permission_nested_in_application_is_not_main_permission(self):
        self.reject("manifest", MANIFEST.replace('<application/>', '<application><uses-permission android:name="android.permission.INTERNET"/></application>').replace('<uses-permission android:name="android.permission.INTERNET"/>', '', 1), "INTERNET")

    def test_load_after_flutter_initialization(self):
        self.reject("activity", ACTIVITY.replace('        System.loadLibrary("chat_ffi")', '        super.onCreate(savedInstanceState)\n        System.loadLibrary("chat_ffi")'), "before super")

    def test_activity_context_is_not_application_context(self):
        self.reject("activity", ACTIVITY.replace("initializeNetworkContext(applicationContext)", "initializeNetworkContext(this)"), "applicationContext")

    def test_unguarded_debug_channel(self):
        self.reject("activity", ACTIVITY.replace("if ((applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) != 0)", "if (true)", 1), "Debug channel")

    def test_channel_after_debug_block_is_not_guarded(self):
        self.reject("activity", ACTIVITY.replace('            MethodChannel', '        }\n        if (true) {\n            MethodChannel'), "Debug channel")

    def test_unbraced_debug_channel_guard_cannot_claim_later_run_block(self):
        guard = "if ((applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) != 0) {"
        self.reject("activity", ACTIVITY.replace(guard, guard[:-1] + "Unit\n        run {", 1), "Debug channel")

    def test_unguarded_debug_entrypoint_args(self):
        self.reject("activity", ACTIVITY.replace("if ((applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) != 0)", "if (true)"), "Debug entrypoint")

    def test_unbraced_debug_intent_guard_cannot_claim_later_run_block(self):
        guard = "if ((applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE) != 0) {"
        self.reject("activity", ACTIVITY.replace(guard, guard[:-1] + "Unit\n        run {"), "Debug entrypoint")

    def test_wrong_debug_control_extra(self):
        self.reject("activity", ACTIVITY.replace("chat_control_url", "other_control_url"), "intent contract")

    def test_wrong_debug_name_argument(self):
        self.reject("activity", ACTIVITY.replace('"--name"', '"--other-name"'), "intent contract")

    def test_reserved_is_not_context(self):
        self.reject("jni", JNI.replace("    JVM_PTR.set", "    install_android_jni_context(vm, _reserved);\n    JVM_PTR.set"), "JNI_OnLoad")

    def test_static_jni_receiver_is_rejected(self):
        self.reject("jni", JNI.replace("_this: JObject", "_this: JClass"), "instance")

    def test_local_reference_is_not_retained_context(self):
        self.reject("jni", JNI.replace("env.new_global_ref(context)", "context"), "GlobalRef")

    def test_duplicate_ndk_initialization(self):
        self.reject("jni", JNI.replace("    JNI_TRUE\n}", "    ndk_context::initialize_android_context(ptr, context);\n    JNI_TRUE\n}"), "ndk")

    def test_missing_mutex(self):
        self.reject("jni", JNI.replace("INIT.lock()", "noop()"), "serialized")

    def test_global_reference_created_after_dns_install(self):
        line = "    let global = env.new_global_ref(context);\n"
        self.reject("jni", JNI.replace(line, "").replace("    CONTEXT.set", line + "    CONTEXT.set"), "initialization order")

    def test_panicking_extern(self):
        self.reject("jni", JNI.replace("let _init = INIT.lock();", "let _init = INIT.lock().unwrap();"), "panic")

    def test_missing_native_r8_rule(self):
        self.reject("keep", "# " + KEEP, "R8")

    def test_missing_ios_usage(self):
        self.write_plist({})
        self.assertTrue(any("usage" in e for e in check(self.root)))

    def test_ios_bonjour_is_not_authorized(self):
        self.write_plist({"NSLocalNetworkUsageDescription": "설명", "NSBonjourServices": []})
        self.assertTrue(any("Bonjour" in e for e in check(self.root)))

    def test_android_module_is_target_gated(self):
        self.reject("lib", "mod android_jni;", "Android-only")


if __name__ == "__main__":
    unittest.main()
