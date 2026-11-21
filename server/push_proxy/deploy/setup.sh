#!/usr/bin/env bash
# push_proxy server setup for an Oracle Ubuntu 24.04 VM. The systemd unit and the Caddyfile
# are the ones in server/push_proxy/README.md: change both places together.
# Usage: sudo bash setup.sh <domain>
# Needs in /tmp: push_proxy (the CI binary). First run also needs fcm-service-account.json.
# Safe to run again: it installs a new /tmp/push_proxy and keeps the existing handle key.
set -euo pipefail

DOMAIN="${1:?usage: sudo bash setup.sh <domain>}"
[ "$(id -u)" -eq 0 ] || { echo "run with sudo"; exit 1; }
[ -f /tmp/push_proxy ] || { echo "/tmp/push_proxy missing"; exit 1; }

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
install -m 0755 /tmp/push_proxy /usr/local/bin/push_proxy
rm -f /tmp/push_proxy
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
systemctl daemon-reload
systemctl enable push_proxy >/dev/null
systemctl restart push_proxy

echo "== Caddy"
cat > /etc/caddy/Caddyfile <<EOF
$DOMAIN {
	reverse_proxy 127.0.0.1:8787
}
EOF
systemctl reload caddy || systemctl restart caddy

echo "== check"
sleep 3
systemctl is-active push_proxy caddy
journalctl -u push_proxy -n 5 --no-pager
echo "HTTPS status from this server (any 4xx means TLS and the proxy answer):"
curl -sS -o /dev/null -w '%{http_code}\n' --max-time 60 "https://$DOMAIN/handle" || echo "no answer yet: Caddy may still be getting the certificate (journalctl -u caddy)"
