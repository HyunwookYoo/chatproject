#!/usr/bin/env python3
"""Check platform networking prerequisites; complements builds and device tests."""
import pathlib
import plistlib
import re
import sys
import xml.etree.ElementTree as ET

ROOT = pathlib.Path(__file__).resolve().parents[2]


def source(path):
    text = path.read_text(encoding="utf-8")
    return re.sub(r"/\*.*?\*/|//[^\n]*", "", text, flags=re.S)


def block(text, pattern):
    match = re.search(pattern, text)
    if not match:
        return ""
    start = text.find("{", match.end())
    depth = 1
    for end in range(start + 1, len(text)):
        depth += (text[end] == "{") - (text[end] == "}")
        if depth == 0:
            return text[start + 1:end]
    return ""


def debug_blocks(body):
    return [block(body[m.start():], r"if\s*\(\(applicationInfo\.flags\s+and\s+ApplicationInfo\.FLAG_DEBUGGABLE\)\s*!=\s*0\)(?=\s*\{)")
            for m in re.finditer(r"if\s*\(\(applicationInfo\.flags", body)]


def check(root=ROOT):
    errors = []
    android = root / "app/android/app"
    manifest = ET.parse(android / "src/main/AndroidManifest.xml").getroot()
    name = "{http://schemas.android.com/apk/res/android}name"
    permissions = {p.get(name) for p in manifest.findall("uses-permission")}
    for permission in ("INTERNET", "ACCESS_NETWORK_STATE"):
        if "android.permission." + permission not in permissions:
            errors.append(f"Main manifest must declare {permission}")

    activity = source(android / "src/main/kotlin/dev/chatproject/chat_app/MainActivity.kt")
    create = block(activity, r"override fun onCreate\([^)]*\)")
    load = create.find('System.loadLibrary("chat_ffi")')
    initialize = create.find("check(initializeNetworkContext(applicationContext))")
    flutter = create.find("super.onCreate(")
    if not (0 <= load < initialize < flutter):
        errors.append("Load and checked applicationContext initialization must occur before super.onCreate")
    if not re.search(r"private external fun initializeNetworkContext\(context: Context\): Boolean", activity):
        errors.append("JNI initialization must be an instance Context method")
    configure = block(activity, r"override fun configureFlutterEngine\([^)]*\)")
    channels = configure.count('"chat_app/native_core"')
    if channels != 1 or sum(b.count('"chat_app/native_core"') for b in debug_blocks(configure)) != channels:
        errors.append("Debug channel must be inside FLAG_DEBUGGABLE guard")
    args = block(activity, r"override fun getDartEntrypointArgs\([^)]*\)")
    custom = args.count("intent.")
    if not custom or sum(b.count("intent.") for b in debug_blocks(args)) != custom or "super.getDartEntrypointArgs()" not in args:
        errors.append("Debug entrypoint intent args must be guarded and preserve superclass defaults")
    guarded_args = "\n".join(debug_blocks(args))
    if not all(s in guarded_args for s in ('getStringExtra("chat_control_url")', 'getStringExtra("chat_name")', '"--control-url"', '"--name"')):
        errors.append("Debug intent contract must map chat_control_url and chat_name to Dart flags")

    keep = re.sub(r"#[^\n]*", "", (android / "proguard-rules.pro").read_text(encoding="utf-8"))
    if not re.search(r"-keepclasseswithmembernames\s+class\s+dev\.chatproject\.chat_app\.MainActivity\s*\{\s*native\s+<methods>;\s*\}", keep):
        errors.append("R8 must keep MainActivity native method names")

    rust = root / "app/rust/src"
    lib = source(rust / "lib.rs")
    if not re.search(r'#\[cfg\(target_os\s*=\s*"android"\)\]\s*mod android_jni;', lib):
        errors.append("JNI module must be Android-only")
    jni_path = rust / "android_jni.rs"
    jni = source(jni_path) if jni_path.exists() else ""
    onload = block(jni, r"fn JNI_OnLoad\([^)]*\)[^{]*")
    if not onload or "reserved" in onload or "install_android_jni_context" in onload or "JNI_VERSION_1_6" not in onload:
        errors.append("JNI_OnLoad must store only VM/version and leave reserved unused")
    export = r"fn Java_dev_chatproject_chat_1app_MainActivity_initializeNetworkContext\(([^)]*)\)"
    signature = re.search(export, jni)
    if not signature or not re.search(r"_this:\s*JObject", signature[1]) or "JClass" in signature[1]:
        errors.append("Exported JNI initialization requires instance JObject receiver")
    init = block(jni, export + r"[^{]*")
    if "INIT.lock()" not in init or "CONTEXT.get()" not in init or "Mutex" not in jni or "OnceLock<ContextState>" not in jni:
        errors.append("Context initialization must be serialized and idempotent")
    if "GlobalRef" not in jni or "new_global_ref(context)" not in init or "context.is_null()" not in init:
        errors.append("Context must be non-null and retained as GlobalRef")
    order = [init.find(s) for s in ("JVM_PTR.get()", "JavaVM::from_raw", "new_global_ref(context)", "iroh::dns::install_android_jni_context", "CONTEXT.set(")]
    if any(i < 0 for i in order) or order != sorted(order):
        errors.append("JNI initialization order must retain VM/context before installing DNS context")
    if "initialize_android_context" in jni or jni.count("iroh::dns::install_android_jni_context") != 1:
        errors.append("ndk context must be initialized only once through iroh")
    if re.search(r"\.(?:unwrap|expect)\(|\b(?:panic|assert|assert_eq|assert_ne)!", onload + init):
        errors.append("JNI extern functions must not panic on error")

    with (root / "app/ios/Runner/Info.plist").open("rb") as handle:
        plist = plistlib.load(handle)
    if not str(plist.get("NSLocalNetworkUsageDescription", "")).strip():
        errors.append("iOS LAN usage explanation is required")
    if "NSBonjourServices" in plist:
        errors.append("Bonjour services are not authorized")
    return errors


def main():
    try:
        errors = check()
    except (OSError, ET.ParseError, plistlib.InvalidFileException) as error:
        errors = [f"Platform file cannot be read: {error}"]
    for error in errors:
        print(error)
    return bool(errors)


if __name__ == "__main__":
    sys.exit(main())
