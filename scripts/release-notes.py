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


def unwrap(markdown: str) -> str:
    """Joins hard-wrapped lines: a GitHub release body renders each newline as a line break.
    Code blocks, headings, list items and blank lines stay as they are."""
    out: list[str] = []
    fence = False
    joinable = False
    for line in markdown.splitlines():
        stripped = line.strip()
        if stripped.startswith("```"):
            fence = not fence
            out.append(line)
            joinable = False
            continue
        starts_block = not stripped or fence or re.match(r"(#{1,6} |[-*+] |\d+\. |\|)", stripped)
        if joinable and not starts_block:
            out[-1] += " " + stripped
            continue
        out.append(line)
        joinable = bool(stripped) and not fence and not stripped.startswith(("#", "|"))
    return "\n".join(out)


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: release-notes.py VERSION", file=sys.stderr)
        return 2
    changelog = Path(__file__).resolve().parent.parent / "CHANGELOG.md"
    notes = unwrap(section(changelog.read_text(), sys.argv[1]))
    if not notes:
        print(f"CHANGELOG.md has no entry for {sys.argv[1]}; add a `## [{sys.argv[1].removeprefix('v')}]` section", file=sys.stderr)
        return 1
    print(notes)
    return 0


if __name__ == "__main__":
    sys.exit(main())
