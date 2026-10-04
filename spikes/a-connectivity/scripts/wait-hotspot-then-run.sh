#!/usr/bin/env bash
# Waits until the PC's Ethernet is down and Wi-Fi is up with internet, then runs the LTE<->LTE probe.
D="$(cd "$(dirname "$0")" && pwd)"
end=$(( $(date +%s) + 1800 ))
while [ $(date +%s) -lt $end ]; do
  st=$(powershell.exe -NoProfile -Command "(Get-NetAdapter -Name Ethernet).Status; (Get-NetAdapter -Name Wi-Fi).Status" | tr -d '\r' | tr '\n' ' ')
  eth=${st%% *}; wifi=$(echo $st | cut -d" " -f2)
  if [ "$eth" != "Up" ] && [ "$wifi" = "Up" ]; then if ping -n 1 -w 2000 1.1.1.1 >/dev/null 2>&1; then
        echo "hotspot ready at $(date +%H:%M:%S): $st"
        netsh wlan show interfaces | grep -E "^\s+(SSID|Radio type|Signal)" | sed 's/^ *//'
        sleep 3
        bash "$D/pc-hotspot-vs-phone-lte.sh"
        echo "A2 finished at $(date +%H:%M:%S) — reconnect Ethernet"
        exit 0
      fi
  fi
  sleep 5
done
echo "timeout waiting for hotspot"
