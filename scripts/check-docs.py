"""Fails on documentation references that lead nowhere.

    python3 scripts/check-docs.py

Checks every tracked text file for:
  - `docs/NAME.md` paths that don't exist
  - `README.md#anchor`, the repository URL with `#anchor`, and `README, "Heading"` (also
    `README.md, "A" and "B"`, wrapped across comment lines) naming a README section that
    doesn't exist
  - relative links in Markdown files to files or README anchors that don't exist
and that SECURITY.md and CONTRIBUTING.md exist, and CHANGELOG.md has a section for
Cargo.toml's version (scripts/release-notes.py). Anchors are GitHub's: what the README's
headings turn into on github.com, which the website's rehype-slug matches.
"""

from __future__ import annotations

from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parent.parent
REPO_URL = "github.com/TimMikeladze/rekcod"

# Files whose doc paths are history, not pointers.
SKIP_FILES = {
    "docs/audit-2026-11-18.md",  # the audit quotes the deleted paths it found
    "website/src/lib/content.test.ts",  # routing fixtures
    "scripts/check-docs.py",  # its own examples
}
# (file, text) pairs allowed to name a missing doc.
ALLOWED = {
    # 0.3.0 wrote this marker into users' ~/.ssh/config; it has to keep matching.
    ("crates/rekcod/src/sshkey.rs", "docs/ssh-keys.md"),
}


def slug(heading: str, seen: dict[str, int]) -> str:
    """GitHub's anchor for a heading: inline markup dropped, lowercased, punctuation other
    than `-` and `_` removed, spaces to `-`, and `-1`, `-2` for repeats."""
    text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", heading)
    text = text.replace("`", "").strip().lower()
    text = re.sub(r"[^\w\- ]", "", text)
    base = text.replace(" ", "-")
    n = seen.get(base, 0)
    seen[base] = n + 1
    return base if n == 0 else f"{base}-{n}"


def headings(markdown: str) -> list[str]:
    out, fence = [], None
    for line in markdown.splitlines():
        m = re.match(r"^\s*(`{3,}|~{3,})", line)
        if m:
            if fence is None:
                fence = m.group(1)[0]
            elif m.group(1)[0] == fence:
                fence = None
            continue
        if fence is None:
            h = re.match(r"^#{1,6}\s+(.*?)\s*#*\s*$", line)
            if h:
                out.append(h.group(1))
    return out


def readme_sections() -> tuple[set[str], set[str]]:
    text = (ROOT / "README.md").read_text()
    names = headings(text)
    seen: dict[str, int] = {}
    anchors = {slug(h, seen) for h in names}
    return anchors, {h.replace("`", "") for h in names}


def tracked() -> list[str]:
    out = subprocess.run(["git", "ls-files", "-z"], cwd=ROOT, capture_output=True, check=True).stdout
    return [f for f in out.decode().split("\0") if f]


def text_of(path: Path) -> str | None:
    try:
        data = path.read_bytes()
    except OSError:
        return None
    if b"\0" in data[:8192]:
        return None
    try:
        return data.decode()
    except UnicodeDecodeError:
        return None


def unwrap_comments(text: str) -> str:
    """Joins comment continuation lines, so a quoted heading wrapped across `//!`, `///`,
    `//`, `#` or ` * ` lines reads as one line."""
    return re.sub(r"\n[ \t]*(?://[!/]?|#|\*)[ \t]?", " ", text)


def main() -> int:
    anchors, titles = readme_sections()
    files = tracked()
    present = set(files)
    errors: list[str] = []

    for name in ("SECURITY.md", "CONTRIBUTING.md", "CHANGELOG.md"):
        if name not in present:
            errors.append(f"{name} is missing")
    if "Limitations" not in titles:
        errors.append('README.md has no "Limitations" section')
    version = re.search(r'^version = "([^"]+)"', (ROOT / "Cargo.toml").read_text(), re.M)
    notes = subprocess.run(
        [sys.executable, str(ROOT / "scripts/release-notes.py"), version.group(1) if version else "?"],
        capture_output=True,
        text=True,
    )
    if notes.returncode != 0:
        errors.append(notes.stderr.strip())

    for f in files:
        if f in SKIP_FILES or f.startswith(("website/node_modules/", "dashboard/node_modules/")):
            continue
        text = text_of(ROOT / f)
        if text is None:
            continue
        for n, line in enumerate(text.splitlines(), 1):
            if "git show" in line:
                continue
            for m in re.finditer(r"(?<![\w/.-])docs/[A-Za-z0-9_.-]+\.md", line):
                if m.group(0) not in present and (f, m.group(0)) not in ALLOWED:
                    errors.append(f"{f}:{n}: {m.group(0)} doesn't exist")
            for m in re.finditer(r"(?:README\.md|" + re.escape(REPO_URL) + r"/?)#([A-Za-z0-9_-]+)", line):
                if m.group(1) not in anchors:
                    errors.append(f"{f}:{n}: README has no #{m.group(1)}")

        flat = unwrap_comments(text)
        for m in re.finditer(r'README(?:\.md)?,\s*((?:"[^"\n]+"(?:,\s*|\s+and\s+)?)+)', flat):
            for title in re.findall(r'"([^"\n]+)"', m.group(1)):
                if title.replace("`", "") not in titles:
                    errors.append(f'{f}: README has no section "{title}"')

        if f.endswith(".md"):
            base = Path(f).parent
            for m in re.finditer(r"\]\(([^)\s]+)\)", text):
                target = m.group(1)
                if re.match(r"^[a-z][a-z0-9+.-]*:", target) or target.startswith("//"):
                    continue
                path, _, frag = target.partition("#")
                resolved = (base / path).as_posix() if path else f
                resolved = str(Path(resolved)).replace("\\", "/")
                resolved = re.sub(r"(^|/)\./", r"\1", resolved)
                if path and not (ROOT / resolved).exists():
                    errors.append(f"{f}: link to {target}: {resolved} doesn't exist")
                elif frag and resolved == "README.md" and frag not in anchors:
                    errors.append(f"{f}: link to {target}: README has no #{frag}")

    vercel = ROOT / "website/astro.config.mjs"
    if vercel.exists():
        for m in re.finditer(r"'/#([A-Za-z0-9_-]+)'", vercel.read_text()):
            if m.group(1) not in anchors:
                errors.append(f"website/astro.config.mjs: redirect to #{m.group(1)}, which README.md doesn't have")

    for e in errors:
        print(e)
    print(f"{len(files)} files checked, {len(errors)} broken references", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
