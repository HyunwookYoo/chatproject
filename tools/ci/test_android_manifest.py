"""The Android manifests keep mailbox mode optional: one remoteMessaging service, no boot start."""
import pathlib
import unittest
import xml.etree.ElementTree as ET

SOURCES = pathlib.Path(__file__).resolve().parents[2] / "app/android/app/src"
MAIN = SOURCES / "main/AndroidManifest.xml"
ANDROID = "{http://schemas.android.com/apk/res/android}"


def permissions(root):
    return {p.get(ANDROID + "name") for p in root.iter("uses-permission")}


class AndroidManifestTests(unittest.TestCase):
    def setUp(self):
        self.main = ET.parse(MAIN).getroot()
        # main, debug, profile and androidTest all merge into an APK.
        self.all = [ET.parse(path).getroot() for path in sorted(SOURCES.glob("*/AndroidManifest.xml"))]

    def test_one_remote_messaging_service_that_is_not_exported(self):
        services = [s for root in self.all for s in root.iter("service")]
        self.assertEqual(len(services), 1, "exactly one service")
        service = services[0]
        self.assertEqual(service.get(ANDROID + "name"), ".MailboxService")
        self.assertEqual(service.get(ANDROID + "foregroundServiceType"), "remoteMessaging")
        self.assertEqual(service.get(ANDROID + "exported"), "false")

    def test_mailbox_permissions(self):
        declared = permissions(self.main)
        for name in (
            "FOREGROUND_SERVICE",
            "FOREGROUND_SERVICE_REMOTE_MESSAGING",
            "POST_NOTIFICATIONS",
            "REQUEST_IGNORE_BATTERY_OPTIMIZATIONS",
        ):
            self.assertIn("android.permission." + name, declared)

    def test_no_boot_start(self):
        for root in self.all:
            self.assertNotIn("android.permission.RECEIVE_BOOT_COMPLETED", permissions(root))
            self.assertEqual(list(root.iter("receiver")), [], "no <receiver>")
            actions = {a.get(ANDROID + "name") for a in root.iter("action")}
            self.assertNotIn("android.intent.action.BOOT_COMPLETED", actions)


if __name__ == "__main__":
    unittest.main()
