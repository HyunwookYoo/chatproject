# Spike C: FCM on a real Android device

This spike measures real FCM behaviour for design §8.3 and §18 (spike C, FCM part). The APNs part is separate.

| # | Question | Scenario |
|---|---|---|
| 1 | How long does an online, high-priority data message take to arrive? | [S1](#s1-online-latency) |
| 2 | Under Doze, which messages arrive: high or normal priority? | [S2](#s2-doze-high-vs-normal) |
| 3 | How many non-collapsible messages survive while the device is offline? The docs say 100 are stored, and an overflow drops them all and calls `onDeletedMessages()`. | [S3](#s3-offline-storage) |
| 4 | What happens around the 4,096 B payload limit? The design's push bucket is 2,900 B or less of MLS ciphertext, which is 3,868 B as base64. | [S4](#s4-payload-size) |
| 5 | Are messages delivered after the app is swiped from recents? After a force-stop? | [S5](#s5-swiped-vs-force-stopped) |

## Layout

- `fcm-probe/` is a Flutter app with a trivial UI. FCM is handled natively in Kotlin, in `android/app/src/main/kotlin/dev/chatproject/spike/fcmprobe/`. The package name is `dev.chatproject.spike.fcmprobe`.
  - The app writes its FCM token to `files/token.txt` and logs it with tag `FcmProbe`.
  - Each message adds one JSON line to `files/recv.jsonl` and posts a notification. The line has these fields:
    - `recv_ms`: the device clock.
    - `seq` and `sent_ms`: from the payload. `sent_ms` is the PC clock.
    - `payload_bytes`: UTF-8 bytes of the data keys plus values.
    - `priority` and `originalPriority`.
  - `onDeletedMessages()` adds `{"deleted":true,"recv_ms":...}`.
- `sender/` holds the Node 24 scripts. They have no npm dependencies.
  - `send.mjs token` checks that the device has a token. It prints only the token's length and a hash prefix.
  - `send.mjs burst` sends data messages through FCM HTTP v1, 300 ms apart, because FCM allows at most 240 messages a minute to one Android device. It records what it sent in `last-burst.json`.
  - `collect.mjs` pulls `recv.jsonl`, estimates the device clock offset and reports on the last burst.
- `secrets/` holds `google-services.json` and `fcm-service-account.json`. Never commit or paste them. See [secrets/README.md](secrets/README.md).

## Setup

PowerShell:

```powershell
$env:Path += ";$env:LOCALAPPDATA\Android\Sdk\platform-tools"
adb devices                  # find the serial
$S = "emulator-5554"         # or the phone's serial
```

In bash, use `S=<serial>` instead. The commands below are the same in both shells.

Build, install and launch:

```powershell
cd C:\ChatProject\spikes\c-push\fcm-probe
flutter build apk --debug
adb -s $S install -r build\app\outputs\flutter-apk\app-debug.apk
adb -s $S shell am start -n dev.chatproject.spike.fcmprobe/.MainActivity
```

- The build copies `secrets\google-services.json` to `fcm-probe\android\app\`, which is gitignored.
  - If the secret is missing, the build writes a DUMMY file instead (its `_comment` says so). The app then builds and runs, but logs `getToken failed`. Add the real file and rebuild.
- The debug APK contains arm64-v8a, armeabi-v7a and x86_64 code, so the same APK installs on phones and on the emulator.
- Allow notifications when the app asks (Android 13+), or run `adb -s $S shell pm grant dev.chatproject.spike.fcmprobe android.permission.POST_NOTIFICATIONS`. FCM may downgrade high-priority messages that do not lead to a visible notification.

Check the token:

```powershell
cd ..\sender
node send.mjs token --serial $S
```

Run every `node` command from `sender\`. `collect.mjs` reuses the serial of the last burst, and `--verbose` lists each message.

Record these with every result:

- Model and Android version: `adb -s $S shell getprop ro.product.model` and `getprop ro.build.version.release`.
- Play services version: `adb -s $S shell "dumpsys package com.google.android.gms | grep versionName"`.
- Network: Wi-Fi, LTE or 5G, and the carrier.
- App standby bucket: `adb -s $S shell am get-standby-bucket dev.chatproject.spike.fcmprobe`.

The emulator is a Google Play image, so S1 and S4 can run there. Run S2, S3 and S5 on a real phone over USB. Wireless adb drops in airplane mode.

## Scenarios

### S1. Online latency

Keep the screen on and the network up. Open the app once, then press Home.

```powershell
node send.mjs burst --count 20 --size 3900 --priority high --serial $S
node collect.mjs --verbose
```

Run this 3 times per network (Wi-Fi, LTE/5G). `--size 3900` is about the push bucket: 3,868 B of base64 plus `seq` and `sent_ms`.

### S2. Doze, high vs normal

Put the device into deep Doze:

```powershell
adb -s $S shell dumpsys battery unplug
adb -s $S shell input keyevent KEYCODE_SLEEP
adb -s $S shell dumpsys deviceidle force-idle
adb -s $S shell dumpsys deviceidle get deep      # must print IDLE
```

1. Send high-priority messages while idle:

   ```powershell
   node send.mjs burst --count 10 --size 3900 --priority high --serial $S
   # wait 1 minute
   node collect.mjs --verbose
   ```

2. Send normal-priority messages while still idle:

   ```powershell
   node send.mjs burst --count 10 --size 3900 --priority normal --serial $S
   # wait 5 minutes
   adb -s $S shell dumpsys deviceidle get deep    # still IDLE?
   node collect.mjs --verbose
   ```

3. Leave Doze, and note the time you run `unforce`:

   ```powershell
   adb -s $S shell dumpsys deviceidle unforce
   adb -s $S shell dumpsys battery reset
   adb -s $S shell input keyevent KEYCODE_WAKEUP
   # wait 1 minute
   node collect.mjs --verbose
   ```

Android documents that high-priority messages arrive within seconds even in Doze. Normal-priority messages wait until Doze ends or a maintenance window opens. In step 3, compare each message's `recv` time with the time of `unforce`.

### S3. Offline storage

Run this once each with `--count 100`, `101` and `150`:

```powershell
adb -s $S shell cmd connectivity airplane-mode enable
# Check that the device really has no network. Some phones keep Wi-Fi on in airplane mode;
# if so, also run: adb -s $S shell svc wifi disable
# wait 1 minute
node send.mjs burst --count 100 --size 200 --priority high --serial $S    # every send should be HTTP 200
adb -s $S shell cmd connectivity airplane-mode disable    # plus `svc wifi enable` if you disabled it
# wait 2 minutes
node collect.mjs
```

- Per the Firebase docs, 100 or fewer messages should all arrive. Beyond 100, nothing should arrive and `deleted:` should show `onDeletedMessages`.
- Keep the device online for a few minutes between runs so that nothing is still queued.
- Android shows at most 50 notifications per app. Clear the notification shade between runs. `recv.jsonl` is the record, not the shade.
- Optional variants:
  - `--count 10 --collapse c1` should deliver only the newest message.
  - `--ttl 60` with more than 2 minutes offline should deliver nothing.

### S4. Payload size

```powershell
node send.mjs burst --count 2 --size 3900 --priority high --serial $S
node collect.mjs
```

1. Repeat with `--size` 4000, 4077, 4096, 4097 and 4200.
2. For each size, note the HTTP status. Above the limit, expect `HTTP 400` with `INVALID_ARGUMENT`.
3. Also note whether `collect` shows `payload: <size> B`.

`--size` sets keys plus values. `send.mjs` also prints the JSON size, which is 19 B larger, so `--size 4077` is 4,096 B as JSON. The step where sends start to fail shows how FCM counts. If 4077 also fails, bisect downward.

### S5. Swiped vs force-stopped

1. Swipe the app away:

   ```powershell
   adb -s $S shell am start -n dev.chatproject.spike.fcmprobe/.MainActivity
   # on the device: open Recents and swipe the app away
   adb -s $S shell "dumpsys package dev.chatproject.spike.fcmprobe | grep stopped="   # stopped=false
   node send.mjs burst --count 5 --size 3900 --priority high --serial $S
   node collect.mjs --verbose
   ```

2. Force-stop the app:

   ```powershell
   adb -s $S shell am force-stop dev.chatproject.spike.fcmprobe
   adb -s $S shell "dumpsys package dev.chatproject.spike.fcmprobe | grep stopped="   # stopped=true
   node send.mjs burst --count 5 --size 3900 --priority high --serial $S
   # wait 1 minute
   node collect.mjs
   adb -s $S shell "logcat -d | grep -i 'result=CANCELLED'"   # Play services refused a stopped app
   adb -s $S shell am start -n dev.chatproject.spike.fcmprobe/.MainActivity
   # wait 1 minute
   node collect.mjs     # do the messages sent while stopped arrive now, or are they lost?
   ```

On AOSP and Pixel, expect delivery after a swipe and no delivery while force-stopped. Some OEMs treat a swipe as a force-stop, so record the model.

## Reading the results

`collect.mjs` prints one line per item:

| Line | Meaning |
|---|---|
| `clock` | Device clock minus PC clock. It is the largest (device time − PC arrival time) over 10 samples of `adb shell date +%s%3N`, accurate to a few ms. The emulator measured about −0.29 s; phones can be further off. |
| `burst` | Parameters and start time of the last `send.mjs burst`. |
| `FCM` | How many sends FCM accepted with HTTP 200. |
| `received`, `missing` | Accepted `seq` numbers the app logged or did not log. Lines match on (`seq`, `sent_ms`), so old lines in `recv.jsonl` do no harm. |
| `latency` | `recv_ms − offset − sent_ms`, as p50, p95 and max. It runs from just before the HTTPS POST to FCM until `onMessageReceived`. That includes the PC-to-FCM request (`send.mjs` prints its duration per message), FCM itself, the push connection, and process start if the app was not running. |
| `priority` | `delivered/original` counts. `normal/high` means FCM downgraded the message. |
| `payload` | `payload_bytes` as received: keys plus values. |
| `deleted` | `onDeletedMessages()` calls after the burst started. Each one means FCM dropped stored messages. |

Raw data:

- `adb -s $S shell run-as dev.chatproject.spike.fcmprobe cat files/recv.jsonl`
- `adb -s $S logcat -s FcmProbe`

The logcat output contains the FCM token, so don't paste it anywhere public.

To start over: `adb -s $S shell run-as dev.chatproject.spike.fcmprobe rm files/recv.jsonl`.
