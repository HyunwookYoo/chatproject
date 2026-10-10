import unittest

from pick_simulator import pick

RUNTIME = "com.apple.CoreSimulator.SimRuntime.iOS-{}"


def device(name, udid, available=True):
    return {"name": name, "udid": udid, "isAvailable": available}


class PickTest(unittest.TestCase):
    def test_newest_runtime_wins(self):
        listing = {"devices": {
            RUNTIME.format("26-2"): [device("iPhone 17", "old")],
            RUNTIME.format("26-5"): [device("iPhone Air", "new")],
        }}
        self.assertEqual(pick(listing)[0], "new")

    def test_prefers_iphone_17_within_a_runtime(self):
        listing = {"devices": {RUNTIME.format("26-5"): [
            device("iPhone 17 Pro", "pro"), device("iPhone 17", "plain"),
        ]}}
        self.assertEqual(pick(listing)[0], "plain")

    def test_skips_unavailable_and_non_iphone_devices(self):
        listing = {"devices": {
            RUNTIME.format("26-5"): [device("iPhone 17", "gone", available=False), device("iPad Air", "ipad")],
            RUNTIME.format("26-4"): [device("iPhone 17e", "ok")],
            "com.apple.CoreSimulator.SimRuntime.watchOS-26-5": [device("Apple Watch", "watch")],
        }}
        self.assertEqual(pick(listing)[0], "ok")

    def test_no_iphone_is_an_error(self):
        with self.assertRaises(SystemExit):
            pick({"devices": {}})


if __name__ == "__main__":
    unittest.main()
