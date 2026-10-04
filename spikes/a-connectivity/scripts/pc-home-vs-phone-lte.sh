#!/usr/bin/env bash
# Spike A1: home-line PC <-> KT mobile-data phone, both directions, relay-only dials.
export MSYS_NO_PATHCONV=1
ADB=/c/Users/user/AppData/Local/Android/Sdk/platform-tools/adb.exe
P=PHONESERIAL
BIN=/data/local/tmp/iroh-probe
PCBIN=/c/ChatProject/spikes/a-connectivity/target/x86_64-pc-windows-msvc/release/iroh-probe.exe
OUT=/c/ChatProject/spikes/a-connectivity/results/$(date +%Y-%m-%d)-kt
RUNS=${RUNS:-5}
ts() { date '+%H:%M:%S'; }

$ADB -s $P get-state 2>/dev/null | grep -q device || { echo "ABORT: phone $P not connected"; exit 1; }
mkdir -p $OUT && cd $OUT || exit 1
$ADB -s $P push C:/ChatProject/spikes/a-connectivity/target/aarch64-linux-android/release/iroh-probe $BIN >/dev/null && $ADB -s $P shell chmod 755 $BIN
WIFI_BEFORE=$($ADB -s $P shell settings get global wifi_on | tr -d '\r')
echo "wifi_on before: $WIFI_BEFORE"
PCPID=""
cleanup() {
  [ -n "$PCPID" ] && kill $PCPID 2>/dev/null
  $ADB -s $P shell pkill -f iroh-probe 2>/dev/null
  if [ "$WIFI_BEFORE" != "0" ]; then $ADB -s $P shell cmd wifi set-wifi-enabled enabled && echo "wifi restored"; fi
}
trap cleanup EXIT
$ADB -s $P shell cmd wifi set-wifi-enabled disabled
$ADB -s $P shell svc data enable
sleep 8
echo "network type: $($ADB -s $P shell getprop gsm.network.type | tr -d '\r')"
echo "ipv4 ifaces: $($ADB -s $P shell ip -4 -o addr | awk '{print $2}' | tr -d '\r' | sort -u | tr '\n' ' ')"
echo "ipv6 global: $($ADB -s $P shell ip -6 -o addr show scope global | awk '{print $2}' | tr -d '\r' | sort -u | tr '\n' ' ')"
$ADB -s $P shell ping -c 1 -W 3 1.1.1.1 >/dev/null 2>&1 && echo "phone online via mobile data" || echo "WARN: phone offline"

echo "##### dir1: PC (home) listens, phone (mobile) dials  $(ts)"
$PCBIN listen > pc-listen.out 2> pc-listen.err &
PCPID=$!
for i in $(seq 1 30); do grep -q '^endpoint' pc-listen.out 2>/dev/null && break; sleep 1; done
T1=$(grep -m1 '^endpoint' pc-listen.out)
[ -z "$T1" ] && { echo "no PC ticket"; cat pc-listen.err; }
for r in $(seq 1 $RUNS); do
  $ADB -s $P shell $BIN dial "$T1" --relay-only --count 20 > d1-relayonly-$r.json 2> d1-relayonly-$r.err
  sleep 2
done
$ADB -s $P shell $BIN dial "$T1" --count 20 > d1-fullticket-1.json 2> d1-fullticket-1.err
kill $PCPID 2>/dev/null; PCPID=""

echo "##### dir2: phone (mobile) listens, PC (home) dials  $(ts)"
$ADB -s $P shell $BIN listen > phone-listen.out 2> phone-listen.err &
for i in $(seq 1 30); do grep -q '^endpoint' phone-listen.out 2>/dev/null && break; sleep 1; done
T2=$(grep -m1 '^endpoint' phone-listen.out | tr -d '\r')
[ -z "$T2" ] && { echo "no phone ticket"; cat phone-listen.err; }
for r in $(seq 1 $RUNS); do
  $PCBIN dial "$T2" --relay-only --count 20 > d2-relayonly-$r.json 2> d2-relayonly-$r.err
  sleep 2
done
$PCBIN dial "$T2" --count 20 > d2-fullticket-1.json 2> d2-fullticket-1.err
echo "##### done  $(ts)"
