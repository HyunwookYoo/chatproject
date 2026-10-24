import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
BASH = "C:/Program Files/Git/bin/bash.exe" if os.name == "nt" else shutil.which("bash")

FAKE_TOOL = r'''
import os, sys
tool, *args = sys.argv[1:]
if tool == "plist":
    print("dev.chatproject.chatapp.NotificationService" if "CFBundleIdentifier" in args[1] else "1")
elif tool == "nm":
    if args[0] == "-gU":
        print("00000000 T _frb_get_rust_content_hash")
    else:
        if os.environ.get("FAIL_TOOL") == "nm": sys.exit(7)
        print(os.environ["IMPORTS"])
elif tool == "otool":
    print(args[-1] + ":")
    print(" /usr/lib/libSystem.B.dylib (compatibility version 1.0.0)")
    if args[-1].endswith("chat_ffi"):
        if os.environ.get("FAIL_TOOL") == "otool": sys.exit(7)
        if os.environ.get("SQLITE_LINK"): print(" /usr/lib/libsqlite3.dylib (compatibility version 1.0.0)")
        if os.environ["NETWORK"]:
            print(" /" + os.environ["NETWORK"] + " (compatibility version 1.0.0)")
'''


@unittest.skipUnless(BASH and pathlib.Path(BASH).is_file(), "requires native bash")
class IosSymbolsTest(unittest.TestCase):
    def run_check(self, imports="_getaddrinfo\n_nw_path_get_status\n_CCHmac", network=None, fail_tool="", sqlite_link=""):
        with tempfile.TemporaryDirectory(prefix="ios symbols fixture ") as folder:
            root = pathlib.Path(folder)
            app = root / "Runner.app"
            appex = app / "PlugIns/NotificationService.appex"
            appex.mkdir(parents=True)
            (appex / "NotificationService").touch()
            (appex / "nse_state.bin").touch()
            ffi = app / "Frameworks/chat_ffi.framework"
            ffi.mkdir(parents=True)
            (ffi / "chat_ffi").touch()
            fake_bin = root / "bin"
            fake_bin.mkdir()
            fake = root / "fake_tool.py"
            fake.write_text(FAKE_TOOL, encoding="utf-8")
            for tool in ("plist", "nm", "otool"):
                wrapper = fake_bin / tool
                wrapper.write_text('#!/usr/bin/env bash\nexec "' + pathlib.Path(sys.executable).as_posix() +
                                   '" "' + fake.as_posix() + '" ' + tool + ' "$@"\n', encoding="utf-8")
                wrapper.chmod(0o755)
            env = dict(os.environ, IMPORTS=imports, FAIL_TOOL=fail_tool, SQLITE_LINK=sqlite_link,
                       NETWORK="System/Library/Frameworks/Network.framework/Network" if network is None else network.lstrip("/"),
                       PLISTBUDDY=(fake_bin / "plist").as_posix(), CHECK_RELEASE_HOOKS="0",
                       NSE_ID="dev.chatproject.chatapp.NotificationService",
                       FIXTURE_BIN=str(fake_bin), FIXTURE_APP=app.as_posix(),
                       FIXTURE_SCRIPT=(ROOT / "tools/ci/check_ios_app.sh").as_posix())
            prefix = '$(cygpath -u "$FIXTURE_BIN")' if os.name == "nt" else '$FIXTURE_BIN'
            command = 'export PATH="' + prefix + ':$PATH"; exec bash "$FIXTURE_SCRIPT" "$FIXTURE_APP"'
            return subprocess.run([BASH, "-c", command], env=env, capture_output=True, text=True)

    def test_clean_public_imports_pass(self):
        result = self.run_check(imports="_getaddrinfo\n_nw_path_get_status\n_SCNetworkReachabilityCreateWithName\n_CCHmac")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_missing_commoncrypto_provider_is_rejected(self):
        result = self.run_check(imports="_getaddrinfo\n_nw_path_get_status")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("CommonCrypto", result.stdout)

    def test_dynamic_ordinary_sqlite_is_rejected(self):
        result = self.run_check(sqlite_link="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ordinary SQLite", result.stdout)

    def test_each_private_configuration_family_is_rejected(self):
        for symbol in ("_SCDynamicStoreCreate", "_SCPreferencesCreate", "_SCNetworkInterfaceCopyAll",
                       "_SCNetworkServiceCopyAll", "_SCNetworkSetCopyCurrent", "_kSCPropNetDNSServerAddresses"):
            with self.subTest(symbol=symbol):
                result = self.run_check(imports="                 U " + symbol)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("macOS-only SystemConfiguration", result.stdout)

    def test_missing_or_private_network_framework_is_rejected(self):
        for path in ("", "/System/Library/PrivateFrameworks/Network.framework/Network"):
            with self.subTest(path=path):
                result = self.run_check(network=path)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("public Network.framework", result.stdout)

    def test_symbol_and_link_tool_failures_are_rejected(self):
        for tool in ("nm", "otool"):
            with self.subTest(tool=tool):
                result = self.run_check(fail_tool=tool)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("::error::", result.stdout)


if __name__ == "__main__":
    unittest.main()
