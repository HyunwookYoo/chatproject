# Device tooling

Run from the repository root with the pinned Flutter SDK's Dart:

```sh
dart pub get --directory tools/e2e
dart run tools/e2e/bin/devices.dart ensure --count 2
dart run tools/e2e/bin/devices.dart test -- --name needle
```

Install Android SDK platform-tools, emulator, command-line tools under
`cmdline-tools/latest`, and the selected system image first. The tool does not
download SDK packages. Set `ANDROID_HOME` or `ANDROID_SDK_ROOT`, and optionally
`ANDROID_AVD_HOME`. `avd_config.json` is the canonical image selection:
local uses API 36 / google_apis_playstore / x86_64; `CI=true` uses API 36 /
google_apis / x86_64. AVD creation uses `avdmanager create avd --name ... --package ...`.
Existing `config.ini` image, ABI, and CPU architecture must match. An incompatible
AVD is preserved and reported rather than recreated.

Only `chat_e2e_1` and `chat_e2e_2` are candidates. All requested console and adb
ports are checked before the first launch. A busy port is an error: its holder
is never killed. Empty or mismatched console names require manual adb diagnosis;
the tool never runs `adb kill-server`. `ensure` boots only when required to save
the `clean` snapshot, checks the save exit status and actual directory, and stops
only the emulator it started. Both initial and test boots disable snapshot save
on exit. Already running AVDs remain running on success and failure.

`test` forwards arguments after `--` literally to `flutter test` in `app/`.
Unknown options before `--` are usage errors. It adds reverse mappings for
47000 and 3340 only when absent, using `--no-rebind`. Existing matching mappings
are reused without claiming ownership; conflicting mappings fail. Cleanup runs
in reverse acquisition order and attempts every resource. A test's nonzero exit
code survives cleanup failures; cleanup failure after success returns 1.

Managed child output is drained into `<run-directory>/<pid>.stdout.log` and
`<pid>.stderr.log`; `exitCode` completes after those streams are flushed. The
run directory is under ignored `build/e2e/devices/`. Ownership JSON records
process IDs, launch paths, AVDs, and reverse mappings. Raw failure/cleanup details
remain there as well. Do not publish those files: they can contain device IDs,
paths, addresses, or other private diagnostics. The old `run`, `startDetached`,
and `runInherited` APIs remain available; callers needing ownership use `start`.

Handled Ctrl+C closes resource admission, stops current owned children, waits
for in-flight acquisition and the command body to unwind, then performs final
cleanup and returns 130. On Windows, managed stop verifies the current process's
actual executable path and creation time while holding a process handle, then
uses `taskkill /PID <owned-pid> /T /F`. It never uses image-name matching or
signals a stale PID. Failed identity verification is reported for manual cleanup.
POSIX stop targets the retained direct child; detached descendants are outside
that guarantee. Signal subscription support is checked at runtime. Unsupported
signals, terminal hard termination, and host shutdown cannot promise cleanup.
The Windows test suite verifies child-tree stop and cancellation admission,
but skips the POSIX child-targeted SIGINT test; subscription success alone does
not prove Windows terminal interrupt delivery.

`up` and `test --keep` intentionally retain owned emulators and their records.
Those emulator launches use `start(..., retainOutput: true)`: POSIX executes the
native program with file descriptors redirected by `sh exec`; Windows retains a
small PowerShell wrapper that copies both native streams directly into its PID
log files. This lets the CLI exit while the emulator keeps writing output.
The retained mode accepts native executables, not batch scripts. Ordinary
managed starts retain the original direct-child behavior. Windows cleanup owns
the verified wrapper and its child tree. Both modes flush logs before `exitCode`.
New runs warn about existing ownership records. `down` is explicit safe record
cleanup: it retires records only after the owning run and recorded processes
are confirmed absent. It never kills a stale process, even if its PID/path seems
to match. Live, reused, inaccessible, or uncertain PIDs require manual diagnosis.
Stale reverse mappings remain recorded because the original owner being gone
does not establish that a mapping was not replaced. Inspect the ignored record,
verify the current process/AVD and mapping yourself, and clean only those you
intend to stop. `down` returns 1 while unresolved resources remain. Windows uses
a read-only process lookup; Linux requires `/proc`; other hosts fail closed.

```sh
cd tools/e2e
dart test
dart analyze
```

Tests use fake command executables and temporary AVD/config folders. They do
not start a real emulator, download an image, or access a physical phone.
