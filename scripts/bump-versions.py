#!/usr/bin/env python3
"""Moves rekcod's pinned versions to the newest releases.

  scripts/bump-versions.py                  # rewrite the pins, print what changed
  scripts/bump-versions.py --check          # print what would change, write nothing
  scripts/bump-versions.py --summary F.md   # also write the change list as Markdown (PR body)

Pins: guest/versions.env (Alpine minirootfs, Docker engine, Linux kernel, Alpine edge's
linux-virt for KERNEL=alpine) and crates/rekcod/src/cli_fetch.rs (Docker CLI, Compose and
Buildx for the Mac). Checksums come from each project's published sha256 where there is
one; Docker's static tarballs have none, so they're downloaded and hashed.

Signatures: kernel.org's sha256sums.asc and the Alpine minirootfs are checked with gpgv
against the keys in guest/keys/ (kernel.org's checksum autosigner, B886 8C80 BA62 A1FF FAF5
FDA9 632D 3A06 589D A6B1; Alpine's Natanael Copa, 0482 D840 22F5 2DF1 C4E7 CD43 293A CD09
07D9 495A). Docker, Compose and Buildx publish no signature for these files. Every version,
tag and hash is checked against a strict pattern before it's written: build.sh sources
versions.env as shell.

The kernel follows the newest stable series. It moves to a new series once that series has
its .2 point release, or right away when the current series is end of life. A new series
needs the fuse patch rebased by hand (guest/fuse/); guest/kernel/base.config is replaced
from Alpine edge's linux-virt whenever its series isn't the pinned kernel's and edge has
that series.
"""

import argparse
import base64
import gzip
import hashlib
import io
import json
import os
import re
import subprocess
import sys
import tarfile
import tempfile
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
VERSIONS = ROOT / "guest/versions.env"
CLI_FETCH = ROOT / "crates/rekcod/src/cli_fetch.rs"
BASE_CONFIG = ROOT / "guest/kernel/base.config"
KEYS = ROOT / "guest/keys"
EDGE_MAIN = "https://dl-cdn.alpinelinux.org/alpine/edge/main/aarch64/"
UA = {"User-Agent": "rekcod-bump"}

# What each pin may look like. versions.env is sourced as shell and cli_fetch.rs is Rust
# source, so nothing else gets written.
VERSION = r"\d+(\.\d+){1,2}"
PATTERNS = {
    "ALPINE_RELEASE": VERSION,
    "DOCKER_VERSION": VERSION,
    "LINUX_VERSION": VERSION,
    "LINUX_VIRT_VERSION": r"\d+(\.\d+)*-r\d+",
}
SHA256 = r"[0-9a-f]{64}"
TAG = r"v\d+\.\d+\.\d+"
URL = r"https://[A-Za-z0-9._/-]+"


def checked(kind, value, pattern):
    if not isinstance(value, str) or not re.fullmatch(pattern, value):
        raise SystemExit(f"refusing {kind} {value!r}: doesn't match {pattern}")
    return value


def get(url, binary=False):
    req = urllib.request.Request(url, headers=UA)
    if url.startswith("https://api.github.com/") and os.environ.get("GITHUB_TOKEN"):
        # Unredirected: urllib copies ordinary headers to wherever a redirect points.
        req.add_unredirected_header("Authorization", "Bearer " + os.environ["GITHUB_TOKEN"])
    with urllib.request.urlopen(req, timeout=300) as r:
        body = r.read()
    return body if binary else body.decode()


def dearmor(text):
    """The binary packets of an ASCII-armored OpenPGP key block (gpgv wants binary)."""
    lines = text.strip().splitlines()
    start = lines.index("-----BEGIN PGP PUBLIC KEY BLOCK-----")
    end = lines.index("-----END PGP PUBLIC KEY BLOCK-----")
    body = lines[start + 1 : end]
    body = body[body.index("") + 1 :] if "" in body else body
    if body and body[-1].startswith("="):
        body = body[:-1]
    return base64.b64decode("".join(body))


def gpgv(key, signed, sig=None):
    """Verifies `signed`: a clearsigned text, or the data for the detached `sig`.

    For a clearsigned text, returns what gpgv says was signed, so nothing outside the
    signature is ever read.
    """
    with tempfile.TemporaryDirectory() as d:
        ring, data = Path(d, "ring.gpg"), Path(d, "data")
        ring.write_bytes(dearmor((KEYS / key).read_text()))
        data.write_bytes(signed if isinstance(signed, bytes) else signed.encode())
        args = ["gpgv", "--homedir", d, "--keyring", str(ring)]
        if sig is None:
            args += ["--output", "-"]
        else:
            Path(d, "sig").write_bytes(sig)
            args.append(str(Path(d, "sig")))
        r = subprocess.run(args + [str(data)], capture_output=True)
        if r.returncode != 0:
            raise SystemExit(f"bad signature (key {key}):\n{r.stderr.decode(errors='replace')}")
        return r.stdout


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


def candidates(releases, current):
    """(version, is_eol) of kernel.org's stable releases, plus the pinned series once it's longterm."""
    return [
        (r["version"], r["iseol"])
        for r in releases
        if r["moniker"] == "stable" or (r["moniker"] == "longterm" and series(r["version"]) == series(current))
    ]


def pick_kernel(current, stable):
    """The kernel to pin, given candidates() as (version, is_eol). Never older than current."""
    if not stable:
        return current
    newest = max((v for v, _ in stable), key=ver)
    if series(newest) != series(current) and ver(newest)[2:3] >= (2,):
        pick = newest
    else:
        same = [(v, eol) for v, eol in stable if series(v) == series(current)]
        if not same or any(eol for _, eol in same):
            pick = newest
        else:
            pick = max([current] + [v for v, _ in same], key=ver)
    return max(current, pick, key=ver)


def config_version(base_config):
    """The linux-virt version line 1 of guest/kernel/base.config names, e.g. 7.3.3-r0."""
    m = re.match(r"# Base kernel config: Alpine edge linux-virt (\S+),", base_config)
    return m.group(1) if m else None


def config_stale(base_config, linux):
    """base.config is from another kernel series than the pinned one."""
    v = config_version(base_config)
    return v is None or series(v) != series(linux)


def read_env(text):
    return dict(re.findall(r'^: "\$\{(\w+):=([^}]*)\}"', text, re.M))


def set_env(text, key, value):
    checked(key, value, SHA256 if key.endswith("_SHA256") else PATTERNS[key])
    new, n = re.subn(rf'^(: "\$\{{{key}:=)[^}}]*(\}}")', lambda m: m.group(1) + value + m.group(2), text, flags=re.M)
    if n != 1:
        raise SystemExit(f"{key} not found once in {VERSIONS}")
    return new


def minirootfs(yaml_text):
    """(version, sha256) of the aarch64 minirootfs in Alpine's latest-releases.yaml."""
    for block in yaml_text.split("\n-\n"):
        fields = dict(re.findall(r"^\s+(\w+): (.*)$", block, re.M))
        if fields.get("flavor") == "alpine-minirootfs":
            return checked("Alpine version", fields.get("version"), VERSION), checked("minirootfs sha256", fields.get("sha256"), SHA256)
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
    checked("asset url", url, URL)
    checked("asset sha256", sha, SHA256)
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

    # Alpine minirootfs: the yaml names it, Alpine's release key vouches for the file.
    alpine, alpine_sha = minirootfs(get("https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/aarch64/latest-releases.yaml"))
    if ver(alpine) > ver(env["ALPINE_RELEASE"]):
        mini = f"https://dl-cdn.alpinelinux.org/alpine/v{'.'.join(alpine.split('.')[:2])}/releases/aarch64/alpine-minirootfs-{alpine}-aarch64.tar.gz"
        tarball = get(mini, binary=True)
        gpgv("alpine-ncopa.asc", tarball, sig=get(mini + ".asc", binary=True))
        if hashlib.sha256(tarball).hexdigest() != alpine_sha:
            raise SystemExit(f"minirootfs {alpine}: the signed file's sha256 isn't the one latest-releases.yaml gives")
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
    linux = checked("kernel version", pick_kernel(env["LINUX_VERSION"], candidates(releases, env["LINUX_VERSION"])), VERSION)
    new_series = series(linux) != series(env["LINUX_VERSION"])
    if linux != env["LINUX_VERSION"]:
        major = ver(linux)[0]
        sums = gpgv("kernel.org-autosigner.asc", get(f"https://cdn.kernel.org/pub/linux/kernel/v{major}.x/sha256sums.asc")).decode()
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
    # From the config's own header, not the old pin: a series that moved before edge had
    # its linux-virt still gets the config on a later run.
    config = None
    base_text = BASE_CONFIG.read_text()
    if config_stale(base_text, linux):
        if virt and series(virt) == series(linux):
            config = edge_config(checked("linux-virt version", virt, PATTERNS["LINUX_VIRT_VERSION"]))
            changes.append(f"guest/kernel/base.config: from Alpine edge linux-virt {virt}")
        else:
            notes.append(f"guest/kernel/base.config is still for {config_version(base_text)}: Alpine edge has no {'.'.join(map(str, series(linux)))} linux-virt yet.")

    # Mac CLI, Compose, Buildx
    cli_text = CLI_FETCH.read_text()
    cli_version = re.search(r'pub const CLI_VERSION: &str = "([^"]+)";', cli_text).group(1)
    if ver(docker) > ver(cli_version):
        checked("Docker version", docker, VERSION)
        url = f"https://download.docker.com/mac/static/stable/aarch64/docker-{docker}.tgz"
        cli_text = cli_text.replace(f'CLI_VERSION: &str = "{cli_version}"', f'CLI_VERSION: &str = "{docker}"')
        cli_text = replace_asset(cli_text, r'https://download\.docker\.com/mac/static/stable/aarch64/docker-[^"]+\.tgz', url, sha256_of(url))
        changes.append(f"Docker CLI (Mac): {cli_version} -> {docker}")
    for repo, name_fmt in [("docker/compose", "docker-compose-darwin-aarch64"), ("docker/buildx", "buildx-{tag}.darwin-arm64")]:
        current = re.search(rf"https://github\.com/{repo}/releases/download/(v[^/]+)/", cli_text).group(1)
        tag, digests = latest_release(repo)
        checked(f"{repo} tag", tag, TAG)
        if ver(tag) <= ver(current):
            continue
        asset = name_fmt.format(tag=tag)
        if not digests.get(asset):
            raise SystemExit(f"{repo} {tag} has no sha256 digest for {asset}")
        url = f"https://github.com/{repo}/releases/download/{tag}/{asset}"
        cli_text = replace_asset(cli_text, rf'https://github\.com/{re.escape(repo)}/releases/download/v[^"]+', url, digests[asset])
        changes.append(f"{repo.split('/')[1].capitalize()} (Mac): {current} -> {tag}")

    lines = [f"- {c}" for c in changes] or ["- nothing to update"]
    if any(c.startswith(("Docker", "Compose", "Buildx")) for c in changes):
        notes.append("Docker, Compose and Buildx publish no signature: their hashes are what download.docker.com and GitHub served this run.")
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
            header = base_text.split("\n")[:3]
            header[0] = f"# Base kernel config: Alpine edge linux-virt {virt}, /{name}."
            BASE_CONFIG.write_text("\n".join(header) + "\n" + text)


if __name__ == "__main__":
    sys.exit(main())
