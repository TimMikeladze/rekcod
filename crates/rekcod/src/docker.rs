//! Thin wrappers over the `docker` CLI: the user's own for contexts and
//! save/load, or a bundled one when there is none (docs/cli-passthrough.md).

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result, bail};

pub const CONTEXT: &str = "rekcod";

/// The user's docker CLI: `$REKCOD_DOCKER_CLI`, else `docker` on `PATH`.
pub fn system_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("REKCOD_DOCKER_CLI") {
        return Some(PathBuf::from(p));
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join("docker")).find(|p| p.is_file())
}

pub fn cli() -> Command {
    let mut cmd = Command::new(system_path().unwrap_or_else(|| "docker".into()));
    // An inherited DOCKER_HOST/DOCKER_CONTEXT would silently override --context.
    cmd.env_remove("DOCKER_HOST").env_remove("DOCKER_CONTEXT");
    cmd
}

/// A docker CLI aimed at nothing in particular: the user's, else the bundled one
/// (downloaded on first use) with its own `DOCKER_CONFIG` so its plugins are found.
#[cfg(target_os = "macos")]
pub fn any_cli(paths: &crate::paths::Paths) -> anyhow::Result<Command> {
    if system_path().is_some() {
        return Ok(cli());
    }
    let dir = crate::cli_fetch::ensure_cli(&paths.home)?;
    let mut cmd = Command::new(dir.join("docker"));
    cmd.env_remove("DOCKER_HOST").env_remove("DOCKER_CONTEXT");
    if std::env::var_os("DOCKER_CONFIG").is_none() {
        let config = paths.home.join("docker");
        std::fs::create_dir_all(&config)?;
        let plugins = config.join("cli-plugins");
        let target = dir.join("cli-plugins");
        if std::fs::read_link(&plugins).ok().as_deref() != Some(target.as_path()) {
            let _ = std::fs::remove_file(&plugins);
            std::os::unix::fs::symlink(&target, &plugins).context("link cli-plugins")?;
        }
        cmd.env("DOCKER_CONFIG", config);
    }
    Ok(cmd)
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
