#!/usr/bin/env python3
"""Phase 0 headroom benchmark (see docs/plan.md).

Measures how far a bind mount is from the two reference points that matter:
  native  - same workload on the Mac's own disk
  volume  - same workload on a VM-local named volume (the practical ceiling
            for any shared-filesystem trick)

    python3 bench/headroom.py --context orbstack
    python3 bench/headroom.py --context desktop-linux
    python3 bench/headroom.py --native-only

The same file runs inside the container (`phases` / `respond` subcommands),
so the workload is byte-for-byte identical on every target. Python 3.9+.
"""
import argparse
import ctypes
import json
import os
import shutil
import statistics
import struct
import subprocess
import sys
import tempfile
import time

IMAGE = "python:3.13-slim"
VOLUME = "rekcod-headroom"
PKGS, FILES, DEPTH = 500, 40, 3  # ~20.5k files, node_modules-shaped
PAYLOAD = b"module.exports = 1;\n" * 25  # 500 bytes


# ---------------------------------------------------------------- workloads


def tree(root):
    """Write storm: create a node_modules-shaped tree."""
    n = 0
    for p in range(PKGS):
        pkg = os.path.join(root, "node_modules", "pkg%d" % p)
        leaf = os.path.join(pkg, *("d%d" % d for d in range(DEPTH)))
        os.makedirs(leaf, exist_ok=True)
        with open(os.path.join(pkg, "package.json"), "w") as f:
            f.write('{"name":"pkg%d","main":"d0/d1/d2/f0.js"}' % p)
        n += 1
        for i in range(FILES):
            with open(os.path.join(leaf, "f%d.js" % i), "wb") as f:
                f.write(PAYLOAD)
            n += 1
    return n


def walk(root):
    """Stat storm: readdir + stat every entry (git status, webpack, tsc)."""
    n = 0
    for dirpath, dirnames, filenames in os.walk(root):
        for name in dirnames + filenames:
            os.lstat(os.path.join(dirpath, name))
            n += 1
    return n


def neg(root):
    """Negative-lookup storm: node/python module resolution probing."""
    n = 0
    for dirpath, _, filenames in os.walk(os.path.join(root, "node_modules")):
        for name in filenames:
            base = os.path.join(dirpath, name)
            for probe in (base + ".ts", base + ".json", base + "/index.js", base + ".node"):
                try:
                    os.stat(probe)
                except OSError:
                    pass
                n += 1
    return n


def read(root):
    """Data: open + read + close every file."""
    n = 0
    for dirpath, _, filenames in os.walk(root):
        for name in filenames:
            with open(os.path.join(dirpath, name), "rb") as f:
                f.read()
            n += 1
    return n


def churn(root):
    """Create then delete 5k files (npm install / rm -rf node_modules)."""
    d = os.path.join(root, "churn")
    os.makedirs(d, exist_ok=True)
    for i in range(5000):
        with open(os.path.join(d, "c%d" % i), "wb") as f:
            f.write(PAYLOAD)
    shutil.rmtree(d)
    return 10000


WORKLOADS = {"tree": tree, "walk": walk, "neg": neg, "read": read, "churn": churn}


def run_phases(root, phases):
    out = []
    for i, name in enumerate(phases):
        t = time.perf_counter()
        ops = WORKLOADS[name](root)
        ms = (time.perf_counter() - t) * 1000
        # A phase's first run sees cold guest caches; a repeat sees warm ones.
        label = name + ("#2" if name in phases[:i] else "")
        out.append({"phase": label, "ms": round(ms, 1), "ops": ops})
    return out


# ------------------------------------------------- inotify round-trip probe

# The mask real watchers use (libuv/chokidar, watchdog): content, attribute,
# create and rename events.
IN_MODIFY, IN_ATTRIB, IN_CLOSE_WRITE, IN_MOVED_TO, IN_CREATE = 0x2, 0x4, 0x8, 0x80, 0x100


def respond(d):
    """In-guest: on each host write of `ping`, answer by writing `pong`.

    Runs until killed. Hosts may deliver duplicate events per change, so
    answer each distinct sequence number once."""
    libc = ctypes.CDLL("libc.so.6", use_errno=True)
    fd = libc.inotify_init1(0)
    if libc.inotify_add_watch(fd, d.encode(), IN_MODIFY | IN_ATTRIB | IN_CLOSE_WRITE | IN_MOVED_TO | IN_CREATE) < 0:
        sys.exit("inotify_add_watch failed: errno %d" % ctypes.get_errno())
    open(os.path.join(d, "ready"), "w").close()
    last = None
    while True:
        buf = os.read(fd, 4096)
        off = 0
        while off < len(buf):
            _, _, _, ln = struct.unpack_from("iIII", buf, off)
            name = buf[off + 16 : off + 16 + ln].rstrip(b"\0")
            off += 16 + ln
            if name == b"ping":
                with open(os.path.join(d, "ping")) as f:
                    seq = f.read()
                if seq == last:
                    continue
                last = seq
                tmp = os.path.join(d, ".pong")
                with open(tmp, "w") as f:
                    f.write(seq)
                os.replace(tmp, os.path.join(d, "pong"))


def wait_for(path, want, timeout):
    end = time.perf_counter() + timeout
    while time.perf_counter() < end:
        try:
            with open(path) as f:
                if f.read() == want:
                    return True
        except OSError:
            pass
        time.sleep(0.0002)
    return False


def latency(ctx, host_dir, count=30):
    """Host write -> guest inotify -> guest write -> host sees it (round trip)."""
    d = os.path.join(host_dir, "watch")
    os.makedirs(d, exist_ok=True)
    name = "rekcod-headroom-respond"
    subprocess.run(docker(ctx, "rm", "-f", name), capture_output=True)
    proc = subprocess.Popen(
        docker(ctx, "run", "--rm", "--name", name, "-v", "%s:/w" % host_dir, "-v", "%s:/bench:ro" % HERE, IMAGE,
               "python", "/bench/headroom.py", "respond", "/w/watch"),
        stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        if not wait_for_exists(os.path.join(d, "ready"), 60):
            return {"error": "responder never became ready"}
        samples = []
        for i in range(count):
            seq = str(i)
            tmp = os.path.join(d, ".ping")
            with open(tmp, "w") as f:
                f.write(seq)
            t = time.perf_counter()
            os.replace(tmp, os.path.join(d, "ping"))
            if not wait_for(os.path.join(d, "pong"), seq, 2.0):
                return {"error": "no inotify event in guest within 2s (polling-only?)"}
            samples.append((time.perf_counter() - t) * 1000)
            time.sleep(0.3)  # one edit at a time, like a human saving a file
        samples.sort()
        return {"p50_ms": round(statistics.median(samples), 2),
                "p90_ms": round(samples[int(len(samples) * 0.9) - 1], 2)}
    finally:
        proc.kill()
        subprocess.run(docker(ctx, "rm", "-f", name), capture_output=True)


def wait_for_exists(path, timeout):
    end = time.perf_counter() + timeout
    while time.perf_counter() < end:
        if os.path.exists(path):
            return True
        time.sleep(0.05)
    return False


# ------------------------------------------------------------- orchestration

HERE = os.path.dirname(os.path.abspath(__file__))


def docker(ctx, *args):
    return ["docker", "--context", ctx, *args] if ctx else ["docker", *args]


def in_container(ctx, mount, phases):
    cmd = docker(ctx, "run", "--rm", "-v", "%s:/w" % mount, "-v", "%s:/bench:ro" % HERE, IMAGE,
                 "python", "/bench/headroom.py", "phases", "/w", ",".join(phases))
    out = subprocess.run(cmd, check=True, capture_output=True, text=True).stdout
    return json.loads(out)


def table(results):
    targets = [t for t in ("native", "volume", "bind") if t in results]
    by = {t: {r["phase"]: r["ms"] for r in results[t]} for t in targets}
    phases = [r["phase"] for r in results["native"]]
    cols = targets + (["bind/vol", "bind/native"] if "bind" in by else [])
    print("%-8s" % "phase" + "".join("%12s" % c for c in cols))
    for p in phases:
        row = [by[t].get(p) for t in targets]
        line = "%-8s" % p + "".join("%12s" % ("-" if v is None else "%.0f ms" % v) for v in row)
        if "bind" in by and by["bind"].get(p) is not None:
            b = by["bind"][p]
            v, n = by["volume"].get(p), by["native"].get(p)
            line += "%12s" % ("%.2fx" % (b / v) if v else "-")
            line += "%12s" % ("%.2fx" % (b / n) if n else "-")
        print(line)


def main():
    if len(sys.argv) > 1 and sys.argv[1] == "phases":
        print(json.dumps(run_phases(sys.argv[2], sys.argv[3].split(","))))
        return
    if len(sys.argv) > 1 and sys.argv[1] == "respond":
        respond(sys.argv[2])
        return

    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--context", help="docker context (orbstack, desktop-linux, rekcod)")
    ap.add_argument("--native-only", action="store_true")
    ap.add_argument("--out", help="append JSON results to this file")
    a = ap.parse_args()
    if not a.native_only:
        ping = subprocess.run(docker(a.context, "version", "--format", "{{.Server.Version}}"), capture_output=True, text=True)
        if ping.returncode != 0:
            err = (ping.stderr.strip().splitlines() or ["unknown error"])[0]
            sys.exit("docker context %r not usable: %s" % (a.context, err))

    # Same phase order everywhere. On bind the tree is written by the Mac,
    # so the guest's first walk is genuinely cold.
    # Second runs show the warm (cached) cost; first runs the cold cost.
    measured = ["walk", "walk", "neg", "neg", "read", "read", "churn"]
    results = {}
    native = tempfile.mkdtemp(prefix="rekcod-native-")
    try:
        results["native"] = run_phases(native, ["tree"] + measured)
    finally:
        shutil.rmtree(native, ignore_errors=True)

    if not a.native_only:
        subprocess.run(docker(a.context, "volume", "rm", "-f", VOLUME), capture_output=True)
        subprocess.run(docker(a.context, "pull", "-q", IMAGE), check=True, capture_output=True)
        results["volume"] = in_container(a.context, VOLUME, ["tree"] + measured)
        subprocess.run(docker(a.context, "volume", "rm", "-f", VOLUME), capture_output=True)

        # Under $HOME: every tool shares it by default; /tmp often is not shared.
        bind = tempfile.mkdtemp(prefix=".rekcod-bind-", dir=os.path.expanduser("~"))
        try:
            tree(bind)  # host-side write, untimed in the bind column
            results["bind"] = [{"phase": "tree", "ms": None, "ops": 0}] + in_container(a.context, bind, measured)
            results["inotify_rtt"] = latency(a.context, bind)
        finally:
            shutil.rmtree(bind, ignore_errors=True)

    print("context: %s" % ("native-only" if a.native_only else a.context or os.environ.get("DOCKER_HOST", "default")))
    table(results)
    if "inotify_rtt" in results:
        print("inotify round trip (host write -> guest event -> host sees reply):", results["inotify_rtt"])
    if a.out:
        with open(a.out, "a") as f:
            f.write(json.dumps({"context": a.context, "ts": time.time(), "results": results}) + "\n")


if __name__ == "__main__":
    main()
