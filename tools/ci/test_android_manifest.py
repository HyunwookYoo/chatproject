"""The Android manifests keep mailbox mode optional (one remoteMessaging service, no boot start) and
take pushes through Firebase only; the test push receiver exists in debug builds alone. The merged
manifest of a release APK is checked for boot starts by check_release_hooks.py in CI."""
import pathlib
import unittest
import xml.etree.ElementTree as ET

SOURCES = pathlib.Path(__file__).resolve().parents[2] / "app/android/app/src"
MAIN = SOURCES / "main/AndroidManifest.xml"
DEBUG = SOURCES / "debug/AndroidManifest.xml"
ANDROID = "{http://schemas.android.com/apk/res/android}"
TOOLS = "{http://schemas.android.com/tools}"
BOOT_PERMISSION = "android.permission.RECEIVE_BOOT_COMPLETED"
RESCHEDULE = "androidx.work.impl.background.systemalarm.RescheduleReceiver"


def removed(element):
    return element.get(TOOLS + "node") == "remove"


def permissions(root):
    return {p.get(ANDROID + "name") for p in root.iter("uses-permission") if not removed(p)}


def receivers(root):
    return [r for r in root.iter("receiver") if not removed(r)]


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
            self.assertNotIn(BOOT_PERMISSION, permissions(root))
            self.assertNotIn("android.intent.action.BOOT_COMPLETED", actions(root))

    def test_workmanager_boot_receiver_is_removed_from_the_merge(self):
        # Final review 6 (Ruling R17 reversed): the daily renewal is scheduled again at the next
        # app start (ChatApplication, KEEP), never at boot.
        [permission] = [p for p in self.main.iter("uses-permission") if p.get(ANDROID + "name") == BOOT_PERMISSION]
        self.assertTrue(removed(permission))
        [receiver] = [r for r in self.main.iter("receiver") if r.get(ANDROID + "name") == RESCHEDULE]
        self.assertTrue(removed(receiver))

    def test_only_the_debug_manifest_has_the_test_push_receiver(self):
        for path, root in self.all.items():
            if path != DEBUG:
                self.assertEqual(receivers(root), [], f"no <receiver> in {path.parent.name}")
        [receiver] = receivers(self.debug)
        self.assertEqual(receiver.get(ANDROID + "name"), ".TestPushReceiver")
        self.assertEqual(receiver.get(ANDROID + "exported"), "true")
        self.assertEqual(actions(receiver), {"dev.chatproject.chat_app.TEST_PUSH"})


if __name__ == "__main__":
    unittest.main()
