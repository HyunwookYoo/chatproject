"""Verify the pinned SQLCipher archive metadata and actual native C source."""

import argparse
import hashlib
import pathlib
import re
import tomllib


PACKAGES = {
    "rusqlite": ("0.40.2", "23f2a97da3e3873c73cb2a2e71b35c40ff95e0b1eefa8d72d8499a6928c3b5b3"),
    "libsqlite3-sys": ("0.38.2", "f1d20bef17f513b9b3004532233187769cd072d790971f4e4da0e346eb6401e8"),
}
SOURCE_SHA256 = "ea0bf0b08f688ca5d9312b2e33e7f81b3f4ae54b5016fb062ae1f2632a30a1b9"
MACROS = {
    "SQLITE_VERSION_NUMBER": "3051003",
    "SQLITE_SOURCE_ID": '"2026-03-13 10:38:09 737ae4a34738ffa0c3ff7f9bb18df914dd1cad163f28fd6b6e114a344fe6alt1"',
    "CIPHER_VERSION_NUMBER": "4.14.0",
    "CIPHER_VERSION_BUILD": "community",
}


def check(manifest: pathlib.Path, source: pathlib.Path) -> list[str]:
    errors = []
    try:
        lock = tomllib.loads(manifest.read_text(encoding="utf-8"))
        native = source.read_bytes()
    except (OSError, UnicodeError, tomllib.TOMLDecodeError):
        return ["unable to read Cargo.lock or SQLCipher source"]
    for name, (version, checksum) in PACKAGES.items():
        packages = [p for p in lock.get("package", []) if p.get("name") == name]
        if len(packages) != 1 or any(
            p.get("version") != version
            or p.get("checksum") != checksum
            or p.get("source") != "registry+https://github.com/rust-lang/crates.io-index"
            for p in packages
        ):
            errors.append(f"{name}: expected exact pinned registry version/checksum")
    if hashlib.sha256(native).hexdigest() != SOURCE_SHA256:
        errors.append("SQLCipher C source SHA256 mismatch")
    text = native.decode("utf-8", errors="replace")
    for name, value in MACROS.items():
        values = re.findall(r"^#define\s+" + name + r"[ \t]+([^\r\n]+)", text, re.MULTILINE)
        if values != [value]:
            errors.append(f"{name}: unexpected native source macro")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--manifest", type=pathlib.Path, required=True)
    parser.add_argument("--source", type=pathlib.Path, required=True)
    args = parser.parse_args()
    errors = check(args.manifest, args.source)
    for error in errors:
        print(error)
    if not errors:
        print("SQLCipher pinned archive metadata, source SHA256, and macros verified")
    return int(bool(errors))


if __name__ == "__main__":
    raise SystemExit(main())
