//! `rekcod mcp` run as agents run it: JSON-RPC on stdin and stdout, against fake sandboxes
//! in a scratch REKCOD_HOME and a docker CLI stand-in. No VM is booted.

use std::io::{BufRead, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("rekcod-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

/// A stopped copy-mode sandbox of `project`, laid out as `sandbox create` leaves one.
fn sandbox(home: &Path, id: &str, project: &Path) -> PathBuf {
    let dir = home.join("sb").join(id);
    std::fs::create_dir_all(dir.join("run")).unwrap();
    std::fs::create_dir_all(dir.join("ws")).unwrap();
    let meta = serde_json::json!({
        "id": id, "name": null, "project": project, "mode": "copy", "image": "alpine", "golden": null,
        "allow": [], "offline": true, "cpus": 1, "memory_gib": 1, "ttl_secs": 0, "secrets": [], "created": 0,
    });
    std::fs::write(dir.join("meta.json"), meta.to_string()).unwrap();
    dir
}

/// A `docker` that stands in for `docker exec` into the workspace: `write_file` stores stdin
/// in one file and `read_file` prints it. Under `/proc` it fails before reading the body,
/// as `mkdir -p` does in a sandbox.
fn docker_mock(base: &Path) -> PathBuf {
    let docker = base.join("docker");
    let store = base.join("store");
    let script = format!(
        "#!/bin/sh\nfor last; do :; done\ncase \"$last\" in /proc/*) echo 'mkdir: cannot create directory' >&2; exit 1;; esac\n\
         case \"$*\" in *'cat > '*) cat > '{0}';; *' cat -- '*) cat '{0}';; *) exit 1;; esac\n",
        store.display()
    );
    std::fs::write(&docker, script).unwrap();
    std::fs::set_permissions(&docker, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    docker
}

/// Makes the sandbox count as running: its pid file, locked by this process.
fn hold(dir: &Path) -> std::fs::File {
    let mut f = std::fs::File::create(dir.join("run/daemon.pid")).unwrap();
    assert_eq!(unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
    write!(f, "{}", std::process::id()).unwrap();
    f
}

#[test]
fn mcp_survives_a_child_that_drops_stdin_and_keeps_stdout_for_replies() {
    let base = scratch("mcp");
    let (home, project) = (base.join("home"), base.join("p"));
    std::fs::create_dir_all(&project).unwrap();
    let docker = docker_mock(&base);
    let running = sandbox(&home, "runaaaaa", &project);
    let _held = hold(&running);
    sandbox(&home, "stopaaaa", &project);

    let mut mcp = Command::new(env!("CARGO_BIN_EXE_rekcod"))
        .args(["mcp", "--root"])
        .arg(&project)
        .env("REKCOD_HOME", &home)
        .env("REKCOD_DOCKER_CLI", &docker)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let call = |id: u32, tool: &str, args: serde_json::Value| serde_json::json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": tool, "arguments": args}});
    let big = "x".repeat(1 << 20);
    let mut input = mcp.stdin.take().unwrap();
    for msg in [
        call(1, "sandbox_write_file", serde_json::json!({"id": "runaaaaa", "path": "/proc/x/y", "content": big})),
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "ping"}),
        call(3, "sandbox_remove", serde_json::json!({"id": "stopaaaa"})),
        // Bytes that aren't UTF-8 go both ways as base64.
        call(4, "sandbox_write_file", serde_json::json!({"id": "runaaaaa", "path": "/w/bin", "content_base64": "//4A"})),
        call(5, "sandbox_read_file", serde_json::json!({"id": "runaaaaa", "path": "/w/bin"})),
    ] {
        writeln!(input, "{msg}").unwrap();
    }
    drop(input);
    let lines: Vec<String> = std::io::BufReader::new(mcp.stdout.take().unwrap()).lines().map(Result::unwrap).collect();
    let status = mcp.wait().unwrap();
    assert!(status.success(), "{status}: {lines:?}");
    // Every line on stdout is a reply, in order: nothing else writes into the stream.
    let replies: Vec<serde_json::Value> = lines.iter().map(|l| serde_json::from_str(l).expect(l)).collect();
    let ids: Vec<u64> = replies.iter().map(|r| r["id"].as_u64().unwrap()).collect();
    assert_eq!(ids, vec![1, 2, 3, 4, 5], "{lines:?}");
    assert_eq!(replies[0]["result"]["isError"], true);
    assert!(replies[0]["result"]["content"][0]["text"].as_str().unwrap().contains("cannot create"), "{}", lines[0]);
    assert_eq!(replies[2]["result"]["isError"], false, "{}", lines[2]);
    assert!(!home.join("sb/stopaaaa").exists());
    assert_eq!(std::fs::read(base.join("store")).unwrap(), b"\xff\xfe\x00");
    let read: serde_json::Value = serde_json::from_str(replies[4]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(read["content_base64"], "//4A", "{}", lines[4]);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn http_files_carry_bytes() {
    let base = scratch("http");
    let (home, project) = (base.join("home"), base.join("p"));
    std::fs::create_dir_all(&project).unwrap();
    let docker = docker_mock(&base);
    let _held = hold(&sandbox(&home, "httpaaaa", &project));
    let mut serve = Command::new(env!("CARGO_BIN_EXE_rekcod"))
        .args(["sandbox", "serve", "--root"])
        .arg(&project)
        .env("REKCOD_HOME", &home)
        .env("REKCOD_DOCKER_CLI", &docker)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let sock = home.join("run/sandbox.sock");
    let t0 = std::time::Instant::now();
    while !sock.exists() && t0.elapsed() < std::time::Duration::from_secs(20) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let request = |head: &str, body: &[u8]| {
        let mut s = std::os::unix::net::UnixStream::connect(&sock).unwrap();
        write!(s, "{head}\r\nContent-Length: {}\r\n\r\n", body.len()).unwrap();
        s.write_all(body).unwrap();
        let mut out = Vec::new();
        std::io::Read::read_to_end(&mut s, &mut out).unwrap();
        let at = out.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
        (String::from_utf8_lossy(&out[..at]).to_string(), out[at + 4..].to_vec())
    };
    let bytes = b"\xff\xfe\x00 \xc3";
    let (head, body) = request("PUT /sandboxes/httpaaaa/files?path=/w/bin HTTP/1.1", bytes);
    assert!(head.starts_with("HTTP/1.1 200"), "{head} {}", String::from_utf8_lossy(&body));
    let (head, body) = request("GET /sandboxes/httpaaaa/files?path=/w/bin HTTP/1.1", b"");
    let _ = serve.kill();
    let _ = serve.wait();
    assert!(head.starts_with("HTTP/1.1 200"), "{head}");
    assert_eq!(body, bytes);
    std::fs::remove_dir_all(base).unwrap();
}
