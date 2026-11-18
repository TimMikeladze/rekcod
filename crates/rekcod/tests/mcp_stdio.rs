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
    // `docker exec` that fails before reading the body, as when `mkdir -p` fails in the sandbox.
    let docker = base.join("docker");
    std::fs::write(&docker, "#!/bin/sh\necho 'mkdir: cannot create directory' >&2\nexit 1\n").unwrap();
    std::fs::set_permissions(&docker, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
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
    assert_eq!(ids, vec![1, 2, 3], "{lines:?}");
    assert_eq!(replies[0]["result"]["isError"], true);
    assert!(replies[0]["result"]["content"][0]["text"].as_str().unwrap().contains("cannot create"), "{}", lines[0]);
    assert_eq!(replies[2]["result"]["isError"], false, "{}", lines[2]);
    assert!(!home.join("sb/stopaaaa").exists());
    std::fs::remove_dir_all(base).unwrap();
}
