#!/usr/bin/env bash
# push_proxy and queue_server setup for an Oracle Ubuntu 24.04 VM. The systemd units are the
# ones in server/push_proxy/README.md and server/queue_server/README.md, the Caddyfile the one in
# server/queue_server/README.md: change the README and this script together.
# Usage: sudo bash setup.sh <domain>
# First run needs in /tmp: push_proxy and queue_server (the CI binaries) and fcm-service-account.json.
# Safe to run again: it installs whichever binaries are in /tmp, keeps the existing handle key
# and never touches the queue database (/var/lib/queue_server).
set -euo pipefail

DOMAIN="${1:?usage: sudo bash setup.sh <domain>}"
[ "$(id -u)" -eq 0 ] || { echo "run with sudo"; exit 1; }
for bin in push_proxy queue_server; do
  [ -f "/tmp/$bin" ] || [ -x "/usr/local/bin/$bin" ] || { echo "/tmp/$bin missing"; exit 1; }
done

echo "== swap (1 GB RAM)"
if ! swapon --show | grep -q /swapfile; then
  fallocate -l 2G /swapfile
  chmod 600 /swapfile
  mkswap /swapfile >/dev/null
  swapon /swapfile
  grep -q '^/swapfile ' /etc/fstab || echo '/swapfile none swap sw 0 0' >> /etc/fstab
fi

echo "== packages"
export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y -q unattended-upgrades iptables-persistent curl gpg debian-keyring debian-archive-keyring apt-transport-https
if ! command -v caddy >/dev/null; then
  curl -1sLf https://dl.cloudsmith.io/public/caddy/stable/gpg.key | gpg --dearmor --yes -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg
  curl -1sLf https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt > /etc/apt/sources.list.d/caddy-stable.list
  chmod o+r /usr/share/keyrings/caddy-stable-archive-keyring.gpg /etc/apt/sources.list.d/caddy-stable.list
  apt-get update -q
  apt-get install -y -q caddy
fi

echo "== VM firewall: open 80 and 443 before the image's REJECT rule"
for port in 80 443; do
  if ! iptables -C INPUT -p tcp -m state --state NEW -m tcp --dport "$port" -j ACCEPT 2>/dev/null; then
    at=$(iptables -L INPUT --line-numbers -n | awk '$2 == "REJECT" {print $1; exit}')
    iptables -I INPUT "${at:-1}" -p tcp -m state --state NEW -m tcp --dport "$port" -j ACCEPT
  fi
done
netfilter-persistent save >/dev/null

echo "== user, binary, secrets"
id push-proxy >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin push-proxy
id queue-server >/dev/null 2>&1 || useradd --system --no-create-home --shell /usr/sbin/nologin queue-server
for bin in push_proxy queue_server; do
  if [ -f "/tmp/$bin" ]; then
    install -m 0755 "/tmp/$bin" "/usr/local/bin/$bin"
    rm -f "/tmp/$bin"
  fi
done
install -d -m 0750 -o root -g push-proxy /etc/push_proxy
if [ -f /tmp/fcm-service-account.json ]; then
  install -m 0640 -o root -g push-proxy /tmp/fcm-service-account.json /etc/push_proxy/fcm-service-account.json
  rm -f /tmp/fcm-service-account.json
fi
[ -f /etc/push_proxy/fcm-service-account.json ] || { echo "fcm-service-account.json missing: copy it to /tmp and run again"; exit 1; }
if [ ! -f /etc/push_proxy/env ]; then
  # A new handle key only on the first run: replacing it would orphan every handle already given out.
  umask 077
  cat > /etc/push_proxy/env <<EOF
PUSH_LISTEN=127.0.0.1:8787
PUSH_KEY=$(openssl rand -hex 32)
PUSH_KEY_ID=1
FCM_SERVICE_ACCOUNT_FILE=/etc/push_proxy/fcm-service-account.json
PUSH_TRUST_PROXY=1
EOF
  umask 022
fi
chmod 0600 /etc/push_proxy/env
chown root:root /etc/push_proxy/env

echo "== systemd"
cat > /etc/systemd/system/push_proxy.service <<'EOF'
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
EOF
cat > /etc/systemd/system/queue_server.service <<'EOF'
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
EOF
systemctl daemon-reload
for unit in push_proxy queue_server; do
  systemctl enable "$unit" >/dev/null
  systemctl restart "$unit"
done

echo "== Caddy: validate the new Caddyfile, then replace the old one"
new=$(mktemp /etc/caddy/Caddyfile.XXXXXX)
chmod 0644 "$new"
cat > "$new" <<EOF
$DOMAIN {
	handle /q/* {
		# 1 MiB is a full legal request (queue_wire::MAX_BODY), with room to spare: a smaller
		# bound would refuse a full SEND with 413.
		request_body {
			max_size 1152KiB
		}
		# The whole body is read here before the queue server is asked, so a slow upload holds
		# a Caddy connection, not one of the server's 128.
		reverse_proxy 127.0.0.1:8788 {
			request_buffers 1152KiB
		}
	}
	handle {
		reverse_proxy 127.0.0.1:8787
	}
}
EOF
if ! caddy validate --config "$new" --adapter caddyfile; then
  rm -f "$new"
  echo "new Caddyfile invalid: /etc/caddy/Caddyfile and the running Caddy are unchanged"
  exit 1
fi
old=$(mktemp /etc/caddy/Caddyfile.old.XXXXXX)
if [ -f /etc/caddy/Caddyfile ]; then cp -p /etc/caddy/Caddyfile "$old"; else rm -f "$old"; fi
mv "$new" /etc/caddy/Caddyfile
if ! systemctl reload-or-restart caddy; then
  if [ -f "$old" ]; then
    mv "$old" /etc/caddy/Caddyfile
    systemctl reload-or-restart caddy || true
    echo "Caddy did not take the new Caddyfile: the old one is back (journalctl -u caddy)"
  else
    echo "Caddy did not start with the new Caddyfile (journalctl -u caddy)"
  fi
  exit 1
fi
rm -f "$old"

echo "== check: each service active for 5 s"
down=""
for _ in 1 2 3 4 5; do
  sleep 1
  for unit in push_proxy queue_server caddy; do
    systemctl is-active --quiet "$unit" || down="$down $unit"
  done
done
journalctl -u push_proxy -n 5 --no-pager
journalctl -u queue_server -n 5 --no-pager
if [ -n "$down" ]; then
  echo "not active: $(printf '%s\n' $down | sort -u | xargs)"
  exit 1
fi
echo "HTTPS status from this server (any 4xx means TLS and the server answer; /q/recv is the queue server):"
for path in handle q/recv; do
  curl -sS -o /dev/null -w "/$path %{http_code}\n" --max-time 60 "https://$DOMAIN/$path" || echo "no answer yet: Caddy may still be getting the certificate (journalctl -u caddy)"
done
