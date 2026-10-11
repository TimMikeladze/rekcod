#!/usr/bin/env python3
"""fsx-style coherence stress: random file ops from the Mac and from a container
on one bind-mounted dir. After every op the container's view (names, types,
content hashes) must match the Mac's within --deadline seconds (rekcod's
contract is bounded staleness, ~25 ms). Every check also re-warms the guest's
caches, so each next op hits cached dentries/attrs/pages.

  python3 tests/stress/fsx.py [--context rekcod] [--ops 300] [--seed N]
"""
import argparse
import hashlib
import os
import random
import shutil
import subprocess
import sys
import tempfile
import time

NAMES = ["a", "b", "c", "Doc", "doc2", "x.txt", "y.json"]
DIRS = ["", "d1", "d1/sub", "D2"]

# Prints "<type> <md5|-> <path>" for everything under /c, sorted.
SNAPSHOT = r"""cd /c && find . -mindepth 1 | sort | while IFS= read -r p; do
if [ -d "$p" ]; then echo "d - $p"; else echo "f $(md5sum <"$p" | cut -c1-32) $p"; fi; done"""


def mac_snapshot(root):
    out = []
    for dirpath, dirs, files in os.walk(root):
        for d in dirs:
            out.append("d - ./" + os.path.relpath(os.path.join(dirpath, d), root))
        for f in files:
            p = os.path.join(dirpath, f)
            with open(p, "rb") as fh:
                out.append("f %s ./%s" % (hashlib.md5(fh.read()).hexdigest(), os.path.relpath(p, root)))
    return sorted(out, key=lambda l: l.split(" ", 2)[2])


def docker(ctx, *args, check=True):
    return subprocess.run(["docker", "--context", ctx, *args], capture_output=True, text=True, check=check)


def random_path(rng):
    d = rng.choice(DIRS)
    return os.path.join(d, rng.choice(NAMES)) if d else rng.choice(NAMES)


def mac_op(rng, root):
    rel = random_path(rng)
    p = os.path.join(root, rel)
    kind = rng.choice(["write", "write", "append", "truncate", "rename", "unlink", "mkdir", "rmtree"])
    try:
        if kind == "write":
            os.makedirs(os.path.dirname(p), exist_ok=True)
            with open(p, "wb") as f:
                f.write(os.urandom(rng.randint(0, 9000)))
        elif kind == "append" and os.path.isfile(p):
            with open(p, "ab") as f:
                f.write(os.urandom(rng.randint(1, 300)))
        elif kind == "truncate" and os.path.isfile(p):
            os.truncate(p, rng.randint(0, max(1, os.path.getsize(p))))
        elif kind == "rename" and os.path.isfile(p):
            dst = os.path.join(root, random_path(rng))
            if not os.path.isdir(dst) and os.path.isdir(os.path.dirname(dst)):
                os.replace(p, dst)
        elif kind == "unlink" and os.path.isfile(p):
            os.unlink(p)
        elif kind == "mkdir":
            os.makedirs(os.path.join(root, rng.choice(DIRS[1:])), exist_ok=True)
        elif kind == "rmtree":
            d = os.path.join(root, rng.choice(DIRS[1:]))
            if os.path.isdir(d) and rng.random() < 0.3:
                shutil.rmtree(d)
    except (IsADirectoryError, NotADirectoryError, FileNotFoundError, OSError):
        pass
    return "mac " + kind + " " + rel


def guest_op(rng, ctx, name):
    rel = random_path(rng)
    kind = rng.choice(["write", "append", "rename", "unlink", "mkdir"])
    size = rng.randint(0, 9000)
    cmd = {
        "write": f'mkdir -p "$(dirname /c/{rel})" && [ ! -d /c/{rel} ] && head -c {size} /dev/urandom > /c/{rel}',
        "append": f'[ -f /c/{rel} ] && head -c {size % 300 + 1} /dev/urandom >> /c/{rel}',
        "rename": f'[ -f /c/{rel} ] && dst=/c/{random_path(rng)} && [ -d "$(dirname $dst)" ] && [ ! -d $dst ] && mv /c/{rel} $dst',
        "unlink": f'[ -f /c/{rel} ] && rm /c/{rel}',
        "mkdir": f"mkdir -p /c/{rng.choice(DIRS[1:])}",
    }[kind]
    docker(ctx, "exec", name, "sh", "-c", cmd + " || true")
    return "guest " + kind + " " + rel


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--context", default="rekcod")
    ap.add_argument("--ops", type=int, default=300)
    ap.add_argument("--seed", type=int, default=int(time.time()))
    ap.add_argument("--deadline", type=float, default=1.0)
    args = ap.parse_args()
    rng = random.Random(args.seed)
    root = tempfile.mkdtemp(prefix="rekcod-fsx-", dir=os.path.expanduser("~"))
    name = "rekcod-fsx"
    docker(args.context, "rm", "-f", name, check=False)
    docker(args.context, "run", "-d", "--name", name, "-v", f"{root}:/c", "alpine:3.22", "sleep", "3600")
    print(f"fsx: seed {args.seed}, {args.ops} ops, context {args.context}, dir {root}")
    worst, failures = 0.0, 0
    try:
        for i in range(args.ops):
            op = mac_op(rng, root) if rng.random() < 0.6 else guest_op(rng, args.context, name)
            want = mac_snapshot(root)
            t0 = time.monotonic()
            while True:
                got = docker(args.context, "exec", name, "sh", "-c", SNAPSHOT).stdout.splitlines()
                if got == want:
                    worst = max(worst, time.monotonic() - t0)
                    break
                if time.monotonic() - t0 > args.deadline:
                    failures += 1
                    print(f"  op {i}: {op}: container view still differs after {args.deadline}s")
                    for line in sorted(set(want) ^ set(got)):
                        print(("    mac only:   " if line in want else "    guest only: ") + line)
                    break
        print(f"fsx: {args.ops - failures}/{args.ops} ops converged; slowest {worst * 1000:.0f} ms (incl. one docker exec)")
    finally:
        docker(args.context, "rm", "-f", name, check=False)
        shutil.rmtree(root, ignore_errors=True)
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
