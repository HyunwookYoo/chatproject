import os
import json
import pathlib
import shutil
import subprocess
import tempfile
import unittest


PACKAGE = pathlib.Path(__file__).resolve().parents[2] / "app/rust_builder/cargokit/build_tool"
DART = shutil.which("dart")

PROBE = r'''
import 'package:build_tool/src/builder.dart';
import 'package:build_tool/src/cargo.dart';
import 'package:build_tool/src/options.dart';
import 'package:build_tool/src/rustup.dart';
import 'package:build_tool/src/target.dart';

class ProbeRustup implements Rustup {
  String? selected;
  @override
  List<String>? installedTargets(String toolchain) {
    selected = toolchain;
    return ['x86_64-unknown-linux-gnu'];
  }
  @override
  dynamic noSuchMethod(Invocation invocation) => throw StateError('unexpected rustup command');
}

void main(List<String> args) {
  final options = args.isEmpty ? CargokitCrateOptions() : CargokitCrateOptions(cargo: {
    BuildConfiguration.debug: CargoBuildOptions(toolchain: Toolchain.beta, flags: []),
  });
  final builder = RustBuilder(
    target: Target(rust: 'x86_64-unknown-linux-gnu'),
    environment: BuildEnvironment(configuration: BuildConfiguration.debug,
      crateOptions: options, targetTempDir: 'unused', manifestDir: 'unused',
      crateInfo: CrateInfo(packageName: 'fixture'), isAndroid: false),
  );
  final rustup = ProbeRustup();
  try {
    builder.prepare(rustup);
    print('selected=${rustup.selected}');
  } on StateError catch (error) {
    print('StateError: ${error.message}; selected=${rustup.selected}');
  }
}
'''

BUILD_PROBE = r'''
import 'dart:convert';
import 'package:build_tool/src/builder.dart';
import 'package:build_tool/src/cargo.dart';
import 'package:build_tool/src/options.dart';
import 'package:build_tool/src/target.dart';
import 'package:build_tool/src/util.dart';
import 'package:build_tool/src/rustup.dart';
Future<void> main() async {
  for (final configuration in BuildConfiguration.values) {
    testRunCommandOverride = (args) {
      print(jsonEncode({'executable':args.executable,'args':args.arguments}));
      return TestRunCommandResult();
    };
    final builder=RustBuilder(target:Target(rust:'x86_64-pc-windows-msvc'),
      environment:BuildEnvironment(configuration:configuration,
        crateOptions:CargokitCrateOptions(cargo:{BuildConfiguration.debug:
          CargoBuildOptions(toolchain:Toolchain.stable,flags:['--features','test-hooks'])}),
        targetTempDir:'owned-target',manifestDir:'owned-source',
        crateInfo:CrateInfo(packageName:'fixture'),isAndroid:false));
    await builder.build();
  }
}
'''


@unittest.skipUnless(DART, "requires Dart SDK")
class CargokitToolchainTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        result = subprocess.run([DART, "pub", "get", "--enforce-lockfile"], cwd=PACKAGE,
                                capture_output=True, text=True)
        if result.returncode:
            raise RuntimeError(result.stdout + result.stderr)

    def run_probe(self, override, configured=False):
        with tempfile.TemporaryDirectory() as folder:
            probe = pathlib.Path(folder) / "probe.dart"
            probe.write_text(PROBE, encoding="utf-8")
            env = dict(os.environ)
            env.pop("CARGOKIT_TOOLCHAIN", None)
            if override is not None:
                env["CARGOKIT_TOOLCHAIN"] = override
            result = subprocess.run([DART, "--packages=" + str(PACKAGE / ".dart_tool/package_config.json"),
                                     str(probe), *(["configured"] if configured else [])],
                                    env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            return result.stdout

    def test_empty_override_fails_before_rustup_commands(self):
        output = self.run_probe("")
        self.assertIn("StateError:", output)
        self.assertIn("CARGOKIT_TOOLCHAIN", output)
        self.assertIn("must not be empty", output)
        self.assertIn("selected=null", output)

    def test_pinned_override_and_upstream_fallbacks(self):
        self.assertEqual(self.run_probe("1.92.0", configured=True), "selected=1.92.0\n")
        self.assertEqual(self.run_probe(None), "selected=stable\n")
        self.assertEqual(self.run_probe(None, configured=True), "selected=beta\n")

    def test_real_builder_pins_lock_target_and_debug_features(self):
        with tempfile.TemporaryDirectory() as folder:
            probe=pathlib.Path(folder)/"build_probe.dart"
            probe.write_text(BUILD_PROBE,encoding="utf-8")
            result=subprocess.run([DART,"--packages="+str(PACKAGE/".dart_tool/package_config.json"),str(probe)],
                env={**os.environ,"CARGOKIT_TOOLCHAIN":"1.92.0"},capture_output=True,text=True)
            self.assertEqual(result.returncode,0,result.stderr)
            commands=[json.loads(line) for line in result.stdout.splitlines()]
            self.assertEqual(len(commands),3)
            for index,command in enumerate(commands):
                self.assertEqual(command["executable"],"rustup")
                args=command["args"]
                self.assertEqual(args[:5],["run","1.92.0","cargo","build","--locked"])
                self.assertEqual(args[args.index("--target")+1],"x86_64-pc-windows-msvc")
                self.assertEqual(args[args.index("--target-dir")+1],"owned-target")
                self.assertEqual("--release" in args,index!=0)
                self.assertEqual("test-hooks" in args,index==0)

    @unittest.skipUnless(os.name == "nt", "requires Windows cmd wrapper")
    def test_android_tool_paths_survive_unix_make_shell(self):
        probe_source = r'''
import 'dart:convert';
import 'package:build_tool/src/android_environment.dart';
import 'package:build_tool/src/target.dart';
Future<void> main(List<String> args) async {
  final env = AndroidEnvironment(sdkPath: args[0], ndkVersion: '28.2.13676358',
    minSdkVersion: 24, targetTempDir: args[1],
    target: Target.forFlutterName('android-x64')!);
  print(jsonEncode(await env.buildEnvironment()));
}
'''
        with tempfile.TemporaryDirectory(prefix="cargokit-android-") as folder:
            root = pathlib.Path(folder)
            bin_dir = root / "ndk/28.2.13676358/toolchains/llvm/prebuilt/windows-x86_64/bin"
            bin_dir.mkdir(parents=True)
            tools = {"AR": "llvm-ar.exe", "CC": "clang.exe",
                     "CXX": "clang++.exe", "RANLIB": "llvm-ranlib.exe"}
            for name in tools.values():
                (bin_dir / name).touch()
            probe = root / "android_probe.dart"
            probe.write_text(probe_source, encoding="utf-8")
            result = subprocess.run([DART, "--packages=" + str(PACKAGE / ".dart_tool/package_config.json"),
                                     str(probe), str(root), str(root / "target")],
                                    capture_output=True, text=True, encoding="utf-8")
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            env = json.loads(result.stdout)
            for key, name in tools.items():
                value = env[key + "_x86_64-linux-android"]
                self.assertEqual(pathlib.Path(value).resolve(), (bin_dir / name).resolve())
                self.assertNotIn("\\", value)
            self.assertEqual(env["CFLAGS_x86_64-linux-android"], "--target=x86_64-linux-android24")
            self.assertEqual(env["CXXFLAGS_x86_64-linux-android"], env["CFLAGS_x86_64-linux-android"])
            self.assertEqual(env["_CARGOKIT_NDK_LINK_CLANG"], str(bin_dir / "clang.exe"))
            self.assertTrue(env["CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER"].endswith("run_build_tool.cmd"))

    @unittest.skipUnless(os.name == "nt", "requires Windows cmd wrapper")
    def test_actual_windows_wrapper_preserves_success_failure_and_snapshot_retry(self):
        for exits, expected, calls in [([0], 0, 1), ([7], 7, 1),
                                       ([253, 0], 0, 2), ([253, 9], 9, 2)]:
            with self.subTest(exits=exits):
                result, count = self.run_windows_wrapper(exits)
                self.assertEqual(result.returncode, expected, result.stdout + result.stderr)
                self.assertEqual(count, calls)

    @unittest.skipUnless(os.name == "nt", "requires Windows cmd wrapper")
    def test_actual_windows_wrapper_fails_before_using_kernel_after_pub_or_compile_error(self):
        for failure in ("pub", "compile"):
            with self.subTest(failure=failure):
                result, count = self.run_windows_wrapper([0], failure)
                self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(count, 0)
                if failure == "pub":
                    self.assertNotIn("Compiling bin", result.stdout)

    def run_windows_wrapper(self, exits, failure=None):
        flutter = pathlib.Path(os.environ.get("FLUTTER_ROOT", pathlib.Path(DART).resolve().parents[1]))
        if not (flutter / "bin/cache/dart-sdk/bin/dart.exe").exists():
            self.skipTest("requires Flutter's real Windows Dart SDK")
        with tempfile.TemporaryDirectory(prefix="cargokit-wrapper-") as folder:
            root = pathlib.Path(folder)
            wrapper = root / "run_build_tool.cmd"
            shutil.copy2(PACKAGE.parent / "run_build_tool.cmd", wrapper)
            package = root / "build_tool"
            (package / "lib").mkdir(parents=True)
            pubspec = "name: build_tool\nversion: 1.0.0\nenvironment:\n  sdk: '>=3.0.0 <4.0.0'\n"
            if failure == "pub":
                pubspec = "not valid: ["
            (package / "pubspec.yaml").write_text(pubspec, encoding="utf-8")
            source = """import 'dart:io';
void runMain(List<String> args) {
  final file = File(Platform.environment['CARGOKIT_PROBE_COUNT']!);
  final count = file.existsSync() ? int.parse(file.readAsStringSync()) : 0;
  file.writeAsStringSync('${count + 1}');
  final exits = Platform.environment['CARGOKIT_PROBE_EXITS']!.split(',').map(int.parse).toList();
  exit(exits[count < exits.length ? count : exits.length - 1]);
}
"""
            if failure == "compile":
                source = "void runMain( deliberately invalid"
            (package / "lib/build_tool.dart").write_text(source, encoding="utf-8")
            temporary = root / "temporary"
            (temporary / ".dart_tool").mkdir(parents=True)
            count_file = root / "calls.txt"
            env = {**os.environ, "FLUTTER_ROOT": str(flutter),
                   "CARGOKIT_TOOL_TEMP_DIR": str(temporary),
                   "CARGOKIT_PROBE_COUNT": str(count_file),
                   "CARGOKIT_PROBE_EXITS": ",".join(map(str, exits))}
            result = subprocess.run(["cmd", "/d", "/c", str(wrapper), "build-gradle"],
                                    env=env, capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=60)
            count = int(count_file.read_text()) if count_file.exists() else 0
            return result, count

    @unittest.skipUnless(shutil.which("cmake"), "requires CMake")
    def test_cmake_temp_path_is_short_windows_only_and_per_crate(self):
        with tempfile.TemporaryDirectory() as folder:
            root=PACKAGE.parents[3]
            output=pathlib.Path(folder)/"selected.txt"
            script=pathlib.Path(folder)/"probe.cmake"
            # Execute the actual path selection; suppress only build-generation
            # primitives which cannot run in CMake script mode.
            for windows in (True,False):
                script.write_text(f'''
function(execute_process)
endfunction()
function(add_custom_command)
  file(APPEND "{output.as_posix()}" "${{CARGOKIT_TEMP_DIR}}\\n")
endfunction()
function(add_custom_target)
endfunction()
function(set_source_files_properties)
endfunction()
set(WIN32 {"TRUE" if windows else "FALSE"})
set(CMAKE_SOURCE_DIR "{root.as_posix()}/app/windows")
set(CMAKE_CURRENT_BINARY_DIR "{root.as_posix()}/app/build/windows/x64/plugins/fixture")
include("{(PACKAGE.parent/'cmake/cargokit.cmake').as_posix()}")
apply_cargokit(fixture ../rust first exported)
apply_cargokit(fixture ../rust second exported)
''',encoding="utf-8")
                output.write_text("",encoding="utf-8")
                result=subprocess.run([shutil.which("cmake"),"-P",str(script)],capture_output=True,text=True)
                self.assertEqual(result.returncode,0,result.stderr)
                paths=output.read_text(encoding="utf-8").splitlines()
                expected=[root/"build"/name for name in ("first","second")] if windows else [root/"app/build/windows/x64/plugins/fixture/cargokit_build"]*2
                self.assertEqual([pathlib.Path(value) for value in paths],expected)
                for selected in map(pathlib.Path, paths):
                    self.assertTrue(selected.is_absolute())
                    self.assertTrue(selected.is_relative_to(root))
                if windows:
                    # Longest generated object from the retained OpenSSL failure.
                    suffix=("x86_64-pc-windows-msvc/{profile}/build/"
                        "openssl-sys-6bba6ab70b5d0387/out/openssl-build/build/src/"
                        "providers/implementations/ciphers/"
                        "libdefault-lib-cipher_aes_cbc_hmac_sha512_etm_hw.obj")
                    for profile in ("debug","release"):
                        generated=root/"build/chat_ffi"/suffix.format(profile=profile)
                        previous=root/"app/build/cargokit/chat_ffi"/suffix.format(profile=profile)
                        self.assertEqual(len(str(previous))-len(str(generated)),13)
                        if os.name == "nt":
                            self.assertLess(len(str(generated)),260,
                                "This checkout exceeds the measured MSVC build path budget")


if __name__ == "__main__":
    unittest.main()
