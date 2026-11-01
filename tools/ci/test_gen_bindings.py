import json
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
import json, os, pathlib, sys
tool, *args = sys.argv[1:]
root = pathlib.Path(os.environ["FIXTURE_ROOT"])
with (root / "calls.jsonl").open("a", encoding="utf-8") as log:
    log.write(json.dumps([tool, args, str(pathlib.Path.cwd())]) + "\n")
target = root / "target with spaces"
if tool == "uname":
    print(os.environ["FAKE_HOST"])
elif tool == "flutter_rust_bridge_codegen" and args == ["--version"]:
    print("flutter_rust_bridge_codegen " + os.environ["FAKE_VERSION"])
elif tool == "cargo" and args[:1] == ["metadata"]:
    print(json.dumps({"target_directory": str(target)}))
elif tool == "cargo" and args[:1] == ["build"]:
    (target / "debug").mkdir(parents=True)
    for name in ("chat_ffi.dll", "chat_nse.lib", "libchat_ffi.dylib", "libchat_ffi.so", "libchat_nse.a"):
        (target / "debug" / name).touch()
elif tool == "cargo" and args[:1] == ["run"]:
    if not pathlib.Path(args[args.index("--library") + 1]).is_file():
        sys.exit("library path is missing or was split")
'''


@unittest.skipUnless(BASH and pathlib.Path(BASH).is_file(), "requires native bash")
class GeneratorTest(unittest.TestCase):
    def run_generator(self, version="2.11.1", host="Linux"):
        folder = tempfile.TemporaryDirectory(prefix="bindings fixture ")
        self.addCleanup(folder.cleanup)
        root = pathlib.Path(folder.name)
        (root / "tools").mkdir()
        (root / "app").mkdir()
        shutil.copyfile(ROOT / "tools/gen_bindings.sh", root / "tools/gen_bindings.sh")
        fake_bin = root / "bin"
        fake_bin.mkdir()
        fake = root / "fake_tool.py"
        fake.write_text(FAKE_TOOL, encoding="utf-8")
        for tool in ("uname", "flutter", "flutter_rust_bridge_codegen", "cargo", "dart"):
            wrapper = fake_bin / tool
            wrapper.write_text('#!/usr/bin/env bash\nexec "' + pathlib.Path(sys.executable).as_posix() +
                               '" "' + fake.as_posix() + '" ' + tool + ' "$@"\n', encoding="utf-8")
            wrapper.chmod(0o755)
        env = dict(os.environ, FIXTURE_ROOT=str(root), FAKE_VERSION=version, FAKE_HOST=host,
                   FIXTURE_BIN=str(fake_bin), FIXTURE_SCRIPT=str(root / "tools/gen_bindings.sh"))
        # Git Bash prepends its own utilities when entered from Windows. Put the
        # fixture first after that startup so uname is controlled too.
        prefix = '$(cygpath -u "$FIXTURE_BIN")' if os.name == "nt" else '$FIXTURE_BIN'
        command = 'export PATH="' + prefix + ':$PATH"; exec bash "$FIXTURE_SCRIPT"'
        result = subprocess.run([BASH, "-c", command], env=env,
                                capture_output=True, text=True)
        calls = [json.loads(line) for line in (root / "calls.jsonl").read_text(encoding="utf-8").splitlines()]
        return result, calls, root

    def test_codegen_rejects_similar_version(self):
        for version in ("2.11.10", "2.11.1-dev", "2.12.0"):
            with self.subTest(version=version):
                result, calls, _ = self.run_generator(version)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("2.11.1 is required", result.stderr)
                self.assertFalse(any(tool in ("flutter", "dart") or args == ["generate"]
                                     for tool, args, _ in calls))
        self.assertEqual(self.run_generator("2.11.1")[0].returncode, 0)

    def test_all_hosts_use_metadata_library_paths_and_prepare_flutter_once(self):
        for host, ffi, nse in (("Darwin", "libchat_ffi.dylib", "libchat_nse.a"),
                               ("MINGW64_NT-10.0", "chat_ffi.dll", "chat_nse.lib"),
                               ("Linux", "libchat_ffi.so", "libchat_nse.a")):
            with self.subTest(host=host):
                result, calls, root = self.run_generator(host=host)
                self.assertEqual(result.returncode, 0, result.stderr)
                commands = [(tool, args) for tool, args, _ in calls]
                expected = [("uname", ["-s"]),
                            ("flutter_rust_bridge_codegen", ["--version"]),
                            ("cargo", ["metadata", "--no-deps", "--format-version", "1"]),
                            ("flutter", ["pub", "get"]),
                            ("flutter_rust_bridge_codegen", ["generate"]),
                            ("dart", ["run", "build_runner", "build", "--define", "freezed:freezed=format=true"]),
                            ("cargo", ["build", "-p", "chat_ffi", "-p", "chat_nse"])]
                self.assertCountEqual(commands[:7], expected)
                self.assertLess(commands.index(("flutter", ["pub", "get"])),
                                commands.index(("flutter_rust_bridge_codegen", ["generate"])))
                for tool, args, cwd in calls:
                    if tool in ("flutter", "dart") or args == ["generate"]:
                        self.assertEqual(pathlib.Path(cwd), root / "app")
                runs = [args for tool, args, _ in calls if tool == "cargo" and args[:1] == ["run"]]
                self.assertEqual(len(runs), 2)
                for args, library, language, output in (
                    (runs[0], ffi, "kotlin", "app/android/app/src/main/kotlin"),
                    (runs[1], nse, "swift", "app/ios/NotificationService/Generated"),
                ):
                    self.assertEqual(args[:7], ["run", "-q", "-p", "uniffi-bindgen", "--", "generate", "--library"])
                    self.assertEqual(pathlib.Path(args[7]), root / "target with spaces/debug" / library)
                    self.assertEqual(args[8:], ["--language", language, "--out-dir", output, "--no-format"])


if __name__ == "__main__":
    unittest.main()
