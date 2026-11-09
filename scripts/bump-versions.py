#!/usr/bin/env python3
"""Moves rekcod's pinned versions to the newest releases.

  scripts/bump-versions.py                  # rewrite the pins, print what changed
  scripts/bump-versions.py --check          # print what would change, write nothing
  scripts/bump-versions.py --summary F.md   # also write the change list as Markdown (PR body)

Pins: guest/versions.env (Alpine minirootfs, Docker engine, Linux kernel, Alpine edge's
linux-virt for KERNEL=alpine) and crates/rekcod/src/cli_fetch.rs (Docker CLI, Compose and
Buildx for the Mac). Checksums come from each project's published sha256 where there is
one; Docker's static tarballs have none, so they're downloaded and hashed.

The kernel follows the newest stable series. It moves to a new series once that series has
its .2 point release, or right away when the current series is end of life. A new series
needs the fuse patch rebased by hand (guest/fuse/); guest/kernel/base.config is replaced
from Alpine edge's linux-virt when edge has the same series.
"""

import argparse
import gzip
import hashlib
import io
import json
import os
import re
import sys
import tarfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
VERSIONS = ROOT / "guest/versions.env"
CLI_FETCH = ROOT / "crates/rekcod/src/cli_fetch.rs"
BASE_CONFIG = ROOT / "guest/kernel/base.config"
EDGE_MAIN = "https://dl-cdn.alpinelinux.org/alpine/edge/main/aarch64/"
UA = {"User-Agent": "rekcod-bump"}


def get(url, binary=False):
    headers = dict(UA)
    if url.startswith("https://api.github.com/") and os.environ.get("GITHUB_TOKEN"):
        headers["Authorization"] = "Bearer " + os.environ["GITHUB_TOKEN"]
    with urllib.request.urlopen(urllib.request.Request(url, headers=headers), timeout=300) as r:
        body = r.read()
    return body if binary else body.decode()


def sha256_of(url):
    h = hashlib.sha256()
    with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=600) as r:
        while chunk := r.read(1 << 20):
            h.update(chunk)
    return h.hexdigest()


def ver(v):
    return tuple(int(x) for x in re.findall(r"\d+", v))


def series(v):
    return ver(v)[:2]


def pick_kernel(current, stable):
    """The kernel to pin, given kernel.org's stable releases as (version, is_eol)."""
    newest = max((v for v, _ in stable), key=ver)
    if series(newest) != series(current) and ver(newest)[2:3] >= (2,):
        return newest
    same = [(v, eol) for v, eol in stable if series(v) == series(current)]
    if not same or any(eol for _, eol in same):
        return newest
    return max([current] + [v for v, _ in same], key=ver)


def read_env(text):
    return dict(re.findall(r'^: "\$\{(\w+):=([^}]*)\}"', text, re.M))


def set_env(text, key, value):
    new, n = re.subn(rf'^(: "\$\{{{key}:=)[^}}]*(\}}")', rf"\g<1>{value}\g<2>", text, flags=re.M)
    if n != 1:
        raise SystemExit(f"{key} not found once in {VERSIONS}")
    return new


def minirootfs(yaml_text):
    """(version, sha256) of the aarch64 minirootfs in Alpine's latest-releases.yaml."""
    for block in yaml_text.split("\n-\n"):
        fields = dict(re.findall(r"^\s+(\w+): (.*)$", block, re.M))
        if fields.get("flavor") == "alpine-minirootfs":
            return fields["version"], fields["sha256"]
    raise SystemExit("no alpine-minirootfs in latest-releases.yaml")


def newest_docker():
    names = re.findall(r'href="docker-(\d+\.\d+\.\d+)\.tgz"', get("https://download.docker.com/linux/static/stable/aarch64/"))
    return max(names, key=ver)


def edge_linux_virt():
    """Alpine edge's linux-virt package version, e.g. 7.3.3-r0."""
    found = re.findall(r'href="linux-virt-(\d[\w.]*-r\d+)\.apk"', get(EDGE_MAIN))
    return max(found, key=ver) if found else None


def edge_config(pkg_version):
    """(name, text) of /boot/config-*-virt in Alpine edge's linux-virt apk (concatenated gzip tar segments)."""
    data = gzip.decompress(get(f"{EDGE_MAIN}linux-virt-{pkg_version}.apk", binary=True))
    with tarfile.open(fileobj=io.BytesIO(data), mode="r:", ignore_zeros=True) as tar:
        for m in tar:
            if m.isfile() and re.fullmatch(r"boot/config-[^/]+-virt", m.name):
                return m.name, tar.extractfile(m).read().decode()
    raise SystemExit(f"no kernel config in linux-virt-{pkg_version}.apk")


def latest_release(repo):
    r = json.loads(get(f"https://api.github.com/repos/{repo}/releases/latest"))
    return r["tag_name"], {a["name"]: a.get("digest", "").removeprefix("sha256:") for a in r["assets"]}


def replace_asset(text, url_re, url, sha):
    pattern = rf'url: "{url_re}",\n(\s+)sha256: "[0-9a-f]{{64}}"'
    new, n = re.subn(pattern, lambda m: f'url: "{url}",\n{m.group(1)}sha256: "{sha}"', text)
    if n != 1:
        raise SystemExit(f"asset {url_re} not found once in {CLI_FETCH}")
    return new


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--check", action="store_true", help="print what would change, write nothing")
    ap.add_argument("--summary", type=Path, help="write the change list as Markdown here")
    args = ap.parse_args()

    env_text = VERSIONS.read_text()
    env = read_env(env_text)
    changes, notes = [], []

    def bump(key, value, label=None):
        nonlocal env_text
        if env[key] != value:
            changes.append(f"{label or key}: {env[key]} -> {value}")
            env_text = set_env(env_text, key, value)

    # Alpine minirootfs
    alpine, alpine_sha = minirootfs(get("https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/aarch64/latest-releases.yaml"))
    if ver(alpine) > ver(env["ALPINE_RELEASE"]):
        bump("ALPINE_RELEASE", alpine, "Alpine")
        bump("MINIROOTFS_SHA256", alpine_sha)

    # Docker engine (and the Mac CLI below, same release)
    docker = newest_docker()
    if ver(docker) > ver(env["DOCKER_VERSION"]):
        bump("DOCKER_VERSION", docker, "Docker")
        bump("DOCKER_SHA256", sha256_of(f"https://download.docker.com/linux/static/stable/aarch64/docker-{docker}.tgz"))
    else:
        docker = env["DOCKER_VERSION"]

    # Kernel
    releases = json.loads(get("https://www.kernel.org/releases.json"))["releases"]
    stable = [(r["version"], r["iseol"]) for r in releases if r["moniker"] == "stable"]
    linux = pick_kernel(env["LINUX_VERSION"], stable)
    new_series = series(linux) != series(env["LINUX_VERSION"])
    if linux != env["LINUX_VERSION"]:
        major = ver(linux)[0]
        sums = get(f"https://cdn.kernel.org/pub/linux/kernel/v{major}.x/sha256sums.asc")
        m = re.search(rf"^([0-9a-f]{{64}})  linux-{re.escape(linux)}\.tar\.xz$", sums, re.M)
        if not m:
            raise SystemExit(f"no sha256 for linux-{linux}.tar.xz")
        bump("LINUX_VERSION", linux, "Linux")
        bump("LINUX_SHA256", m.group(1))
    if new_series:
        notes.append(
            f"New kernel series {linux}: rebase guest/fuse/rekcod-fuse.patch onto fs/fuse of linux-{linux} "
            "and run scripts/e2e.sh three times before merging."
        )
    virt = edge_linux_virt()
    if virt and virt.split("-r")[0] == linux:
        bump("LINUX_VIRT_VERSION", virt, "Alpine edge linux-virt (KERNEL=alpine)")
    elif virt:
        notes.append(f"Alpine edge has linux-virt {virt}, not {linux}: KERNEL=alpine builds fail until they match.")
    config = None
    if new_series:
        if virt and series(virt) == series(linux):
            config = edge_config(virt)
            changes.append(f"guest/kernel/base.config: from Alpine edge linux-virt {virt}")
        else:
            notes.append(f"guest/kernel/base.config is still for {env['LINUX_VERSION']}: Alpine edge has no {'.'.join(map(str, series(linux)))} linux-virt yet.")

    # Mac CLI, Compose, Buildx
    cli_text = CLI_FETCH.read_text()
    cli_version = re.search(r'pub const CLI_VERSION: &str = "([^"]+)";', cli_text).group(1)
    if ver(docker) > ver(cli_version):
        url = f"https://download.docker.com/mac/static/stable/aarch64/docker-{docker}.tgz"
        cli_text = cli_text.replace(f'CLI_VERSION: &str = "{cli_version}"', f'CLI_VERSION: &str = "{docker}"')
        cli_text = replace_asset(cli_text, r'https://download\.docker\.com/mac/static/stable/aarch64/docker-[^"]+\.tgz', url, sha256_of(url))
        changes.append(f"Docker CLI (Mac): {cli_version} -> {docker}")
    for repo, name_fmt in [("docker/compose", "docker-compose-darwin-aarch64"), ("docker/buildx", "buildx-{tag}.darwin-arm64")]:
        current = re.search(rf"https://github\.com/{repo}/releases/download/(v[^/]+)/", cli_text).group(1)
        tag, digests = latest_release(repo)
        if ver(tag) <= ver(current):
            continue
        asset = name_fmt.format(tag=tag)
        if not digests.get(asset):
            raise SystemExit(f"{repo} {tag} has no sha256 digest for {asset}")
        url = f"https://github.com/{repo}/releases/download/{tag}/{asset}"
        cli_text = replace_asset(cli_text, rf'https://github\.com/{re.escape(repo)}/releases/download/v[^"]+', url, digests[asset])
        changes.append(f"{repo.split('/')[1].capitalize()} (Mac): {current} -> {tag}")

    lines = [f"- {c}" for c in changes] or ["- nothing to update"]
    if notes:
        lines += ["", "Needs a person:"] + [f"- {n}" for n in notes]
    print("\n".join(lines))
    if args.summary:
        args.summary.write_text("\n".join(lines) + "\n")
    if not args.check and changes:
        VERSIONS.write_text(env_text)
        CLI_FETCH.write_text(cli_text)
        if config:
            name, text = config
            header = BASE_CONFIG.read_text().split("\n")[:3]
            header[0] = f"# Base kernel config: Alpine edge linux-virt {virt}, /{name}."
            BASE_CONFIG.write_text("\n".join(header) + "\n" + text)


if __name__ == "__main__":
    sys.exit(main())
