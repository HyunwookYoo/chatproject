# M3 Android public identity URI — native follow-up

Status: **PASS for the accepted Debug APK on an owned Android emulator**. Executed on 2026-10-20 at source `9d1f3945b4b2f475cf757688c14e968e87319d49`, documentation HEAD `f39774098b1bd60f615cc8fbb353710b3f39c139`. This supplements the [M3 results](2026-10-18-m3-account-devices.md); it does not establish full design §10 acceptance.

The exact previously accepted APK was `build/e2e/m3-whole-fix-artifacts/auth-debug-final.apk`, SHA256 `a206bc74072c64ac6d328cd58f4c90a174583252bfae64dc1cae55911d228e61`. No rebuild or product change was needed. One owned `chat_e2e_1` emulator used its existing clean snapshot, Android API36/x86_64. Existing local record services were injected before opening the same account actor used by Flutter. The controller used `relay:null`; there was no relay/node process or public DHT run.

| Check | Actual evidence |
|---|---|
| Android URI registration | Implicit `ACTION_VIEW` with `BROWSABLE` resolved to `dev.chatproject.chat_app/.MainActivity`; URI launches used neither explicit component nor package restriction. |
| Cold launch and queue | Android reported `LaunchState: COLD`. Recipient was Unconfigured, with zero contacts/peers. After configuring its isolated test account, the real Flutter UI displayed the signed invitation preview and add button; contact count remained zero. |
| Explicit save | A tap on the rendered `연락처 추가` button created one contact. Both `verified` and device authentication remained false; no peer connection was made. No fixture accept command substituted for the button. |
| Warm delivery | A second signed invitation with a distinct display name reached the real preview through the running activity. PID and core instance were unchanged; no additional contact or authorization was granted. |
| Invalid input | A static public invalid invitation showed `서명을 확인할 수 없는 초대예요.` without a preview or state change. |
| Pair-prefix exclusion | A static invalid pair URI reached the scheme handler but did not open invitation preview or pairing UI. |

The configured account starts its local endpoint, so the recorded network state was Online with zero peers. Online was not counted as authentication. The local profile disables relay and public DHT address lookup; only the controller/record service is claimed to bind loopback. The endpoint's bind address is not claimed to be loopback-only.

The passing third attempt exited0. Safe UI hierarchies and filtered owning-handle snapshots prove preview and core state, rather than intent delivery alone. The recipient remained PID3431/core instance `3431-18e03fdf26352f30` through cold configuration, explicit save and warm tests. Controller account payloads were redacted before logging; generated invitations, recovery phrases, QR and SAS were not retained. No screenshots, logcat or activity dumps were collected. In-process scans found no generated invitation/full recovery phrase in retained artifacts; whole-process memory erasure is not claimed.

Two earlier scratch failures remain preserved: the first incorrectly required a configured endpoint to remain Stopped; the second used `adb shell` instead of the existing `adb exec-out uiautomator dump /dev/tty` capture protocol. Neither establishes a product failure. The first immediate cleanup receipt also remained false while adb unregistered the stopped emulator; its later independent empty inventory is recorded separately. The third attempt's owned activity, reverse, controller and emulator cleanup passed after bounded unregister observation. A subsequently connected physical phone belongs to the separate next task and was untouched by this run.

All350 frozen source inputs and the three protected M2 documents still matched their accepted hashes; the index was empty. No source fix or additional source review wave occurred. Exact commands, all attempt receipts, safe UI files, ownership records and evidence hashes remain under `.superpowers/sdd/2026-10-18-m3-c-app-validation/native-uri-2026-10-20/`, including `report.md`, `attempt-3/run.dart`, `attempt-3/receipts.json`, `final-provenance.json` and `final-boundary-inventory.json`. Reproduction uses the pinned Dart SDK from the worktree root:

```text
dart --packages=tools/e2e/.dart_tool/package_config.json .superpowers/sdd/2026-10-18-m3-c-app-validation/native-uri-2026-10-20/run.dart
```

Release URI runtime, physical Android camera/device behavior, OS app-switch privacy, Apple execution and Windows protocol registration/warm forwarding remain open. This result narrows the Android URI gate to an executed Debug emulator path; the other M3 acceptance groups retain their prior dispositions.
