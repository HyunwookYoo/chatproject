#!/usr/bin/env python3
"""Prints the UDID of the iPhone simulator to test on: the newest iOS runtime,
and "iPhone 17" when that runtime has one.

Usage: xcrun simctl list devices available iOS --json > sim.json
       python3 pick_simulator.py sim.json
"""
import json
import sys

PREFIX = "com.apple.CoreSimulator.SimRuntime.iOS-"


def pick(listing):
    """Returns (udid, name, runtime). Exits with a message when there is no iPhone."""
    best = None
    for runtime, devices in (listing.get("devices") or {}).items():
        if not runtime.startswith(PREFIX):
            continue
        version = tuple(int(part) for part in runtime[len(PREFIX):].split("-"))
        for device in devices:
            if not device.get("isAvailable", True) or not device.get("name", "").startswith("iPhone"):
                continue
            key = (version, device["name"] == "iPhone 17")
            if best is None or key > best[0]:
                best = (key, device["udid"], device["name"], runtime)
    if best is None:
        sys.exit("no available iPhone simulator")
    return best[1], best[2], best[3]


if __name__ == "__main__":
    with open(sys.argv[1], encoding="utf-8") as f:
        udid, name, runtime = pick(json.load(f))
    print(f"picked {name} on {runtime}", file=sys.stderr)
    print(udid)
