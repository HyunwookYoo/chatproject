# iroh-probe: Spike A connectivity probe

`iroh-probe` measures one iroh 1.3 connection:

- how long connection setup takes;
- whether, and how soon, the connection leaves the relay for a direct (hole-punched) path;
- the round-trip time (RTT) on each path.

This is the tool for Spike A in `docs/superpowers/specs/2026-10-04-p2p-chat-design.md` (sections 5, 17 #3 and 18).

The endpoint is set up as design sections 5.1 and 5.2 describe: `presets::Minimal` plus
`RelayMode::Default`, which keeps the n0 public relays. No address lookup is configured, so
nothing contacts `dns.iroh.link`. A `RUST_LOG=trace` run confirmed this: the only `iroh.link`
hosts it touched were the four `*.relay.n0.iroh.link` relays. Each run creates a new key, so
every dial is a cold connection with no address cache.

## Build

```powershell
cd C:\ChatProject\spikes\a-connectivity
cargo build --release --target x86_64-pc-windows-msvc   # Windows
cargo build --release --target aarch64-linux-android    # physical phones, API 26+
cargo build --release --target x86_64-linux-android     # emulator
```

Each binary is written to `target\<target>\release\iroh-probe` (with `.exe` on Windows).

### Android setup

- **Linker and C compiler.** `.cargo/config.toml` sets the linker to the NDK r28c clang
  wrappers for API 26 (`*-linux-android26-clang.cmd`). It also sets `CC_<target>` and
  `AR_<target>` for the `cc` crate, which `ring` uses to compile its C and assembly. (`ring` is
  iroh's default `tls-ring` crypto backend.) If your NDK is installed somewhere else, change the
  paths in that file. Nothing else is needed: aws-lc is not in the dependency tree, and the
  binaries link only the system libraries `libc`, `libm` and `libdl`.
- **DNS.** iroh's resolver reads Android's DNS servers through JNI. A command-line binary run
  from `adb shell` has no JVM, so that lookup panics. The panic is caught, but its message is
  still printed. To avoid this, on Android the probe builds its resolver without the system
  configuration, and iroh uses its public fallback resolvers instead (Cloudflare, Google and
  Quad9, over UDP and DNS over HTTPS). The real app will call
  `iroh::dns::install_android_jni_context` from `JNI_OnLoad` instead.

## Commands

```
iroh-probe listen [--relay <url>]
iroh-probe dial <ticket> [--relay <url>] [--count N] [--relay-only]
```

- **`listen`** prints the ticket to stdout as one `endpoint…` line. The ticket holds the
  endpoint id, the relay URL and the direct addresses. Readable details go to stderr. The
  listener then echoes data on ALPN `probe/1` until you press Ctrl-C, and it can serve any
  number of dials.
- **`dial`** connects to the ticket and sends `N` 64-byte pings, 200 ms apart (default
  `N` = 50). It then prints one JSON object to stdout. On failure it prints
  `{"error": "..."}` and exits with code 1.
- **`--relay <url>`** uses this one relay instead of the n0 relays, for example the
  self-hosted Seoul relay. Pass the same URL on both sides: the dialer reaches the listener
  through the relay named in the ticket, and `--relay` sets the dialer's own home relay.
- **`--relay-only`** (dial only) ignores the ticket's direct addresses and dials through its
  relay URL alone. This is how connections start in production, because pkarr and the DHT
  publish only the relay URL (design section 5.1). Without this flag, the handshake tries every
  address in the ticket at once. If any address is reachable (LAN, public IP, UPnP mapping), the
  connection is direct from the start. Use this flag when you compare relay connect times.
- **`RUST_LOG=debug`** (or `iroh=trace`) writes logs to stderr.

## Windows side (the home PC)

```powershell
cd C:\ChatProject\spikes\a-connectivity
.\target\x86_64-pc-windows-msvc\release\iroh-probe.exe listen
```

Copy the `endpoint…` line and leave the listener running; one listener serves every dial.

- Connect the PC to the home line, by cable or home Wi-Fi. Do not tether it to the phone.
- If Windows Defender Firewall asks, allow `iroh-probe.exe` on Private networks. Blocking
  it can lower the direct rate.

To test Windows to Windows, open a second terminal on the same PC or another PC and run:

```powershell
.\target\x86_64-pc-windows-msvc\release\iroh-probe.exe dial <ticket>
.\target\x86_64-pc-windows-msvc\release\iroh-probe.exe dial <ticket> --relay-only
```

## Physical Android phone on mobile data

First, on the phone:

1. Turn on Developer options, then USB debugging.
2. Connect the phone with a USB cable. Do not use wireless adb, because it disconnects when
   Wi-Fi is turned off.
3. Accept the RSA prompt.

Then run these commands in PowerShell 7 (`pwsh`). Windows PowerShell 5.1 saves `>` output as
UTF-16, which many JSON tools cannot read.

```powershell
$adb = "$env:LOCALAPPDATA\Android\Sdk\platform-tools\adb.exe"
cd C:\ChatProject\spikes\a-connectivity
& $adb devices        # with several devices, add -s <serial> to every command below

& $adb push target\aarch64-linux-android\release\iroh-probe /data/local/tmp/iroh-probe
& $adb shell chmod 755 /data/local/tmp/iroh-probe

# Wi-Fi off, mobile data on
& $adb shell svc wifi disable    # or: & $adb shell cmd wifi set-wifi-enabled disabled
& $adb shell svc data enable
& $adb shell ip -4 addr          # expect a cellular interface (rmnet*, ccmni*) with an address, and no address on wlan0

# Run against the ticket from the Windows listener. Use one file per run.
& $adb shell /data/local/tmp/iroh-probe dial <ticket> > skt-lte-01.json
& $adb shell /data/local/tmp/iroh-probe dial <ticket> --relay-only > skt-lte-01-relayonly.json

# Afterwards
& $adb shell svc wifi enable
```

### LTE to LTE

Push the same binary to a second phone. Then run `listen` on one phone and `dial` on the other:

```powershell
& $adb -s <phoneA> shell /data/local/tmp/iroh-probe listen      # copy the ticket
& $adb -s <phoneB> shell /data/local/tmp/iroh-probe dial <ticket>
& $adb -s <phoneA> shell pkill iroh-probe                       # stop the listener
```

### Emulator

The emulator uses the x86_64 build:

```powershell
& $adb -s emulator-5554 push target\x86_64-linux-android\release\iroh-probe /data/local/tmp/iroh-probe
& $adb -s emulator-5554 shell chmod 755 /data/local/tmp/iroh-probe
& $adb -s emulator-5554 shell /data/local/tmp/iroh-probe dial <ticket>
```

In Git Bash, set `MSYS_NO_PATHCONV=1` first. Otherwise Git Bash rewrites `/data/local/tmp/...`
into a Windows path.

## Reading the JSON

This example is the emulator dialing the Windows listener with `--relay-only`. Long lists are
shortened.

```json
{
  "connect_ms": 161.763,
  "time_to_direct_ms": 344.435,
  "final_path": "direct",
  "rtt_ms": {
    "direct": { "n": 49, "p50": 1.334, "p95": 1.815 },
    "mixed":  { "n": 1, "p50": 186.603, "p95": 186.603 },
    "relay":  { "n": 0, "p50": null, "p95": null }
  },
  "count": 50,
  "online_ms": 633.608,
  "relay_used": "https://aps1-1.relay.n0.iroh.link./",
  "local": {
    "endpoint_id": "a588…",
    "home_relay": "https://aps1-1.relay.n0.iroh.link./",
    "addrs": ["10.0.2.15:41098", "203.0.113.10:56108", "[fec0::5054:ff:fe12:3456]:40831"]
  },
  "remote": { "endpoint_id": "f9f6…", "dialed_addrs": ["relay:https://aps1-1.relay.n0.iroh.link./"] },
  "path_changes": [
    { "t_ms": 162.035, "path": "relay",  "remote": "relay:https://aps1-1.relay.n0.iroh.link./" },
    { "t_ms": 344.435, "path": "direct", "remote": "ip:172.27.96.1:56067" },
    { "t_ms": 5356.723, "path": "direct", "remote": "ip:192.168.55.159:56067" }
  ],
  "paths_at_end": [
    { "path": "relay",  "selected": false, "remote": "relay:https://aps1-1.relay.n0.iroh.link./", "local": "Relay(…)", "quic_rtt_ms": 126.411 },
    { "path": "direct", "selected": true,  "remote": "ip:192.168.55.159:56067", "local": "Ip(Some(10.0.2.16))", "quic_rtt_ms": 5.64 }
  ]
}
```

| Field | Meaning |
|---|---|
| `connect_ms` | Time from the start of `Endpoint::connect` until the handshake completes. Endpoint startup is not included; that is `online_ms`. |
| `time_to_direct_ms` | Time from the start of the dial until a direct (IP) path is first selected. `null` means the connection did not go direct during the run, which lasts about `count` × 200 ms plus the RTTs (about 10 s for 50 pings). If the handshake itself went direct, this is about the same as `connect_ms`. |
| `final_path` | The path selected at the end of the run: `direct`, `relay` or `none`. |
| `rtt_ms.relay`, `rtt_ms.direct` | Sample count (`n`), p50 and p95 of the ping RTT, using nearest-rank percentiles. Each sample is tagged with the path selected when the ping was sent and when the echo arrived. |
| `rtt_ms.mixed` | Samples where the selected path changed during the round trip. They are not counted under `relay` or `direct`. |
| `online_ms` | Time from endpoint start until the home relay is connected. |
| `relay_used` | The relay on the connection's relay path, which is the listener's home relay from the ticket. |
| `local` | This device's endpoint id and home relay, plus its known addresses: its interface addresses and the public address the relay reported back (QUIC address discovery). |
| `remote` | The listener's endpoint id and the addresses that were actually dialed. |
| `path_changes` | One entry per change of the selected path. `t_ms` counts from the start of the dial; the first entry is the path at connect time. |
| `paths_at_end` | The paths open at the end, each with QUIC's own RTT estimate. The relay path stays open as a backup, so its `quic_rtt_ms` gives the relay RTT even when `rtt_ms.relay.n` is 0. |

For Spike A, the direct rate is the share of runs where `time_to_direct_ms` is not `null`. To
put a folder of results into one table:

```powershell
Get-ChildItem *.json | ForEach-Object {
  $j = Get-Content $_ -Raw | ConvertFrom-Json
  [pscustomobject]@{ file = $_.Name; connect = $j.connect_ms; to_direct = $j.time_to_direct_ms;
    final = $j.final_path; relay_p50 = $j.rtt_ms.relay.p50; direct_p50 = $j.rtt_ms.direct.p50 }
} | Format-Table
```

## Smoke tests (2026-10-04, this PC, n0 relays)

Both ends picked `aps1-1` (Singapore) as their home relay.

| Setup | connect_ms | time_to_direct_ms | final | direct RTT p50 / p95 (ms) |
|---|---|---|---|---|
| (a) Windows ↔ Windows, full ticket | 1.1 | 1.2 | direct | 0.31 / 0.39 |
| (a) Windows ↔ Windows, `--relay-only` | 169 | 356 | direct | 0.31 / 0.36 |
| (b) Windows listen ↔ emulator dial, full ticket | 6.3 | 6.4 | direct | 1.48 / 2.15 |
| (b) Windows listen ↔ emulator dial, `--relay-only` | 162 | 344 | direct | 1.33 / 1.82 |
| Emulator listen ↔ Windows dial, full ticket, `--count 20` | 179 | 379 | direct | 1.47 / 1.79 |
| Windows ↔ Windows, `--relay https://euc1-1.relay.n0.iroh.link./` on both sides, `--relay-only`, `--count 5` | 518 | 1079 | direct | 0.28 / 0.32 |

- In the emulator-listen run, the emulator's addresses cannot be reached from the PC, so the
  connection started on the relay and hole punching through QEMU's NAT made it direct.
- In every run that started on the relay, one ping was tagged `mixed` because the path switched
  during it: about 190–200 ms through Singapore and 560 ms through the EU relay. No sample was
  taken purely on the relay.
- The QUIC estimate for the relay path was 89–142 ms through Singapore and 461 ms through the
  EU relay.
- With the EU relay, connect time rose to 518 ms, which shows how relay distance adds to
  connect time.
- With the emulator's Wi-Fi turned off (`svc wifi disable`), a dial still went direct in 4.6 ms
  over the emulated cellular interface. Wi-Fi was turned back on afterwards.

These tests show that the tool works. They say nothing about mobile networks.

## Caveats

- **The emulator is not representative.** Its networking goes through QEMU user-mode NAT on
  this PC. It reaches the PC's LAN addresses through the PC's own network stack, so it goes
  direct within milliseconds, and its RTT is a path inside one machine. Only a physical phone
  on mobile data tests carrier NAT. The same applies to tests between two processes on one
  machine.
- **Direct addresses in the ticket.** Without `--relay-only`, a reachable direct address skips
  the relay entirely. Then `connect_ms` says nothing about the relay.
- **Fast switches leave no relay samples.** When hole punching takes a few hundred
  milliseconds, the switch to direct happens during the first ping. Read the relay RTT from
  `paths_at_end[].quic_rtt_ms` instead.
- **The n0 public relays** are rate-limited and meant for development. The nearest one to
  Korea is in Singapore (`aps1`).
- **Self-hosted relay.** iroh-relay ships with `enable_quic_addr_discovery = false`. Turn it on
  (it needs TLS) and open UDP 7842. Otherwise devices that use only that relay cannot learn
  their public address, and the direct rate falls for reasons unrelated to where the relay is.
- **Android DNS.** The probe uses public DNS resolvers, not the carrier's. This affects
  `online_ms`. It affects `connect_ms` only when the listener's relay differs from the dialer's.
- **`adb shell` is not an app.** It runs as the `shell` user, outside app restrictions such as
  Doze, background limits and Data Saver. An app process may behave differently.
- **IPv6.** Korean carriers give phones IPv6 addresses. This PC has no IPv6 address, so a
  direct path to it must use IPv4. Check `local.addrs`.
- **Cold connections.** Each run uses a new key and a new endpoint. The app will also have
  address caches and connections that are already warm (design section 14.1).
