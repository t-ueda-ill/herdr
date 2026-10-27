"""Print `true` when two commits differ only in Herdr's own version.

CI uses this to skip the build and test jobs for release-preparation
changes, which touch nothing those jobs could break.
"""

from __future__ import annotations

import re
import subprocess
import sys

PACKAGE = "herdr"
VERSION_FILES = ("Cargo.toml", "Cargo.lock")
VERSION_LINE = re.compile(r'^version\s*=\s*"[^"]*"\s*$')
PLACEHOLDER = 'version = "<herdr>"'


def without_manifest_version(text: str) -> str:
    table = None
    lines = []
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            table = stripped
        elif table == "[package]" and VERSION_LINE.match(stripped):
            line = PLACEHOLDER
        lines.append(line)
    return "\n".join(lines)


def without_lock_version(text: str) -> str:
    blocks = text.split("[[package]]")
    for index, block in enumerate(blocks):
        lines = block.split("\n")
        if f'name = "{PACKAGE}"' not in lines:
            continue
        blocks[index] = "\n".join(
            PLACEHOLDER if VERSION_LINE.match(line) else line for line in lines
        )
    return "[[package]]".join(blocks)


NORMALIZERS = {
    "Cargo.toml": without_manifest_version,
    "Cargo.lock": without_lock_version,
}


def is_version_bump_only(changes: dict[str, tuple[str | None, str | None]]) -> bool:
    """`changes` maps each changed path to its text before and after; `None`
    when the file is absent on that side."""
    if not changes or not set(changes) <= set(VERSION_FILES):
        return False
    for path, (before, after) in changes.items():
        if before is None or after is None:
            return False
        normalize = NORMALIZERS[path]
        if normalize(before) != normalize(after):
            return False
    return True


def git(*args: str) -> str:
    return subprocess.run(
        ["git", *args], check=True, capture_output=True, text=True
    ).stdout


def file_at(commit: str, path: str) -> str | None:
    try:
        return git("show", f"{commit}:{path}")
    except subprocess.CalledProcessError:
        return None


def main(argv: list[str]) -> int:
    base, head = argv
    paths = git("diff", "--name-only", base, head).splitlines()
    changes = {path: (file_at(base, path), file_at(head, path)) for path in paths}
    print("true" if is_version_bump_only(changes) else "false")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
