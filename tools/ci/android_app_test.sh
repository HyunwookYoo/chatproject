#!/usr/bin/env bash
# Runs one app integration test on the CI emulator within a time bound:
#
#   bash tools/ci/android_app_test.sh <integration_test/x_test.dart> [flutter test options...]
#
# `flutter test -d <device>` waits without a bound for the app's VM service, its test isolate and
# the result, so an app that never reports holds the job until its limit (message_test.dart's
# read phase, 2026-11-15). Here each run ends after APP_TEST_TIMEOUT seconds (default 420; one run
# takes under 70 s), and a failed or timed-out run prints the app's state: its pid, its process
# record (state, frozen), the resumed activity and a capped, filtered logcat. The app logs no
# tokens, handles, tags, payloads or bodies (M7 Global Constraints); nothing here prints more.
set -uo pipefail

serial="${ANDROID_SERIAL:-emulator-5554}"
package="dev.chatproject.chat_app"
bound="${APP_TEST_TIMEOUT:-420}"
if [ "$#" -lt 1 ]; then
  echo "usage: android_app_test.sh <integration_test/x_test.dart> [flutter test options...]" >&2
  exit 2
fi
test_file="$1"

cd "$(dirname "$0")/../../app" || exit 1
timeout --kill-after=30 "$bound" flutter test "$@" -d "$serial"
code=$?
[ "$code" -eq 0 ] && exit 0

if [ "$code" -eq 124 ] || [ "$code" -eq 137 ]; then
  echo "::error::$test_file did not finish within ${bound} s (exit $code); app state follows"
else
  echo "::error::$test_file failed (exit $code); app state follows"
fi
pid="$(adb -s "$serial" shell pidof "$package" 2>/dev/null | tr -d '\r')"
echo "== pid: ${pid:-none}"
if [ -n "$pid" ]; then
  echo "== /proc/$pid/status"
  adb -s "$serial" shell cat "/proc/$pid/status" 2>/dev/null | grep -E '^(State|Threads|SigQ):' || true
fi
echo "== dumpsys activity processes $package"
adb -s "$serial" shell dumpsys activity processes "$package" 2>/dev/null | head -n 80 || true
echo "== resumed activity"
adb -s "$serial" shell dumpsys activity activities 2>/dev/null | grep -E 'ResumedActivity' | head -n 10 || true
if [ -n "$pid" ]; then
  echo "== logcat of pid $pid (last 300 lines)"
  adb -s "$serial" logcat -d -v threadtime --pid="$pid" 2>/dev/null | tail -n 300 || true
fi
echo "== logcat crash and app tags (last 200 lines)"
adb -s "$serial" logcat -d -v threadtime -s AndroidRuntime:E DEBUG:F libc:F flutter:I ChatProject:I DartVM:I Choreographer:I 2>/dev/null | tail -n 200 || true
echo "== logcat ActivityManager lines of $package (last 60 lines)"
adb -s "$serial" logcat -d -v threadtime -s ActivityManager:I ActivityTaskManager:I 2>/dev/null | grep -F "$package" | tail -n 60 || true
exit "$code"
