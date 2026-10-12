#!/usr/bin/env python3
"""Round trip of a 64-byte message to a container through a published port.

    python3 bench/tcp_rtt.py --context rekcod     # compare --context desktop-linux

Prints p50/p99 from the Mac (the full port-forward path) and from inside the
container (loopback, the floor), plus requests/s over 8 parallel connections.
"""
import argparse
import socket
import statistics
import subprocess
import sys
import threading
import time

IMAGE = "python:3.13-slim"
NAME = "rekcod-tcprtt"
SERVER = """
import socket, threading
s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("0.0.0.0", 7000)); s.listen(64)
def echo(c):
    c.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
    while (b := c.recv(65536)):
        c.sendall(b)
while True:
    threading.Thread(target=echo, args=(s.accept()[0],), daemon=True).start()
"""
CLIENT = """
import socket, statistics, sys, time
c = socket.create_connection((sys.argv[1], int(sys.argv[2])))
c.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
msg, ts = b"x" * 64, []
for i in range(int(sys.argv[3])):
    t = time.perf_counter_ns(); c.sendall(msg); n = 0
    while n < 64: n += len(c.recv(64 - n))
    ts.append(time.perf_counter_ns() - t)
ts = sorted(ts[200:])
print(ts[len(ts) // 2] / 1000, ts[int(len(ts) * 0.99)] / 1000)
"""


def run(cmd, **kw):
    return subprocess.run(cmd, check=True, capture_output=True, text=True, **kw).stdout


def mac_rps(port, conns=8, secs=5):
    done = [0] * conns
    stop = time.time() + secs

    def worker(i):
        c = socket.create_connection(("127.0.0.1", port))
        c.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        while time.time() < stop:
            c.sendall(b"x" * 64)
            n = 0
            while n < 64:
                n += len(c.recv(64 - n))
            done[i] += 1

    ts = [threading.Thread(target=worker, args=(i,)) for i in range(conns)]
    [t.start() for t in ts]
    [t.join() for t in ts]
    return sum(done) / secs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--context", default="rekcod")
    ap.add_argument("--port", type=int, default=57000)
    ap.add_argument("-n", type=int, default=5000)
    a = ap.parse_args()
    d = ["docker", "--context", a.context]
    subprocess.run(d + ["rm", "-f", NAME], capture_output=True)
    run(d + ["run", "-d", "--name", NAME, "-p", f"127.0.0.1:{a.port}:7000", IMAGE, "python3", "-c", SERVER])
    try:
        for _ in range(100):
            try:
                socket.create_connection(("127.0.0.1", a.port), timeout=1).close()
                break
            except OSError:
                time.sleep(0.1)
        time.sleep(0.5)
        inside = run(d + ["exec", NAME, "python3", "-c", CLIENT, "127.0.0.1", "7000", str(a.n)]).split()
        mac = subprocess.run([sys.executable, "-c", CLIENT, "127.0.0.1", str(a.port), str(a.n)],
                             check=True, capture_output=True, text=True).stdout.split()
        rps = mac_rps(a.port)
        print(f"{a.context:14} mac p50 {float(mac[0]):6.1f} µs p99 {float(mac[1]):6.1f} µs | "
              f"in-container p50 {float(inside[0]):5.1f} µs | mac 8 conns {rps:8.0f} req/s")
    finally:
        subprocess.run(d + ["rm", "-f", NAME], capture_output=True)


if __name__ == "__main__":
    main()
