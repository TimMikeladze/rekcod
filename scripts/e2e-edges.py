#!/usr/bin/env python3
"""Two edge processes, one controller and a loopback backend; no VMs or launchd jobs."""

import concurrent.futures
import http.client
import http.server
import json
import os
from pathlib import Path
import plistlib
import socket
import sqlite3
import ssl
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request


BINARY = Path(os.environ.get("REKCOD", Path(__file__).resolve().parents[1] / "target/release/rekcod")).resolve()


def port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def wait_for(check, timeout=15):
    end = time.monotonic() + timeout
    last_error = None
    while time.monotonic() < end:
        try:
            result = check()
            if result:
                return result
        except (OSError, urllib.error.URLError, sqlite3.Error) as error:
            last_error = error
        time.sleep(0.1)
    raise AssertionError("condition did not become true within its timeout") from last_error


class Backend(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"ok\n"
        self.send_response(200)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *_):
        pass


def main():
    processes, logs = [], []
    stop = threading.Event()
    backend = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Backend)
    threading.Thread(target=backend.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory(prefix="rekcod-edges-") as temp:
        root = Path(temp)

        def run(home, *args, success=True):
            home.mkdir(exist_ok=True)
            env = {**os.environ, "REKCOD_HOME": str(home), "REKCOD_NO_LAUNCHCTL": "1"}
            result = subprocess.run([str(BINARY), *args], env=env, capture_output=True, text=True, timeout=30)
            if success and result.returncode:
                raise AssertionError(result.stderr)
            if not success:
                assert result.returncode != 0, "invalid setup unexpectedly succeeded"
            return result

        def spawn(home, *args):
            home.mkdir(exist_ok=True)
            log = (home / "process.log").open("w+")
            logs.append(log)
            process = subprocess.Popen([str(BINARY), *args], env={**os.environ, "REKCOD_HOME": str(home)}, stdout=log, stderr=log)
            processes.append(process)
            return process

        controller = root / "controller"
        url = f"https://127.0.0.1:{port()}"
        spawn(controller, "controller", "serve", "--listen", url.removeprefix("https://"), "--domain", "apps.test")
        ca_file = controller / "controller/ca.pem"
        wait_for(lambda: ca_file.exists() and (controller / "controller/ca-fingerprint").exists())
        admin = (controller / "controller/admin-token").read_text().strip()
        join = (controller / "controller/join-token").read_text().strip()
        fingerprint = (controller / "controller/ca-fingerprint").read_text().strip()
        context = ssl.create_default_context(cafile=str(ca_file))
        context.verify_flags |= ssl.VERIFY_X509_STRICT
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), urllib.request.HTTPSHandler(context=context))

        def api(path, token=admin, data=None, raw=False):
            request = urllib.request.Request(url + path, data=None if data is None else json.dumps(data).encode(), headers={"Authorization": f"Bearer {token}", "Content-Type": "application/json"})
            with opener.open(request, timeout=5) as response:
                body = response.read().decode()
                return body if raw else json.loads(body)

        try:
            wait_for(lambda: api("/nodes") == [])
            invalid = root / "invalid"
            run(invalid, "server", "setup", success=False)
            run(invalid, "server", "setup", "--edge-only", success=False)
            assert not list(invalid.iterdir()), "invalid setup changed its home"

            edges = []
            for label, address in [("a", "127.0.0.1"), ("b", "127.0.0.2")]:
                home = root / label
                run(home, "remote", "add", "fleet", url, "--token", admin, "--ca-fingerprint", fingerprint)
                run(home, "server", "setup", "--edge-only", "--remote", "fleet", "--join-token", join, "--addr", address, "--no-power")
                node = json.loads((home / "node.json").read_text())
                # The repeat uses the existing node and does not need a join token.
                run(home, "server", "setup", "--edge-only", "--remote", "fleet", "--addr", address, "--no-power")
                assert json.loads((home / "node.json").read_text())["id"] == node["id"]
                assert not (home / "controller").exists() and not (home / "storage").exists()
                plists = sorted(p.name for p in home.glob("*.plist"))
                assert plists == ["dev.rekcod.edge.plist", "dev.rekcod.node.plist"], plists
                with (home / "dev.rekcod.edge.plist").open("rb") as f:
                    service = plistlib.load(f)["ProgramArguments"]
                assert admin not in service and join not in service
                edge_remote = "edge-" + node["id"]
                token = json.loads((home / "remotes.json").read_text())["remotes"][edge_remote]["token"]
                try:
                    api("/nodes", token)
                    raise AssertionError("edge token can administer the fleet")
                except urllib.error.HTTPError as e:
                    assert e.code == 403
                edges.append((home, edge_remote, token))
            assert len(api("/nodes")) == 2, "setup repeated a node registration"
            print("ok: edge-only setup installs only node and edge; repeat preserves registration and scoped tokens", flush=True)

            endpoint = f"127.0.0.1:{backend.server_address[1]}"
            db_path = controller / "controller/fleet.db"
            with sqlite3.connect(db_path) as db:
                db.execute("insert into nodes (id,url,secret,last_seen) values ('fixture',?,'fixture-secret',?)", ("http://" + endpoint, int(time.time())))
                db.execute("insert into apps (name,owner,git_url,git_ref,port,created,public,live_release) values ('web','test','unused','main',80,?,1,1)", (int(time.time()),))
                db.execute("insert into releases (app,n,commit_sha,state,created) values ('web',1,?,'live',?)", ("a" * 40, int(time.time())))
                db.execute("insert into instances values ('web',1,'fixture-sandbox','fixture',?,'live')", (endpoint,))

            def heartbeat():
                while not stop.is_set():
                    api("/nodes/fixture/heartbeat", "fixture-secret", {"sandboxes": ["fixture-sandbox"], "mem_free_mib": 0})
                    stop.wait(2)

            threading.Thread(target=heartbeat, daemon=True).start()
            listeners = []
            for home, remote, _ in edges:
                address = f"127.0.0.1:{port()}"
                process = spawn(home, "edge", "serve", "--remote", remote, "--listen", address, "--rate", "1000/s", "--burst", "1000")
                listeners.append((address, process))
                wait_for(lambda h=home: (h / "edge-routes.json").exists())

            def get(address, client):
                request = urllib.request.Request("http://" + address + "/", headers={"Host": "web.apps.test", "CF-Connecting-IP": client})
                with opener.open(request, timeout=2) as response:
                    assert response.status == 200 and response.read() == b"ok\n"

            # Keep this traffic in one complete minute for an exact /metrics comparison.
            while int(time.time()) % 60 > 50:
                time.sleep(0.1)
            minute = int(time.time()) // 60
            for (address, _), count in zip(listeners, [12, 8]):
                for _ in range(count):
                    get(address, "203.0.113.9")

            def reported():
                with sqlite3.connect(db_path) as db:
                    return db.execute("select coalesce(sum(requests),0) from traffic where app='web' and minute=?", (minute,)).fetchone()[0] == 20

            wait_for(reported, timeout=70)
            metrics = api("/metrics", edges[1][2], raw=True)
            assert 'rekcod_app_requests_last_minute{app="web"} 20' in metrics, metrics
            clients = api("/edge/clients")
            assert next(c["requests"] for c in clients if c["ip"] == "203.0.113.9") == 20, clients
            print("ok: controller metrics and clients sum 12 requests on edge A plus 8 on edge B", flush=True)

            ready = threading.Event()

            def load():
                for i in range(100):
                    if i == 20:
                        ready.set()
                    for address, _ in listeners:
                        try:
                            get(address, "203.0.113.10")
                            break
                        except (OSError, urllib.error.URLError, http.client.HTTPException):
                            continue
                    else:
                        raise AssertionError("both edge addresses failed")
                    time.sleep(0.01)

            with concurrent.futures.ThreadPoolExecutor(max_workers=1) as pool:
                future = pool.submit(load)
                assert ready.wait(5)
                listeners[0][1].terminate()
                listeners[0][1].wait(timeout=5)
                future.result(timeout=15)
            get(listeners[1][0], "203.0.113.10")
            print("ok: 100 requests succeed through a client with failover while edge A is terminated", flush=True)
            print("all edge checks passed", flush=True)
        except BaseException:
            for log in logs:
                log.flush()
                log.seek(0)
                print(log.read(), flush=True)
            raise
        finally:
            stop.set()
            for process in reversed(processes):
                if process.poll() is None:
                    process.terminate()
            for process in reversed(processes):
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()
            for log in logs:
                log.close()
            backend.shutdown()
            backend.server_close()


if __name__ == "__main__":
    main()
