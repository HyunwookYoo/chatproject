import pathlib
import subprocess
import sys
import tempfile
import unittest
import zipfile


SCRIPT = pathlib.Path(__file__).with_name("check_release_hooks.py")
MARKER = b"chat-test-hooks:v1"
MACHO = b"\xcf\xfa\xed\xfe"


class ReleaseHooksTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = pathlib.Path(self.temp.name)

    def check(self, artifact, success):
        result = subprocess.run([sys.executable, str(SCRIPT), str(artifact)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0 if success else 1, result.stdout + result.stderr)

    def apk(self, entries):
        path = self.root / "app.apk"
        with zipfile.ZipFile(path, "w") as archive:
            for name, data in entries.items():
                archive.writestr(name, data)
        return path

    def test_clean_multiabi_apk(self):
        self.check(self.apk({f"lib/{abi}/libchat_ffi.so": b"ELF clean" for abi in ["x86_64", "arm64-v8a", "armeabi-v7a"]}), True)

    def test_marker_in_only_one_abi_fails(self):
        self.check(self.apk({"lib/x86_64/libchat_ffi.so": b"clean", "lib/arm64-v8a/other.so": MARKER}), False)

    def test_no_native_apk_fails(self):
        self.check(self.apk({"assets/readme": b"clean"}), False)

    def test_archive_traversal_fails_without_extracting(self):
        self.check(self.apk({"../escaped.so": MARKER, "lib/x86_64/libchat_ffi.so": b"clean"}), False)
        self.assertFalse((self.root.parent / "escaped.so").exists())

    def test_windows_scans_every_dll_and_executable(self):
        (self.root / "chat_app.exe").write_bytes(b"MZ clean")
        (self.root / "chat_ffi.dll").write_bytes(b"MZ clean")
        self.check(self.root, True)
        (self.root / "plugin.dll").write_bytes(MARKER)
        self.check(self.root, False)

    def test_windows_missing_native_library_fails(self):
        (self.root / "chat_app.exe").write_bytes(b"MZ clean")
        self.check(self.root, False)

    def test_app_scans_extensionless_macho_and_dylibs(self):
        app = self.root / "Runner.app"
        native = app / "Frameworks" / "chat_ffi.framework" / "chat_ffi"
        native.parent.mkdir(parents=True)
        native.write_bytes(MACHO + b"clean")
        (app / "Runner").write_bytes(MACHO + b"clean")
        self.check(app, True)
        plugin = app / "Frameworks" / "plugin.dylib"
        plugin.write_bytes(MACHO + MARKER)
        self.check(app, False)
        plugin.unlink()
        (app / "Runner").write_bytes(MACHO + MARKER)
        self.check(app, False)

    def test_empty_app_fails(self):
        app = self.root / "Runner.app"
        app.mkdir()
        self.check(app, False)

    def test_app_missing_core_and_foreign_native_extensions_fail(self):
        app = self.root / 'Runner.app'
        app.mkdir()
        (app / 'Runner').write_bytes(MACHO + b'clean')
        self.check(app, False)
        (app / 'chat_ffi').write_bytes(MACHO + b'clean')
        (app / 'foreign.dll').write_bytes(MARKER)
        self.check(app, False)

    def test_missing_unsupported_and_corrupt_fail(self):
        self.check(self.root / "missing.apk", False)
        file = self.root / "readme.txt"
        file.write_bytes(b"clean")
        self.check(file, False)
        broken = self.root / "broken.apk"
        broken.write_bytes(b"invalid zip")
        self.check(broken, False)


if __name__ == "__main__":
    unittest.main()
