#!/usr/bin/env python3
"""Third-party license notices for what a release ships, as plain text on stdout.

  scripts/third-party-notices.py crates rekcod aarch64-apple-darwin
  scripts/third-party-notices.py crates rekcod-agent aarch64-unknown-linux-musl
  scripts/third-party-notices.py npm dashboard

crates: every crate the package links for that target (normal dependencies, as cargo
resolves them; build and dev dependencies aren't shipped). npm: the production dependency
tree of that directory (`npm ls --omit=dev`), which needs its node_modules. For each one:
name, version, declared license, and the text of its LICENSE, COPYING and NOTICE files.
scripts/package.sh writes these into the host and guest tarballs.
"""

import json
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FILES = re.compile(r"^(licen[cs]e|copying|notice|copyright|unlicense)([-._].*)?$", re.I)


def license_files(directory):
    d = Path(directory)
    return sorted((p for p in d.iterdir() if p.is_file() and FILES.match(p.name)), key=lambda p: p.name.lower())


def crates(package, target):
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", target],
            cwd=ROOT,
            check=True,
            capture_output=True,
        ).stdout
    )
    pkgs = {p["id"]: p for p in meta["packages"]}
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    roots = [p["id"] for p in meta["packages"] if p["name"] == package and p["source"] is None]
    if len(roots) != 1:
        raise SystemExit(f"no workspace package {package}")
    seen, todo = set(), list(roots)
    while todo:
        pid = todo.pop()
        if pid in seen:
            continue
        seen.add(pid)
        for dep in nodes[pid]["deps"]:
            # kind None is a normal dependency; filter-platform already dropped other targets.
            if any(k["kind"] is None for k in dep["dep_kinds"]):
                todo.append(dep["pkg"])
    out = []
    for pid in seen:
        p = pkgs[pid]
        if p["source"] is None:  # the workspace's own crates: rekcod's LICENSE
            continue
        out.append((p["name"], p["version"], p.get("license") or p.get("license_file") or "unknown", Path(p["manifest_path"]).parent))
    return sorted(out, key=lambda e: (e[0], e[1]))


def npm(directory):
    d = ROOT / directory
    r = subprocess.run(["npm", "ls", "--omit=dev", "--all", "--parseable"], cwd=d, capture_output=True, text=True)
    # npm ls exits 1 on extraneous or invalid packages; the list is still complete.
    paths = [Path(line) for line in r.stdout.splitlines() if line.strip()]
    out, seen = [], set()
    for path in paths:
        if path.resolve() == d.resolve():
            continue
        pj = json.loads((path / "package.json").read_text())
        key = (pj["name"], pj.get("version", "?"))
        if key in seen:
            continue
        seen.add(key)
        lic = pj.get("license")
        if isinstance(lic, dict):
            lic = lic.get("type")
        out.append((key[0], key[1], lic or "unknown", path))
    if not out:
        raise SystemExit(f"npm ls found nothing in {d}; run npm ci there first")
    return sorted(out, key=lambda e: (e[0], e[1]))


def render(entries, title):
    lines = [title, "=" * len(title), ""]
    for name, version, lic, directory in entries:
        head = f"{name} {version} ({lic})"
        lines += [head, "-" * len(head)]
        files = license_files(directory)
        if not files:
            lines.append("(no license file in the package)")
        for f in files:
            lines += [f"[{f.name}]", f.read_text(errors="replace").rstrip(), ""]
        lines.append("")
    return "\n".join(lines) + "\n"


def main():
    args = sys.argv[1:]
    if len(args) == 3 and args[0] == "crates":
        sys.stdout.write(render(crates(args[1], args[2]), f"Rust crates in {args[1]} ({args[2]})"))
    elif len(args) == 2 and args[0] == "npm":
        sys.stdout.write(render(npm(args[1]), f"npm packages in {args[1]} (production)"))
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main()
