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
                {"name": "Refuse re-runs", "run": 'test "$GITHUB_RUN_ATTEMPT" = 1'},
                {"name": "Install provisioning profiles", "run": "ProvisionedDevices application-groups aps-environment"},
                {"name": "Upload to TestFlight", "uses": PINNED},
            ],
        }},
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
        doc["jobs"]["testflight"]["steps"][2]["uses"] = "apple-actions/upload-testflight-build@v5"
        self.assertTrue(any("not pinned" in p for p in check(self.path, doc)))

    def test_testflight_yaml_is_checked_like_testflight_yml(self):
        # GitHub runs .yaml workflows too, so the rules must follow the file stem, not the extension.
        doc = testflight()
        del doc["jobs"]["testflight"]["environment"]
        problems = check(pathlib.Path("testflight.yaml"), doc)
        self.assertTrue(any("environment" in p for p in problems), problems)


if __name__ == "__main__":
    unittest.main()
