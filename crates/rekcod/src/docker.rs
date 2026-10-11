//! Thin wrappers over the user's own `docker` CLI (contexts, save/load).

use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result, bail};

pub const CONTEXT: &str = "rekcod";

pub fn cli() -> Command {
    let mut cmd = Command::new("docker");
    // An inherited DOCKER_HOST/DOCKER_CONTEXT would silently override --context.
    cmd.env_remove("DOCKER_HOST").env_remove("DOCKER_CONTEXT");
    cmd
}

pub fn available() -> bool {
    cli().arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

pub fn run(args: &[&str]) -> Result<Output> {
    let out = cli().args(args).stdin(Stdio::null()).output().context("run docker CLI")?;
    Ok(out)
}

pub fn run_ok(args: &[&str]) -> Result<String> {
    let out = run(args)?;
    if !out.status.success() {
        bail!("docker {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn current_context() -> Result<String> {
    run_ok(&["context", "show"])
}

pub fn context_exists(name: &str) -> bool {
    run(&["context", "inspect", name]).is_ok_and(|o| o.status.success())
}

/// Creates or repoints the `rekcod` context at `host`.
pub fn ensure_context(host: &str) -> Result<()> {
    let endpoint = format!("host={host}");
    if context_exists(CONTEXT) {
        run_ok(&["context", "update", CONTEXT, "--docker", &endpoint])?;
    } else {
        run_ok(&["context", "create", CONTEXT, "--description", "rekcod", "--docker", &endpoint])?;
    }
    Ok(())
}

pub fn use_context(name: &str) -> Result<()> {
    run_ok(&["context", "use", name]).map(|_| ())
}
