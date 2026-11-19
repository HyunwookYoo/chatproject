# push_proxy

The stateless push proxy (spec `docs/superpowers/specs/2026-11-14-m7-push-design.md`
section 9). `/handle` seals a device's push token into a PushHandle; `/push`
opens handles and, after a random 0–3 s delay, sends to FCM (HTTP v1) or APNs
(HTTP/2, token auth). It writes nothing to disk: secret files are only read,
and the log has time, path, result codes, item count and elapsed time only.

It listens on plain HTTP, on localhost by default. TLS belongs to a reverse
proxy in front of it (Caddy below, with automatic Let's Encrypt).

## Build and run

```sh
cargo build --locked -p push_proxy --release
# binary: target/release/push_proxy
```

Configuration is environment variables and secret file paths only. Errors name
the variable, never its value.

| Variable | Meaning |
|---|---|
| `PUSH_LISTEN` | listen address, default `127.0.0.1:8787` |
| `PUSH_KEY`, `PUSH_KEY_ID` | current handle key (64 hex characters) and its id (0–255) |
| `PUSH_PREV_KEY`, `PUSH_PREV_KEY_ID` | optional previous key: still opens handles, never seals new ones |
| `FCM_SERVICE_ACCOUNT_FILE` | path of the Firebase service account JSON (required) |
| `APNS_KEY_FILE`, `APNS_KEY_ID`, `APNS_TEAM_ID`, `APNS_TOPIC`, `APNS_ENV` | APNs `.p8` key path, its key id, team id, the app's bundle id, `sandbox` or `production`. All or none; with none, APNs handles are refused |
| `PUSH_TRUST_PROXY` | `1` only behind the reverse proxy: the `/handle` rate limit then uses the last `X-Forwarded-For` entry. Unset or `0`: the TCP peer |

A new handle key: `openssl rand -hex 32`. To rotate, move the current key and id
to `PUSH_PREV_*` and set a new `PUSH_KEY` with a different id. Handles live 30
days, so drop the previous key 30 days after the rotation.

`APNS_ENV=sandbox` reaches `api.sandbox.push.apple.com` and serves Xcode debug
builds only; TestFlight and App Store builds need `production`.

What the proxy reaches out to: `oauth2.googleapis.com` and `fcm.googleapis.com`
(port 443), `api.push.apple.com` or `api.sandbox.push.apple.com` (port 443).
Certificates are checked against the system's roots.

## Files on the server

Only paths are listed here; the values never go in this repository.

| Path | Mode | Content |
|---|---|---|
| `/etc/push_proxy/env` | `0600 root` | the `PUSH_*` and `APNS_*` variables above, read by systemd |
| `/etc/push_proxy/fcm-service-account.json` | `0640 root:push-proxy` | Firebase console, Project settings, Service accounts, "Generate new private key" |
| `/etc/push_proxy/AuthKey_<key id>.p8` | `0640 root:push-proxy` | Apple Developer, Keys, the APNs key |

`/etc/push_proxy/env` example (values elided):

```sh
PUSH_LISTEN=127.0.0.1:8787
PUSH_KEY=<64 hex characters>
PUSH_KEY_ID=1
FCM_SERVICE_ACCOUNT_FILE=/etc/push_proxy/fcm-service-account.json
APNS_KEY_FILE=/etc/push_proxy/AuthKey_<key id>.p8
APNS_KEY_ID=<key id>
APNS_TEAM_ID=<team id>
APNS_TOPIC=dev.chatproject.chatapp
APNS_ENV=production
PUSH_TRUST_PROXY=1
```

## systemd

`/etc/systemd/system/push_proxy.service`:

```ini
[Unit]
Description=ChatProject push proxy
After=network-online.target
Wants=network-online.target

[Service]
User=push-proxy
Group=push-proxy
EnvironmentFile=/etc/push_proxy/env
ExecStart=/usr/local/bin/push_proxy
Restart=on-failure
RestartSec=2
# The proxy writes nothing; let it write nothing.
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
PrivateDevices=true
NoNewPrivileges=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
LockPersonality=true
MemoryDenyWriteExecute=true

[Install]
WantedBy=multi-user.target
```

```sh
useradd --system --no-create-home --shell /usr/sbin/nologin push-proxy
install -m 0755 target/release/push_proxy /usr/local/bin/push_proxy
systemctl daemon-reload
systemctl enable --now push_proxy
journalctl -u push_proxy -f   # the request and send lines
```

The process logs to stderr, which journald keeps. A restart loses only the
in-memory rate-limit counters and the cached OAuth token and APNs JWT; handles
keep opening as long as the keys are the same.

## Caddy (TLS, Let's Encrypt)

`/etc/caddy/Caddyfile`, with `push.example.com` pointing at the server and ports
80 and 443 open (Caddy obtains and renews the certificate itself):

```caddy
push.example.com {
	reverse_proxy 127.0.0.1:8787
}
```

Caddy replaces any client-sent `X-Forwarded-For` with the peer address it saw,
which is what `PUSH_TRUST_PROXY=1` relies on. Keep `PUSH_LISTEN` on
`127.0.0.1` so nothing reaches the proxy around Caddy. Caddy's access log is off
unless a `log` directive is added; leave it off, since it would record client
IPs.

The apps take the proxy address at build time: `CHATPROJECT_PUSH_URL=https://push.example.com`.

## Tests

```sh
cargo test --locked -p push_proxy
```

`tests/api.rs` drives the routes with a fake upstream. `tests/upstream.rs`
runs the real FCM and APNs senders against an in-process TLS HTTP/2 fake of
FCM, Google OAuth and APNs, with keys generated at run time.
