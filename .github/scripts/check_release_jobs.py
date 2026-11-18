"""Fails if a job in release.yml both reads the Apple signing secrets and runs a build tool.

A crate's build.rs or an npm install script runs with whatever its job holds, so the job
that unlocks the Developer ID keychain must not run cargo, npm or `package.sh build`.
  python3 .github/scripts/check_release_jobs.py [release.yml]
"""

import re
import sys
from pathlib import Path

BUILD = re.compile(r"\b(cargo|npm|npx|bun|bunx|rustup)\b|package\.sh(?! sign\b)|\./\.github/actions/rust\b")


def jobs(text):
    """{job name: its lines}, from the 2-space-indented keys under `jobs:`."""
    out, name, inside = {}, None, False
    for line in text.splitlines():
        if re.match(r"^jobs:\s*$", line):
            inside = True
            continue
        if inside and re.match(r"^\S", line):
            inside = False
        if not inside:
            continue
        m = re.match(r"^  ([\w-]+):\s*$", line)
        if m:
            name = m.group(1)
            out[name] = []
        elif name:
            out[name].append(line)
    return out


def mixed(text):
    """Names of jobs that reference secrets.APPLE_ and run a build tool (comments ignored)."""
    bad = []
    for name, lines in jobs(text).items():
        code = [re.sub(r"\s#.*$", "", ln) for ln in lines if not ln.lstrip().startswith("#")]
        body = "\n".join(code)
        if "secrets.APPLE_" in body and BUILD.search(body):
            bad.append(name)
    return bad


def main():
    path = Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).resolve().parent.parent / "workflows/release.yml")
    text = path.read_text()
    if not jobs(text):
        sys.exit(f"{path}: no jobs found")
    bad = mixed(text)
    if bad:
        sys.exit(f"{path}: jobs {', '.join(bad)} hold the Apple secrets and run a build tool")
    print(f"{path}: signing secrets and build tools are in separate jobs")


if __name__ == "__main__":
    main()
