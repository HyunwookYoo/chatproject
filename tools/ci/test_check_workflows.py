import pathlib
import unittest

from check_workflows import check

PINNED = "apple-actions/upload-testflight-build@14df18fee5a4ff3b76971dd382f033494a0194e9"


def testflight(**overrides):
    doc = {
        "on": {"workflow_dispatch": None, "push": {"tags": ["testflight-*"]}},
        "permissions": {"contents": "read"},
        "env": {"RUST_TOOLCHAIN": "1.92.0", "CARGOKIT_TOOLCHAIN": "1.92.0"},
        "jobs": {"testflight": {
            "environment": "testflight",
            "steps": [
                {"name": "Refuse re-runs", "run": 'if [ "$GITHUB_RUN_ATTEMPT" != 1 ]; then exit 1; fi'},
                {"name": "Install provisioning profiles", "run": "ProvisionedDevices application-groups aps-environment"},
                {"name": "Locate IPA and check its contents", "run": 'bash tools/ci/check_ios_app.sh --signed "$app_dir"'},
                {"name": "Upload to TestFlight", "uses": PINNED},
            ],
        }},
    }
    doc.update(overrides)
    return doc


def ci(**overrides):
    doc = {
        "on": {"push": {"branches": ["main"]}, "pull_request": None},
        "permissions": {"contents": "read"},
        "jobs": {"ios-release": {"steps": [
            {"name": "Build the app for a device, unsigned", "run": "flutter pub get\nflutter build ios --release --no-codesign"},
            {"name": "Check the app and its extension", "run": "bash tools/ci/check_ios_app.sh app/build/ios/iphoneos/Runner.app"},
        ]}},
    }
    doc.update(overrides)
    return doc


class CheckTest(unittest.TestCase):
    path = pathlib.Path("testflight.yml")

    def test_a_good_testflight_workflow_passes(self):
        self.assertEqual(check(self.path, testflight()), [])

    def test_pull_request_trigger_is_rejected(self):
        problems = check(self.path, testflight(on={"pull_request": None}))
        self.assertTrue(any("triggers" in p for p in problems), problems)

    def test_missing_environment_is_rejected(self):
        doc = testflight()
        del doc["jobs"]["testflight"]["environment"]
        self.assertTrue(any("environment" in p for p in check(self.path, doc)))

    def test_missing_rerun_guard_is_rejected(self):
        doc = testflight()
        doc["jobs"]["testflight"]["steps"].pop(0)
        self.assertTrue(any("Refuse re-runs" in p for p in check(self.path, doc)))

    def test_artifact_upload_is_rejected(self):
        doc = testflight()
        doc["jobs"]["testflight"]["steps"].append(
            {"uses": "actions/upload-artifact@0123456789012345678901234567890123456789"})
        self.assertTrue(any("upload-artifact" in p for p in check(self.path, doc)))

    def test_unpinned_action_is_rejected(self):
        doc = testflight()
        doc["jobs"]["testflight"]["steps"][-1]["uses"] = "apple-actions/upload-testflight-build@v5"
        self.assertTrue(any("not pinned" in p for p in check(self.path, doc)))

    def test_testflight_yaml_is_checked_like_testflight_yml(self):
        # GitHub runs .yaml workflows too, so the rules must follow the file stem, not the extension.
        doc = testflight()
        del doc["jobs"]["testflight"]["environment"]
        problems = check(pathlib.Path("testflight.yaml"), doc)
        self.assertTrue(any("environment" in p for p in problems), problems)

    def test_rerun_guard_must_test_the_attempt_and_fail(self):
        for body in ("true", 'test "$GITHUB_RUN_ATTEMPT" = 1', "exit 1"):
            with self.subTest(body=body):
                doc = testflight()
                doc["jobs"]["testflight"]["steps"][0]["run"] = body
                self.assertTrue(any("GITHUB_RUN_ATTEMPT" in p for p in check(self.path, doc)))

    def test_bare_push_is_rejected(self):
        # A push trigger without a filter starts the job for every branch and tag.
        shapes = {
            "push: (null)": {"workflow_dispatch": None, "push": None},
            "push: {}": {"push": {}},
            "on: push": "push",
            "on: [push]": ["push"],
            "on: [push, workflow_dispatch]": ["push", "workflow_dispatch"],
        }
        for label, on in shapes.items():
            with self.subTest(on=label):
                problems = check(self.path, testflight(on=on))
                self.assertTrue(any("push may only be for tags" in p for p in problems), problems)

    def test_push_for_branches_or_other_tags_is_rejected(self):
        for push in ({"branches": ["main"]}, {"tags": ["v*"]}, {"tags": ["testflight-*"], "branches": ["main"]}):
            with self.subTest(push=push):
                problems = check(self.path, testflight(on={"push": push}))
                self.assertTrue(any("push may only be for tags" in p for p in problems), problems)

    def test_dispatch_only_and_tags_only_triggers_are_accepted(self):
        for on in ({"workflow_dispatch": None}, "workflow_dispatch", {"push": {"tags": ["testflight-*"]}}):
            with self.subTest(on=on):
                self.assertEqual(check(self.path, testflight(on=on)), [])

    def test_missing_profile_step_is_rejected(self):
        doc = testflight()
        del doc["jobs"]["testflight"]["steps"][1]
        self.assertTrue(any("validate the profiles" in p for p in check(self.path, doc)))

    def test_each_profile_check_is_required(self):
        for keyword in ("ProvisionedDevices", "application-groups", "aps-environment"):
            with self.subTest(removed=keyword):
                doc = testflight()
                step = doc["jobs"]["testflight"]["steps"][1]
                step["run"] = step["run"].replace(keyword, "")
                problems = check(self.path, doc)
                self.assertTrue(any("validate the profiles" in p for p in problems), problems)

    def test_the_ipa_check_must_run_the_shared_script_on_the_signed_app(self):
        for body in ("echo ok", 'bash tools/ci/check_ios_app.sh "$app_dir"'):
            with self.subTest(body=body):
                doc = testflight()
                doc["jobs"]["testflight"]["steps"][2]["run"] = body
                problems = check(self.path, doc)
                self.assertTrue(any("check_ios_app.sh --signed" in p for p in problems), problems)

    def test_missing_ipa_check_step_is_rejected(self):
        doc = testflight()
        del doc["jobs"]["testflight"]["steps"][2]
        self.assertTrue(any("check_ios_app.sh --signed" in p for p in check(self.path, doc)))

    def test_the_ipa_must_be_checked_before_it_is_uploaded(self):
        doc = testflight()
        steps = doc["jobs"]["testflight"]["steps"]
        steps[2], steps[3] = steps[3], steps[2]
        self.assertTrue(any("before it uploads" in p for p in check(self.path, doc)))


class CiTest(unittest.TestCase):
    path = pathlib.Path("ci.yml")

    def test_a_good_ci_workflow_passes(self):
        self.assertEqual(check(self.path, ci()), [])

    def test_missing_ios_release_job_is_rejected(self):
        doc = ci()
        del doc["jobs"]["ios-release"]
        problems = check(self.path, doc)
        self.assertTrue(any("ios-release is missing" in p for p in problems), problems)

    def test_ios_release_must_build_the_release_configuration_unsigned(self):
        for build in ("flutter build ios --debug --no-codesign", "flutter build ios --release", "flutter build ipa --release"):
            with self.subTest(build=build):
                doc = ci()
                doc["jobs"]["ios-release"]["steps"][0]["run"] = build
                problems = check(self.path, doc)
                self.assertTrue(any("--no-codesign" in p for p in problems), problems)

    def test_ios_release_must_check_the_built_app(self):
        doc = ci()
        del doc["jobs"]["ios-release"]["steps"][1]
        problems = check(self.path, doc)
        self.assertTrue(any("check_ios_app.sh" in p for p in problems), problems)

    def test_ci_yaml_is_checked_like_ci_yml(self):
        # GitHub runs .yaml workflows too, so the rules must follow the file stem, not the extension.
        doc = ci()
        del doc["jobs"]["ios-release"]
        problems = check(pathlib.Path("ci.yaml"), doc)
        self.assertTrue(any("ios-release is missing" in p for p in problems), problems)


if __name__ == "__main__":
    unittest.main()
