#!/usr/bin/env python3
"""Mac-side change -> container inotify latency, measured as a round trip.

The Mac writes `w/ping`; a container watching `w` with inotifyd writes
`r/pong-<n>` into a second bind mount; the Mac polls for that file. The
round trip includes the guest's write back to the Mac (well under 1 ms), so
it is an upper bound on the one-way delay.

    python3 bench/inotify_rtt.py [--context rekcod] [-n 50]
"""

import argparse
import os
import shutil
import statistics
import subprocess
import tempfile
import time


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--context", default=None)
    ap.add_argument("-n", type=int, default=50)
    args = ap.parse_args()
    docker = ["docker"] + (["--context", args.context] if args.context else [])

    work = tempfile.mkdtemp(prefix="rekcod-rtt.", dir=os.path.expanduser("~"))
    w, r = os.path.join(work, "w"), os.path.join(work, "r")
    os.makedirs(w)
    os.makedirs(r)
    name = f"rekcod-rtt-{os.getpid()}"
    # inotifyd prints "<events>\t<dir>\t<name>"; reply on close-after-write or create.
    handler = ('while IFS="$(printf "\\t")" read -r ev d f; do case "$f:$ev" in ping:*w*|ping:*n*) '
               'n=$(cat /w/ping) && [ -n "$n" ] && touch /r/pong-$n;; esac; done')
    subprocess.run(
        docker + ["run", "-d", "--rm", "--name", name, "-v", f"{w}:/w", "-v", f"{r}:/r",
                  "alpine:3.22", "sh", "-c", f"inotifyd - /w | ({handler})"],
        check=True, stdout=subprocess.DEVNULL,
    )
    samples = []
    try:
        time.sleep(1.5)
        for i in range(args.n + 3):
            pong = os.path.join(r, f"pong-{i}")
            t0 = time.perf_counter()
            with open(os.path.join(w, "ping"), "w") as f:
                f.write(str(i))
            deadline = t0 + 2
            while not os.path.exists(pong):
                if time.perf_counter() > deadline:
                    break
                time.sleep(0.0002)
            else:
                if i >= 3:  # first few warm the path
                    samples.append((time.perf_counter() - t0) * 1000)
            time.sleep(0.12)  # keep edits apart so each is its own event
    finally:
        subprocess.run(docker + ["rm", "-f", name], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        shutil.rmtree(work, ignore_errors=True)
    if not samples:
        raise SystemExit("no round trips observed")
    samples.sort()
    p = lambda q: samples[min(len(samples) - 1, int(q * len(samples)))]
    print(f"inotify round trip over {len(samples)}/{args.n}: p50 {p(0.5):.1f} ms, p90 {p(0.9):.1f} ms, "
          f"min {samples[0]:.1f} ms, mean {statistics.mean(samples):.1f} ms")


if __name__ == "__main__":
    main()
