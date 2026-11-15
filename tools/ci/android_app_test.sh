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
#
# With APP_TEST_ATTEMPTS=2 a flutter tool failure is tried once more: the run passed its bound,
# or the tool lost the app's VM service or never started its development service. The app was
# alive and idle in those runs (a4c2b7a, 0a1bf23, dad7c17 account_privacy_test, a9216d3
# push_ios_test, whose tests all skip on Android); the cause is still not explained (R22, R23).
# A failing test is never tried again, and a phase that keeps state between runs leaves it unset.
set -uo pipefail

serial="${ANDROID_SERIAL:-emulator-5554}"
package="dev.chatproject.chat_app"
bound="${APP_TEST_TIMEOUT:-420}"
if [ "$#" -lt 1 ]; then
  echo "usage: android_app_test.sh <integration_test/x_test.dart> [flutter test options...]" >&2
  exit 2
fi
test_file="$1"

attempts="${APP_TEST_ATTEMPTS:-1}"

cd "$(dirname "$0")/../../app" || exit 1

app_state() {
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
}

output="$(mktemp)"
trap 'rm -f "$output"' EXIT
attempt=1
while :; do
  timeout --kill-after=30 "$bound" flutter test "$@" -d "$serial" 2>&1 | tee "$output"
  code=${PIPESTATUS[0]}
  [ "$code" -eq 0 ] && exit 0
  timed_out=""
  if [ "$code" -eq 124 ] || [ "$code" -eq 137 ]; then timed_out=1; fi
  tool=""
  if [ -n "$timed_out" ] || grep -Eq "VmServiceDisappearedException|Failed to start Dart Development Service" "$output"; then
    tool=1
  fi
  if [ -n "$tool" ] && [ "$attempt" -lt "$attempts" ]; then
    level="warning"
  else
    level="error"
  fi
  if [ -n "$timed_out" ]; then
    echo "::$level::$test_file did not finish within ${bound} s (exit $code); app state follows"
  else
    echo "::$level::$test_file failed (exit $code); app state follows"
  fi
  app_state
  if [ "$level" = "warning" ]; then
    attempt=$((attempt + 1))
    echo "== a flutter tool failure: trying $test_file once more (attempt $attempt of $attempts)"
    continue
  fi
  exit "$code"
done
