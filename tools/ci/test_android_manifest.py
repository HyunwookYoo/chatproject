"""The Android manifests keep mailbox mode optional (one remoteMessaging service, no boot start) and
take pushes through Firebase only; the test push receiver exists in debug builds alone."""
import pathlib
import unittest
import xml.etree.ElementTree as ET

SOURCES = pathlib.Path(__file__).resolve().parents[2] / "app/android/app/src"
MAIN = SOURCES / "main/AndroidManifest.xml"
DEBUG = SOURCES / "debug/AndroidManifest.xml"
ANDROID = "{http://schemas.android.com/apk/res/android}"


def permissions(root):
    return {p.get(ANDROID + "name") for p in root.iter("uses-permission")}


def actions(element):
    return {a.get(ANDROID + "name") for a in element.iter("action")}


class AndroidManifestTests(unittest.TestCase):
    def setUp(self):
        self.main = ET.parse(MAIN).getroot()
        self.debug = ET.parse(DEBUG).getroot()
        # main, debug, profile and androidTest all merge into an APK.
        self.all = {path: ET.parse(path).getroot() for path in sorted(SOURCES.glob("*/AndroidManifest.xml"))}

    def services(self):
        return {s.get(ANDROID + "name"): s for root in self.all.values() for s in root.iter("service")}

    def test_one_remote_messaging_service_that_is_not_exported(self):
        services = self.services()
        self.assertEqual(set(services), {".MailboxService", ".PushService"})
        service = services[".MailboxService"]
        self.assertEqual(service.get(ANDROID + "foregroundServiceType"), "remoteMessaging")
        self.assertEqual(service.get(ANDROID + "exported"), "false")

    def test_push_service_is_a_plain_firebase_receiver(self):
        [service] = [s for s in self.main.iter("service") if s.get(ANDROID + "name") == ".PushService"]
        # No foreground service: a push never shows a persistent notification.
        self.assertIsNone(service.get(ANDROID + "foregroundServiceType"))
        self.assertEqual(service.get(ANDROID + "exported"), "false")
        self.assertEqual(actions(service), {"com.google.firebase.MESSAGING_EVENT"})

    def test_application_and_default_channel(self):
        application = self.main.find("application")
        self.assertEqual(application.get(ANDROID + "name"), ".ChatApplication")
        meta = {m.get(ANDROID + "name"): m.get(ANDROID + "value") for m in application.iter("meta-data")}
        self.assertEqual(meta.get("com.google.firebase.messaging.default_notification_channel_id"), "messages")

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
        for root in self.all.values():
            self.assertNotIn("android.permission.RECEIVE_BOOT_COMPLETED", permissions(root))
            self.assertNotIn("android.intent.action.BOOT_COMPLETED", actions(root))

    def test_only_the_debug_manifest_has_the_test_push_receiver(self):
        for path, root in self.all.items():
            if path != DEBUG:
                self.assertEqual(list(root.iter("receiver")), [], f"no <receiver> in {path.parent.name}")
        [receiver] = list(self.debug.iter("receiver"))
        self.assertEqual(receiver.get(ANDROID + "name"), ".TestPushReceiver")
        self.assertEqual(receiver.get(ANDROID + "exported"), "true")
        self.assertEqual(actions(receiver), {"dev.chatproject.chat_app.TEST_PUSH"})


if __name__ == "__main__":
    unittest.main()
