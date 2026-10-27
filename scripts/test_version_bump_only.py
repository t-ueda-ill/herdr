from __future__ import annotations

import unittest

from scripts.version_bump_only import is_version_bump_only

MANIFEST = """[package]
name = "herdr"
version = "{version}"
edition = "2024"

[dependencies]
libc = {{ version = "{libc}" }}
"""

LOCK = """version = 4

[[package]]
name = "herdr"
version = "{version}"
dependencies = [
 "libc",
]

[[package]]
name = "libc"
version = "{libc}"
checksum = "{checksum}"
"""


def manifest(version: str = "0.9.508", libc: str = "0.2") -> str:
    return MANIFEST.format(version=version, libc=libc)


def lock(version: str = "0.9.508", libc: str = "0.2.170", checksum: str = "aa") -> str:
    return LOCK.format(version=version, libc=libc, checksum=checksum)


class VersionBumpOnlyTests(unittest.TestCase):
    def test_a_herdr_version_bump_in_both_files_is_version_only(self) -> None:
        self.assertTrue(
            is_version_bump_only(
                {
                    "Cargo.toml": (manifest(), manifest(version="0.9.509")),
                    "Cargo.lock": (lock(), lock(version="0.9.509")),
                }
            )
        )

    def test_a_dependency_version_change_is_not_version_only(self) -> None:
        self.assertFalse(
            is_version_bump_only(
                {"Cargo.toml": (manifest(), manifest(libc="0.3"))}
            )
        )
        self.assertFalse(
            is_version_bump_only(
                {"Cargo.lock": (lock(), lock(libc="0.2.171"))}
            )
        )
        self.assertFalse(
            is_version_bump_only(
                {"Cargo.lock": (lock(), lock(libc="0.2.171", checksum="bb"))}
            )
        )

    def test_any_other_changed_file_is_not_version_only(self) -> None:
        self.assertFalse(
            is_version_bump_only(
                {
                    "Cargo.toml": (manifest(), manifest(version="0.9.509")),
                    "src/main.rs": ("", "fn main() {}"),
                }
            )
        )

    def test_an_added_or_removed_file_is_not_version_only(self) -> None:
        self.assertFalse(is_version_bump_only({"Cargo.lock": (None, lock())}))
        self.assertFalse(is_version_bump_only({"Cargo.lock": (lock(), None)}))

    def test_no_change_is_not_version_only(self) -> None:
        self.assertFalse(is_version_bump_only({}))


if __name__ == "__main__":
    unittest.main()
