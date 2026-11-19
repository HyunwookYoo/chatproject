#!/usr/bin/env bash
# Checks the two App Store provisioning profiles and installs them for Xcode. Run by testflight.yml
# (step "Install provisioning profiles") from the repository root; test_testflight_profile.py runs it
# with fake `security` and PlistBuddy.
#
# Reads APP_PROFILE_B64 and NSE_PROFILE_B64 (the profile secrets, from the step env), APP_ID, NSE_ID,
# APP_GROUP, APP_PROFILE_NAME and NSE_PROFILE_NAME (workflow env), and RUNNER_TEMP, GITHUB_ENV, HOME.
# Writes APP_PROFILE_UUID, NSE_PROFILE_UUID and TEAM_ID to GITHUB_ENV. Prints one ::error:: line and
# exits 1 for the first thing that is wrong.
set -euo pipefail

legacy="$HOME/Library/MobileDevice/Provisioning Profiles"
xcode16="$HOME/Library/Developer/Xcode/UserData/Provisioning Profiles"
mkdir -p "$legacy" "$xcode16"
pb="${PLISTBUDDY:-/usr/libexec/PlistBuddy}"
# The team is set once, in the script that writes DEVELOPMENT_TEAM into the project.
expected_team="$(awk -F"'" '/^TEAM = /{print $2}' app/ios/scripts/add_notification_service.rb || true)"
[ -n "$expected_team" ] || { echo "::error::no TEAM = '...' line in app/ios/scripts/add_notification_service.rb"; exit 1; }

install_profile() { # label base64 bundle-id expected-name env-prefix
  local f="$RUNNER_TEMP/$1.mobileprovision" p="$RUNNER_TEMP/$1.plist"
  local uuid name team appid groups
  printf '%s' "$2" | base64 --decode > "$f" \
    || { echo "::error::secret APPSTORE_PROFILE_$5_BASE64 is not valid base64"; exit 1; }
  [ -s "$f" ] || { echo "::error::secret APPSTORE_PROFILE_$5_BASE64 is empty"; exit 1; }
  security cms -D -i "$f" > "$p" \
    || { echo "::error::secret APPSTORE_PROFILE_$5_BASE64 is not a .mobileprovision file"; exit 1; }
  uuid=$("$pb" -c 'Print :UUID' "$p")
  name=$("$pb" -c 'Print :Name' "$p")
  team=$("$pb" -c 'Print :TeamIdentifier:0' "$p")
  appid=$("$pb" -c 'Print :Entitlements:application-identifier' "$p")
  groups=$("$pb" -c 'Print :Entitlements:com.apple.security.application-groups' "$p" 2>/dev/null || true)
  [ "$team" = "$expected_team" ] || { echo "::error::$1 profile belongs to team $team, but TEAM in app/ios/scripts/add_notification_service.rb is $expected_team"; exit 1; }
  [ "$appid" = "$team.$3" ] || { echo "::error::$1 profile is for $appid, expected $team.$3"; exit 1; }
  [ "$name" = "$4" ] || { echo "::error::$1 profile is named '$name', expected '$4'"; exit 1; }
  if "$pb" -c 'Print :ProvisionedDevices' "$p" >/dev/null 2>&1; then
    echo "::error::$1 is not an App Store profile"; exit 1
  fi
  case "$groups" in
    *"$APP_GROUP"*) ;;
    *) echo "::error::$1 profile lacks $APP_GROUP; enable App Groups on the App ID, then regenerate it"; exit 1 ;;
  esac
  cp "$f" "$legacy/$uuid.mobileprovision"
  cp "$f" "$xcode16/$uuid.mobileprovision"
  echo "$5_PROFILE_UUID=$uuid" >> "$GITHUB_ENV"
  echo "TEAM_ID=$team" >> "$GITHUB_ENV"
}

install_profile app "$APP_PROFILE_B64" "$APP_ID" "$APP_PROFILE_NAME" APP
install_profile nse "$NSE_PROFILE_B64" "$NSE_ID" "$NSE_PROFILE_NAME" NSE
[ "$("$pb" -c 'Print :Entitlements:aps-environment' "$RUNNER_TEMP/app.plist")" = production ] \
  || { echo "::error::app profile has no production aps-environment"; exit 1; }
