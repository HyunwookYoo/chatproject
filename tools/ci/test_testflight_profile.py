import base64
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[2]
BASH = "C:/Program Files/Git/bin/bash.exe" if os.name == "nt" else shutil.which("bash")
SCRIPT = ROOT / "tools/ci/testflight_profile.sh"
TEAM = "ABCDE12345"
APP_ID = "dev.chatproject.chatapp"
NSE_ID = "dev.chatproject.chatapp.NotificationService"
APP_GROUP = "group.dev.chatproject.chatapp"
HEADER = b"FAKE-CMS\n"

# `security cms -D -i <file>` and `PlistBuddy -c 'Print :A:B:0' <file>` over a JSON "profile".
FAKE_TOOL = r'''
import json, sys
tool, *args = sys.argv[1:]
if tool == "security":
    data = open(args[args.index("-i") + 1], "rb").read()
    if args[:2] != ["cms", "-D"] or not data.startswith(b"FAKE-CMS\n"):
        print("security: SecCMSDecoderFinalizeMessage failed", file=sys.stderr)
        sys.exit(1)
    sys.stdout.write(data[len(b"FAKE-CMS\n"):].decode())
else:
    key = args[args.index("-c") + 1].split(" ", 1)[1]
    value = json.load(open(args[-1]))
    for part in key.strip(":").split(":"):
        try:
            value = value[int(part)] if isinstance(value, list) else value[part]
        except (KeyError, IndexError, ValueError, TypeError):
            print('Print: Entry, "' + key + '", Does Not Exist')
            sys.exit(1)
    if isinstance(value, list):
        print("Array {\n" + "".join("    " + str(v) + "\n" for v in value) + "}")
    else:
        print(value)
'''


def profile(bundle, name, **overrides):
    doc = {
        "UUID": "uuid-" + bundle.rsplit(".", 1)[-1],
        "Name": name,
        "TeamIdentifier": [TEAM],
        "Entitlements": {
            "application-identifier": TEAM + "." + bundle,
            "com.apple.security.application-groups": [APP_GROUP],
            "aps-environment": "production",
        },
    }
    for key, value in overrides.items():
        target = doc["Entitlements"] if key in doc["Entitlements"] else doc
        if value is None:
            target.pop(key, None)
        else:
            target[key] = value
    return base64.b64encode(HEADER + json.dumps(doc).encode()).decode()


@unittest.skipUnless(BASH and pathlib.Path(BASH).is_file(), "requires native bash")
class TestflightProfileTest(unittest.TestCase):
    def run_script(self, app=None, nse=None, team_line="TEAM = '" + TEAM + "'"):
        app = profile(APP_ID, "ChatProject App Store") if app is None else app
        nse = profile(NSE_ID, "ChatProject NSE App Store") if nse is None else nse
        folder = tempfile.TemporaryDirectory(prefix="testflight profile ")
        self.addCleanup(folder.cleanup)
        root = pathlib.Path(folder.name)
        scripts = root / "work/app/ios/scripts"
        scripts.mkdir(parents=True)
        (scripts / "add_notification_service.rb").write_text("# fixture\n" + team_line + "\n", encoding="utf-8")
        for name in ("home", "temp", "bin"):
            (root / name).mkdir()
        fake = root / "fake_tool.py"
        fake.write_text(FAKE_TOOL, encoding="utf-8")
        for tool in ("security", "PlistBuddy"):
            wrapper = root / "bin" / tool
            wrapper.write_text('#!/usr/bin/env bash\nexec "' + pathlib.Path(sys.executable).as_posix() +
                               '" "' + fake.as_posix() + '" ' + tool + ' "$@"\n', encoding="utf-8")
            wrapper.chmod(0o755)
        (root / "github_env").touch()
        env = dict(os.environ, APP_PROFILE_B64=app, NSE_PROFILE_B64=nse, APP_ID=APP_ID, NSE_ID=NSE_ID,
                   APP_GROUP=APP_GROUP, APP_PROFILE_NAME="ChatProject App Store",
                   NSE_PROFILE_NAME="ChatProject NSE App Store", HOME=(root / "home").as_posix(),
                   RUNNER_TEMP=(root / "temp").as_posix(), GITHUB_ENV=(root / "github_env").as_posix(),
                   PLISTBUDDY=(root / "bin/PlistBuddy").as_posix(), FIXTURE_BIN=str(root / "bin"),
                   FIXTURE_SCRIPT=SCRIPT.as_posix())
        prefix = '$(cygpath -u "$FIXTURE_BIN")' if os.name == "nt" else '$FIXTURE_BIN'
        command = 'export PATH="' + prefix + ':$PATH"; exec bash "$FIXTURE_SCRIPT"'
        result = subprocess.run([BASH, "-c", command], env=env, cwd=root / "work", capture_output=True, text=True)
        return result, root

    def assert_fails(self, message, **kwargs):
        result, _ = self.run_script(**kwargs)
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertIn("::error::", result.stdout)
        self.assertIn(message, result.stdout)

    def test_matching_profiles_are_installed_and_exported(self):
        result, root = self.run_script()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        for folder in ("Library/MobileDevice/Provisioning Profiles", "Library/Developer/Xcode/UserData/Provisioning Profiles"):
            for uuid in ("uuid-chatapp", "uuid-NotificationService"):
                self.assertTrue((root / "home" / folder / (uuid + ".mobileprovision")).is_file(), folder + uuid)
        self.assertEqual((root / "github_env").read_text(encoding="utf-8").split(),
                         ["APP_PROFILE_UUID=uuid-chatapp", "TEAM_ID=" + TEAM,
                          "NSE_PROFILE_UUID=uuid-NotificationService", "TEAM_ID=" + TEAM])

    def test_team_mismatch_is_rejected(self):
        self.assert_fails("belongs to team OTHER00000", nse=profile(NSE_ID, "ChatProject NSE App Store",
                                                                    TeamIdentifier=["OTHER00000"]))

    def test_bundle_id_mismatch_is_rejected(self):
        self.assert_fails("nse profile is for " + TEAM + "." + APP_ID,
                          nse=profile(NSE_ID, "ChatProject NSE App Store", **{"application-identifier": TEAM + "." + APP_ID}))

    def test_development_or_ad_hoc_profile_is_rejected(self):
        self.assert_fails("app is not an App Store profile",
                          app=profile(APP_ID, "ChatProject App Store", ProvisionedDevices=["00008110-0000"]))

    def test_missing_app_group_is_rejected(self):
        self.assert_fails("nse profile lacks " + APP_GROUP,
                          nse=profile(NSE_ID, "ChatProject NSE App Store", **{"com.apple.security.application-groups": None}))

    def test_wrong_profile_name_is_rejected(self):
        self.assert_fails("named 'Other', expected 'ChatProject App Store'", app=profile(APP_ID, "Other"))

    def test_development_push_entitlement_is_rejected(self):
        self.assert_fails("no production aps-environment",
                          app=profile(APP_ID, "ChatProject App Store", **{"aps-environment": "development"}))

    def test_undecodable_secrets_are_rejected(self):
        self.assert_fails("APPSTORE_PROFILE_APP_BASE64 is not valid base64", app="%%%")
        self.assert_fails("APPSTORE_PROFILE_NSE_BASE64 is empty", nse="")
        self.assert_fails("APPSTORE_PROFILE_APP_BASE64 is not a .mobileprovision file",
                          app=base64.b64encode(b"not a profile").decode())

    def test_missing_team_line_is_rejected(self):
        self.assert_fails("no TEAM = '...' line", team_line="DEVELOPMENT_TEAM = nil")

    def test_the_real_project_script_has_a_team_line(self):
        text = (ROOT / "app/ios/scripts/add_notification_service.rb").read_text(encoding="utf-8")
        self.assertRegex(text, re.compile(r"^TEAM = '[A-Z0-9]{10}'$", re.M))


if __name__ == "__main__":
    unittest.main()
