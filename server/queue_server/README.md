# queue_server

The queue server (spec `docs/superpowers/specs/2026-11-22-m8-queue-design.md`
section 9). A receiving device that has no mailbox device makes one queue per
conversation; senders put ciphertexts in it; the device takes them with `RECV`
and drops them with `ACK`. The server knows queue ids, two public keys per
queue, ciphertexts and times, and nothing else. Unlike `push_proxy` it keeps
state: queues and entries survive a restart.

It listens on plain HTTP, on localhost by default. TLS belongs to the reverse
proxy (Caddy routes `/q/*` here, on the push proxy's domain).

## Build and run

```sh
cargo build --locked -p queue_server --release
# binary: target/release/queue_server
```

The server (the push proxy's 1 GB VM) takes the binary from CI: the
`push-proxy` job of every `ci` run uploads `queue_server-x86_64-linux` next to
`push_proxy-x86_64-linux`, built on Ubuntu 24.04 for x86_64 (the server needs
the same or a newer glibc). Download it from a green run of `main` and copy it
over:

```sh
gh run download <run-id> -n queue_server-x86_64-linux -D queue_server-bin
scp queue_server-bin/queue_server ubuntu@<server>:/tmp/queue_server
```

The push proxy's [`deploy/setup.sh`](../push_proxy/deploy/setup.sh) sets up
both servers, since they share the VM, the swap, the firewall and one Caddy
site: it installs the binary, the user, the systemd unit and the Caddyfile
below, which it checks with `caddy validate` before it replaces the old one.
Copy the binary to `/tmp/queue_server` (on the first run also the push proxy's
files, see its [README](../push_proxy/README.md)), then run
`sudo bash /tmp/setup.sh <domain>`. Run it again with a new binary to upgrade;
it never touches `/var/lib/queue_server`, so the queues stay. Change the unit or
the Caddyfile here and in the script together.

Configuration is environment variables only. Errors name the variable, never
its value.

| Variable | Meaning |
|---|---|
| `QUEUE_LISTEN` | listen address, default `127.0.0.1:8788` |
| `QUEUE_DB` | SQLite file, default `/var/lib/queue_server/queue.db`; created on first start |
| `QUEUE_MAX_BYTES` | used bytes of the file (SQLite pages in use) past which `SEND` is `limited`, default 2 GiB |
| `QUEUE_TRUST_PROXY` | `1` only behind the reverse proxy: per-client limits then use the last `X-Forwarded-For` entry. Unset or `0`: the TCP peer |

## Files on the server

| Path | Mode | Content |
|---|---|---|
| `/var/lib/queue_server/` | `0700 queue-server` | systemd's `StateDirectory`: `queue.db` and its `-wal` and `-shm` files. The only place the server writes |

There is no secret and no env file: the variables are in the unit.

## systemd

`/etc/systemd/system/queue_server.service`:

```ini
[Unit]
Description=ChatProject queue server
After=network-online.target
Wants=network-online.target

[Service]
User=queue-server
Group=queue-server
Environment=QUEUE_LISTEN=127.0.0.1:8788
Environment=QUEUE_DB=/var/lib/queue_server/queue.db
Environment=QUEUE_TRUST_PROXY=1
ExecStart=/usr/local/bin/queue_server
Restart=on-failure
RestartSec=2
# 128 connections hold at most ~221 MiB of request bodies (src/limit.rs).
MemoryMax=256M
MemorySwapMax=0
# The server writes its database and nothing else.
StateDirectory=queue_server
StateDirectoryMode=0700
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
useradd --system --no-create-home --shell /usr/sbin/nologin queue-server
install -m 0755 target/release/queue_server /usr/local/bin/queue_server
systemctl daemon-reload
systemctl enable --now queue_server
journalctl -u queue_server -f   # the request and sweep lines
```

`MemoryMax` is the connection cap's bound (`MAX_CONNECTIONS` in
`src/limit.rs`) plus room for the process itself. `MemorySwapMax=0` keeps the
server out of the VM's swap, so past `MemoryMax` the kernel drops its page
cache, then kills it, and systemd starts it again. A restart loses only the
in-memory rate buckets; the queues and entries are in the database.

## Caddy

The push proxy's site, with `/q/*` routed here. `/etc/caddy/Caddyfile`, with
`push.example.com` pointing at the server and ports 80 and 443 open:

```caddy
push.example.com {
	handle /q/* {
		reverse_proxy 127.0.0.1:8788
	}
	handle {
		reverse_proxy 127.0.0.1:8787
	}
}
```

The paths keep their `/q/` prefix (`handle`, not `handle_path`): the server
routes `/q/create` and the rest as they are. Caddy replaces any client-sent
`X-Forwarded-For` with the peer address it saw, which is what
`QUEUE_TRUST_PROXY=1` relies on; keep `QUEUE_LISTEN` on `127.0.0.1` so nothing
reaches the server around Caddy. Leave Caddy's access log off: it would record
client IPs.

## Commands

`POST /q/create`, `/q/send`, `/q/recv`, `/q/ack`, `/q/delete`. Body
`{"items": [...]}` with at most 100 items (`queue_wire`), answer
`{"results": [...]}` in the same order. Every item is signed on its own
(`queue_wire::signing_bytes`); `ts` must be within ±10 minutes of the server.

| Command | Signed by | Results |
|---|---|---|
| `CREATE` | recipient | `ok` (new, or the same keys again), `exists` (other keys), `limited` |
| `SEND` | sender | `ok` (stored, or already stored or acked in the last 20 minutes), `gone`, `full` (256 unacked), `limited` (rate or size cap) |
| `RECV` | recipient | `entries` after `since_seq`, `gone`, `limited` (this answer is full: ask again) |
| `ACK` | recipient | `ok`, `gone` |
| `DELETE` | recipient | `ok` (also when the queue is already gone) |

Any item that does not decode, has an empty or over-64 KiB `ct`, a `ts` out of
range or a bad signature is `bad`; the other items still run.

- `seq` rises per queue and never restarts, also after the queue drains: ask
  with the last seq you saw.
- A `RECV` answer is at most `MAX_BODY` (1 MiB). A queue that got entries may
  have more; one that got `limited` got none this time.
- Request errors: 400 (not JSON, over 100 items), 413 (body over 1 MiB), 429
  (`RECV`/`ACK` rate), 408 (request over 10 s), 500 (store failure).

## Limits and expiry

- `CREATE` 500 an hour, burst 200, per client (an IPv4 address or an IPv6 /64).
  `SEND` 60 a minute, burst 20, per queue. `RECV` and `ACK` 120 requests a
  minute per client, each. Buckets are in memory and start full after a restart.
  A `CREATE` or `SEND` item takes its token before its signature is checked,
  so a badly signed item spends a token and one over the limit costs no verify.
- At most 128 open connections; one more is closed at once. That bounds the
  server's memory to about 221 MiB of held 1 MiB uploads.
- Every hour (and at start): entries older than 24 hours, acked hashes past
  their 20 minutes, and queues with no command for 30 days are deleted.

## Storage and log

Tables, all of them: `queue (id, recipient_pub, sender_pub, last_used_ms)`,
`entry (queue_id, seq, received_ms, ct_hash, ct)`,
`acked (queue_id, ct_hash, drop_at_ms)`. One connection on one worker thread,
WAL, `synchronous=NORMAL`: a killed process loses nothing; an OS crash can lose
the last commits, which senders put again after the entry TTL.

The log (stderr) has time, path, status, result counts and elapsed ms per
request, and counts per sweep. Never a queue id, key, signature, ciphertext,
size or IP.

## Trying it

```sh
cargo build --locked --release -p queue_server --bin queue_server --examples
QUEUE_LISTEN=127.0.0.1:18788 QUEUE_DB=/tmp/q.db target/release/queue_server &
target/release/examples/smoke 127.0.0.1:18788 before   # CREATE, SEND x2, RECV, ACK one
# stop and start the server on the same QUEUE_DB, then:
target/release/examples/smoke 127.0.0.1:18788 after    # RECV gives the unacked one
```

`examples/perf.rs` fills and measures a server started with
`QUEUE_TRUST_PROXY=1` (each batch comes from its own forwarded address):
`perf <addr> fill 10000 10 2048`, then `perf <addr> recv 10000 200 0`.
