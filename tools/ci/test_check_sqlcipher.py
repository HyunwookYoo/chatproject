import pathlib
import tempfile
import unittest
from unittest.mock import patch

from check_sqlcipher import check


ROOT = pathlib.Path(__file__).resolve().parents[2]
SOURCE_FIXTURE = b'''#define SQLITE_VERSION_NUMBER 3051003
#define SQLITE_SOURCE_ID "2026-03-13 10:38:09 737ae4a34738ffa0c3ff7f9bb18df914dd1cad163f28fd6b6e114a344fe6alt1"
#define CIPHER_VERSION_NUMBER 4.14.0
#define CIPHER_VERSION_BUILD community
'''


class SqlcipherCheckTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.manifest = pathlib.Path(self.temp.name) / "Cargo.lock"
        self.source = pathlib.Path(self.temp.name) / "sqlite3.c"
        self.lock = (ROOT / "Cargo.lock").read_text(encoding="utf-8")
        self.c = SOURCE_FIXTURE
        # Unit tests exercise the real checker against a small controlled file.
        # Rust CI separately verifies the fetched native C with the production hash.
        self.enterContext(patch("check_sqlcipher.SOURCE_SHA256", "b62f2380437f6322c12fc312300a51d3eba900879604cb5602b6b41518723178"))
        self.manifest.write_text(self.lock, encoding="utf-8")
        self.source.write_bytes(self.c)

    def test_accepts_pinned_lock_and_source_fixture(self):
        self.assertEqual(check(self.manifest, self.source), [])

    def test_rejects_wrong_rusqlite_version(self):
        self.manifest.write_text(self.lock.replace('name = "rusqlite"\nversion = "0.40.2"', 'name = "rusqlite"\nversion = "0.40.1"'), encoding="utf-8")
        self.assertTrue(any("rusqlite" in error for error in check(self.manifest, self.source)))

    def test_rejects_wrong_native_checksum(self):
        self.manifest.write_text(self.lock.replace("f1d20bef17f513b9b3004532233187769cd072d790971f4e4da0e346eb6401e8", "0" * 64), encoding="utf-8")
        self.assertTrue(any("libsqlite3-sys" in error for error in check(self.manifest, self.source)))

    def test_rejects_duplicate_native_package(self):
        self.manifest.write_text(self.lock + '\n[[package]]\nname="libsqlite3-sys"\nversion="0.37.0"\n', encoding="utf-8")
        self.assertTrue(any("libsqlite3-sys" in error for error in check(self.manifest, self.source)))

    def test_rejects_source_mutation(self):
        self.source.write_bytes(self.c + b"\n/* modified */\n")
        self.assertTrue(any("SHA256" in error for error in check(self.manifest, self.source)))

    def test_rejects_wrong_sqlite_macro(self):
        self.source.write_bytes(self.c.replace(b"#define SQLITE_VERSION_NUMBER 3051003", b"#define SQLITE_VERSION_NUMBER 3051002"))
        self.assertTrue(any("SQLITE_VERSION_NUMBER" in error for error in check(self.manifest, self.source)))

    def test_rejects_wrong_cipher_macro(self):
        self.source.write_bytes(self.c.replace(b"#define CIPHER_VERSION_NUMBER 4.14.0", b"#define CIPHER_VERSION_NUMBER 4.13.0"))
        self.assertTrue(any("CIPHER_VERSION" in error for error in check(self.manifest, self.source)))

    def test_rejects_missing_inputs(self):
        self.source.unlink()
        self.assertTrue(check(self.manifest, self.source))


if __name__ == "__main__":
    unittest.main()
