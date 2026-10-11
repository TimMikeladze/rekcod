//! Phase 6 research prototype: is a custom metadata server over vsock faster
//! per request than Apple's virtio-fs server on cold lookups?
//!
//! With `REKCOD_RESEARCH=1` the host daemon connects to META_PORT and answers
//! `lstat <path>` / `readdir <path>` lines with the Mac's own syscalls. The
//! control command `benchmeta <dir>` then times the same cold lstats both ways:
//! through the host server, and through virtio-fs after dropping the guest's
//! caches for `<dir>`. Not used by anything else.

use std::sync::Mutex;
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio_vsock::VsockStream;

static SERVER: Mutex<Option<BufReader<VsockStream>>> = Mutex::new(None);

pub async fn serve_client(stream: VsockStream) -> Result<()> {
    *SERVER.lock().unwrap() = Some(BufReader::new(stream));
    Ok(())
}

async fn ask(conn: &mut BufReader<VsockStream>, req: &str) -> Result<String> {
    conn.get_mut().write_all(req.as_bytes()).await?;
    let mut line = String::new();
    conn.read_line(&mut line).await?;
    Ok(line)
}

/// `benchmeta <dir>` reply: per-request microseconds, vsock server vs virtio-fs.
pub async fn bench(dir: &str) -> Result<String> {
    let mut conn = SERVER.lock().unwrap().take().context("no host metadata server (start with REKCOD_RESEARCH=1)")?;
    // Collect up to 5000 paths through the host server (no guest caching involved).
    let mut paths = Vec::new();
    let mut stack = vec![dir.trim_end_matches('/').to_string()];
    let t0 = Instant::now();
    let mut readdirs = 0u32;
    while let Some(d) = stack.pop() {
        if paths.len() >= 5000 {
            break;
        }
        let names = ask(&mut conn, &format!("readdir {d}\n")).await?;
        readdirs += 1;
        for entry in names.trim_end_matches('\n').split('\t').filter(|e| !e.is_empty()) {
            let (kind, name) = entry.split_at(1);
            let p = format!("{d}/{name}");
            if kind == "d" {
                stack.push(p.clone());
            }
            paths.push(p);
        }
    }
    let readdir_us = t0.elapsed().as_micros() as f64 / readdirs.max(1) as f64;
    let t0 = Instant::now();
    for p in &paths {
        ask(&mut conn, &format!("lstat {p}\n")).await?;
    }
    let vsock_us = t0.elapsed().as_micros() as f64 / paths.len().max(1) as f64;
    *SERVER.lock().unwrap() = Some(conn);

    // Same lstats through virtio-fs, cold: drop the guest's caches for the tree.
    let _ = std::fs::write("/sys/module/fuse/parameters/rekcod_notify", format!("{}/", dir.trim_end_matches('/')));
    let _ = std::fs::write("/proc/sys/vm/drop_caches", "2");
    let paths2 = paths.clone();
    let virtiofs_us = tokio::task::spawn_blocking(move || {
        let t0 = Instant::now();
        for p in &paths2 {
            let _ = std::fs::symlink_metadata(p);
        }
        t0.elapsed().as_micros() as f64 / paths2.len().max(1) as f64
    })
    .await?;
    Ok(format!(
        "ok n={} vsock_lstat_us={vsock_us:.1} virtiofs_cold_lstat_us={virtiofs_us:.1} vsock_readdir_us={readdir_us:.1}",
        paths.len()
    ))
}
