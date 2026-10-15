#!/usr/bin/env bash
# Regenerates every committed binding from the Rust sources:
#   flutter_rust_bridge: app/lib/src/rust/**, app/rust/src/frb_generated.rs (+ freezed)
#   UniFFI Kotlin (chat_ffi): app/android/app/src/main/kotlin/uniffi/chat_ffi/chat_ffi.kt
#   UniFFI Swift (chat_nse):  app/ios/NotificationService/Generated/
# The `bindings` CI job runs this and fails if anything differs from the commit.
set -euo pipefail
cd "$(dirname "$0")/.."

case "$(uname -s)" in
  MINGW* | MSYS* | CYGWIN*) ffi_name=chat_ffi.dll nse_name=chat_nse.lib ;;
  Darwin) ffi_name=libchat_ffi.dylib nse_name=libchat_nse.a ;;
  *) ffi_name=libchat_ffi.so nse_name=libchat_nse.a ;;
esac

version="$(flutter_rust_bridge_codegen --version)"
if [[ "$version" != "flutter_rust_bridge_codegen 2.11.1" ]]; then
  echo "flutter_rust_bridge_codegen 2.11.1 is required, found: $version" >&2
  exit 1
fi
target_dir="$(cargo metadata --no-deps --format-version 1 | python -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
ffi_lib="$target_dir/debug/$ffi_name"
nse_lib="$target_dir/debug/$nse_name"

(cd app && flutter pub get && flutter_rust_bridge_codegen generate && \
  dart run build_runner build --define freezed:freezed=format=true)
cargo build -p chat_ffi -p chat_nse
cargo run -q -p uniffi-bindgen -- generate --library "$ffi_lib" --language kotlin \
  --out-dir app/android/app/src/main/kotlin --no-format
cargo run -q -p uniffi-bindgen -- generate --library "$nse_lib" --language swift \
  --out-dir app/ios/NotificationService/Generated --no-format
