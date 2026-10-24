#!/usr/bin/env python3
"""Reject controller code in every native member of a Release artifact."""
import pathlib
import sys
import zipfile

MARKER = b"chat-test-hooks:v1"
MARKERS = (MARKER, b"account_local_", b"message_local_", b"Local record fixture unavailable")
MACHO = {bytes.fromhex(value) for value in (
    "feedface", "cefaedfe", "feedfacf", "cffaedfe", "cafebabe", "bebafeca", "cafebabf", "bfbafeca"
)}


def scan(stream):
    tail = b""
    while block := stream.read(1024 * 1024):
        data = tail + block
        if any(marker in data for marker in MARKERS):
            raise ValueError("native artifact contains test controller marker")
        tail = data[-max(map(len, MARKERS)):]


def check(artifact):
    count = 0
    if artifact.is_file() and artifact.suffix.lower() == ".apk":
        # Read archive members in place. Never extract untrusted ZIP names.
        with zipfile.ZipFile(artifact) as archive:
            for member in archive.infolist():
                name = pathlib.PurePosixPath(member.filename)
                if name.is_absolute() or ".." in name.parts or "\\" in member.filename:
                    raise ValueError("unsafe archive member")
                if member.filename.lower().endswith(".so"):
                    with archive.open(member) as stream:
                        scan(stream)
                    count += 1
    elif artifact.is_dir():
        apple = artifact.suffix.lower() == ".app"
        files = sorted(artifact.rglob("*"))
        if not apple and not any(f.suffix.lower() == ".exe" for f in files):
            raise ValueError("unsupported artifact directory")
        core_names = {"chat_ffi", "libchat_ffi.dylib"} if apple else {"chat_ffi.dll"}
        if not any(f.is_file() and f.name.lower() in core_names for f in files):
            raise ValueError("native core library missing")
        libraries = 0
        for file in files:
            if file.is_symlink():
                # Do not silently scan outside the supplied artifact.
                if not file.resolve().is_relative_to(artifact.resolve()):
                    raise ValueError("native artifact contains external symlink")
            if not file.is_file():
                continue
            suffix = file.suffix.lower()
            with file.open("rb") as stream:
                native = apple and stream.read(4) in MACHO
                if native or suffix in (".so", ".dll", ".dylib", ".exe"):
                    stream.seek(0)
                    scan(stream)
                    count += 1
                    if apple or suffix == ".dll":
                        libraries += 1
        if not libraries:
            raise ValueError("no native libraries in artifact")
    else:
        raise ValueError("missing or unsupported artifact")
    if not count:
        raise ValueError("no native libraries in artifact")
    return count


def main():
    try:
        if len(sys.argv) != 2:
            raise ValueError("usage: check_release_hooks.py <APK|Windows-release-directory|app-directory>")
        count = check(pathlib.Path(sys.argv[1]))
        print(f"Release marker check PASS ({count} native files); runtime contract checked separately")
        return 0
    except (OSError, ValueError, zipfile.BadZipFile, RuntimeError) as error:
        print(f"Release marker check FAIL: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
