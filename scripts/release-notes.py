"""Prints CHANGELOG.md's section for a version, the release notes release.yml publishes.

    python3 scripts/release-notes.py 0.3.0 > notes.md

Exits 1 when CHANGELOG.md has no `## [VERSION]` heading or the section under it is empty,
so a tag without an entry publishes nothing.
"""

from pathlib import Path
import re
import sys


def section(changelog: str, version: str) -> str:
    version = version.removeprefix("v")
    lines, out, inside = changelog.splitlines(), [], False
    for line in lines:
        if line.startswith("## "):
            if inside:
                break
            inside = re.match(rf"## \[{re.escape(version)}\](\s|$)", line) is not None
            continue
        # The link definitions at the bottom belong to no section.
        if inside and not re.match(r"\[[^\]]+\]: ", line):
            out.append(line)
    return "\n".join(out).strip()


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: release-notes.py VERSION", file=sys.stderr)
        return 2
    changelog = Path(__file__).resolve().parent.parent / "CHANGELOG.md"
    notes = section(changelog.read_text(), sys.argv[1])
    if not notes:
        print(f"CHANGELOG.md has no entry for {sys.argv[1]}; add a `## [{sys.argv[1].removeprefix('v')}]` section", file=sys.stderr)
        return 1
    print(notes)
    return 0


if __name__ == "__main__":
    sys.exit(main())
