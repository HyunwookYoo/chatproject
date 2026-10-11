#!/usr/bin/env bash
# Looks inside a built Runner.app and prints one ::error:: line for the first thing that is wrong.
#
#   bash tools/ci/check_ios_app.sh [--signed] <path-to-Runner.app>
#
# ci.yml runs it on the simulator Debug build (job ios) and on the unsigned device Release build
# (job ios-release). testflight.yml runs it with --signed on the app inside the exported IPA, before
# the upload. It prints bundle ids, versions, library paths and the values of a few entitlements,
# nothing else.
#
# The defaults match app/ios/scripts/add_notification_service.rb; testflight.yml exports the same names.
set -euo pipefail

signed=false
if [ "${1:-}" = "--signed" ]; then
  signed=true
  shift
fi
if [ "$#" -ne 1 ]; then
  echo "usage: check_ios_app.sh [--signed] <path-to-Runner.app>" >&2
  exit 2
fi
app="$1"

NSE_ID="${NSE_ID:-dev.chatproject.chatapp.NotificationService}"
APP_GROUP="${APP_GROUP:-group.dev.chatproject.chatapp}"
pb="${PLISTBUDDY:-/usr/libexec/PlistBuddy}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

fail() {
  echo "::error::$*"
  exit 1
}

# The names inside a directory, for the error messages.
names() {
  find "$1" -mindepth 1 -maxdepth 1 2> /dev/null | sed 's|.*/||' | tr '\n' ' '
}

[ -d "$app" ] || fail "no app bundle at $app"
appex="$app/PlugIns/NotificationService.appex"
[ -d "$appex" ] || fail "NotificationService.appex is not embedded in $app (PlugIns holds: $(names "$app/PlugIns"))"
got="$("$pb" -c 'Print :CFBundleIdentifier' "$appex/Info.plist")" \
  || fail "cannot read CFBundleIdentifier from the extension's Info.plist"
[ "$got" = "$NSE_ID" ] || fail "the embedded extension is '$got', expected '$NSE_ID'"
echo "extension: $got"
[ -f "$appex/nse_state.bin" ] \
  || fail "nse_state.bin is not in the extension (the NotificationService target carries the 200-leaf fixture as a resource)"

# App Store Connect rejects an extension whose version differs from the app's, but only after the whole
# signed build. Both must come from FLUTTER_BUILD_NAME/NUMBER (Ruling R14).
for key in CFBundleVersion CFBundleShortVersionString; do
  app_value="$("$pb" -c "Print :$key" "$app/Info.plist")" \
    || fail "Runner.app Info.plist has no $key"
  ext_value="$("$pb" -c "Print :$key" "$appex/Info.plist")" \
    || fail "NotificationService.appex Info.plist has no $key"
  [ "$app_value" = "$ext_value" ] \
    || fail "$key differs: Runner.app has '$app_value', NotificationService.appex has '$ext_value'. App Store Connect would reject this build; both Info.plists must use FLUTTER_BUILD_NAME and FLUTTER_BUILD_NUMBER."
  echo "$key: $app_value (Runner.app and extension)"
done

# The extension runs in a ~15 MB process, so it must link system libraries only (decision D1).
# The first line of otool's output is the binary's own path.
exe="$appex/NotificationService"
[ -f "$exe" ] || fail "NotificationService.appex has no executable"
otool -L "$exe" > "$work/otool.txt" || fail "otool cannot read the extension binary"
linked="$(tail -n +2 "$work/otool.txt")"
echo "$linked"
bad="$(printf '%s\n' "$linked" | grep -E 'chat_ffi|Flutter|@rpath' || true)"
if [ -n "$bad" ]; then
  printf 'The extension links these, which are not system libraries:\n%s\n' "$bad"
  fail "NotificationService links chat_ffi, Flutter or an @rpath library (listed above). That adds a second Rust runtime to the 15 MB extension. Its base configuration must be Flutter/Generated.xcconfig, set in add_notification_service.rb."
fi

# Write nm's output to a file first: with `grep -q` at the end of a pipe, grep stops reading at the
# first match, nm can die from SIGPIPE, and pipefail would then fail a good build.
ffi="$app/Frameworks/chat_ffi.framework/chat_ffi"
nm -gU "$ffi" > "$work/chat_ffi.syms" \
  || fail "cannot read the symbols of chat_ffi.framework (Frameworks holds: $(names "$app/Frameworks"))"
grep -q frb_get_rust_content_hash "$work/chat_ffi.syms" \
  || fail "chat_ffi lost its flutter_rust_bridge symbols in this build"
echo "chat_ffi exports frb_get_rust_content_hash"

if [ "$signed" = true ]; then
  read_entitlements() { # name bundle
    codesign -d --entitlements :- "$2" > "$work/$1.plist" 2> /dev/null \
      || fail "cannot read the signed entitlements of $1 (is it signed?)"
    [ -s "$work/$1.plist" ] || fail "$1 is signed without entitlements"
  }
  has_group() { # entitlements-plist
    local i=0 value
    while value="$("$pb" -c "Print :com.apple.security.application-groups:$i" "$1" 2> /dev/null)"; do
      [ "$value" = "$APP_GROUP" ] && return 0
      i=$((i + 1))
    done
    return 1
  }
  read_entitlements Runner.app "$app"
  read_entitlements NotificationService.appex "$appex"
  aps="$("$pb" -c 'Print :aps-environment' "$work/Runner.app.plist" 2> /dev/null || true)"
  echo "Runner.app aps-environment: ${aps:-<missing>}"
  [ "$aps" = production ] \
    || fail "Runner.app is signed with aps-environment '${aps:-<missing>}', not production, so TestFlight builds would get no pushes. Runner.entitlements says development; Xcode should replace it when it signs with the App Store profile."
  for name in Runner.app NotificationService.appex; do
    has_group "$work/$name.plist" \
      || fail "$name is not signed into the App Group $APP_GROUP (its App ID needs App Groups with this group, then regenerate the profile)"
    echo "$name App Group: $APP_GROUP"
  done
fi

echo "$app: all checks passed"
