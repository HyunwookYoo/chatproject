import os
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


if __name__ == "__main__":
    unittest.main()
