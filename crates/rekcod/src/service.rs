//! LaunchAgents for the fleet's long-running commands (`controller serve`, `node serve`,
//! `edge serve`, `storage serve` with `--install`), so a spare Mac brings them back after a reboot or crash.
//! They're per-user agents: they start at login, so the Mac needs automatic login.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::paths::Paths;

pub fn label(name: &str) -> String {
    format!("dev.rekcod.{name}")
}

fn plist_path(name: &str) -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME not set")?;
    Ok(Path::new(&home).join("Library/LaunchAgents").join(format!("{}.plist", label(name))))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// A KeepAlive agent running `exe args...`, logging to `log`. `crash_only`: launchd
/// restarts it only after a non-zero exit, so a deliberate stop sticks.
pub fn plist_with(name: &str, exe: &Path, args: &[String], log: &Path, rekcod_home: Option<&Path>, crash_only: bool) -> String {
    let argv: String = std::iter::once(exe.display().to_string())
        .chain(args.iter().cloned())
        .map(|a| format!("    <string>{}</string>\n", xml_escape(&a)))
        .collect();
    let env = rekcod_home
        .map(|h| {
            format!(
                "  <key>EnvironmentVariables</key>\n  <dict><key>REKCOD_HOME</key><string>{}</string></dict>\n",
                xml_escape(&h.display().to_string())
            )
        })
        .unwrap_or_default();
    let log = xml_escape(&log.display().to_string());
    let keep_alive = if crash_only { "<dict><key>SuccessfulExit</key><false/></dict>" } else { "<true/>" };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{}</string>
  <key>ProgramArguments</key>
  <array>
{argv}  </array>
{env}  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key>{keep_alive}
  <key>ThrottleInterval</key><integer>5</integer>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        label(name)
    )
}

/// The command line this process was started with, minus `--install`.
pub fn args_without_install() -> Vec<String> {
    strip(&std::env::args().skip(1).collect::<Vec<_>>())
}

pub fn strip(argv: &[String]) -> Vec<String> {
    argv.iter().filter(|a| *a != "--install").cloned().collect()
}

fn domain() -> String {
    format!("gui/{}", unsafe { libc::getuid() })
}

fn launchctl(args: &[&str]) -> Result<std::process::Output> {
    Ok(Command::new("/bin/launchctl").args(args).output()?)
}

/// Writes the agent and loads it now. `REKCOD_NO_LAUNCHCTL` writes it into the rekcod home
/// instead and loads nothing (dry run).
pub fn install(paths: &Paths, name: &str, args: &[String]) -> Result<()> {
    install_with(paths, name, args, false)
}

pub fn installed(name: &str) -> bool {
    plist_path(name).is_ok_and(|p| p.exists())
}

/// Starts the agent's job now if it isn't running (no-op otherwise).
pub fn kickstart(name: &str) {
    let _ = launchctl(&["kickstart", &format!("{}/{}", domain(), label(name))]);
}

/// The path to put in a LaunchAgent: the one this binary was run by when that's a symlink
/// into a Homebrew Cellar (`/opt/homebrew/bin/rekcod`), whose target changes with every
/// `brew upgrade`; otherwise the binary's own path.
fn stable_exe() -> Result<PathBuf> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let argv0 = std::env::args().next().unwrap_or_default();
    let invoked = if argv0.contains('/') {
        Some(PathBuf::from(&argv0))
    } else {
        std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(&argv0)).find(|c| c.exists()))
    };
    if let Some(link) = invoked.filter(|l| l.is_absolute() && std::fs::symlink_metadata(l).is_ok_and(|m| m.file_type().is_symlink()))
        && link.canonicalize().is_ok_and(|t| t == exe)
        && exe.components().any(|c| c.as_os_str() == "Cellar")
    {
        return Ok(link);
    }
    Ok(exe)
}

pub fn install_with(paths: &Paths, name: &str, args: &[String], crash_only: bool) -> Result<()> {
    let exe = stable_exe()?;
    let dry = std::env::var_os("REKCOD_NO_LAUNCHCTL").is_some();
    let path = if dry { paths.home.join(format!("{}.plist", label(name))) } else { plist_path(name)? };
    std::fs::create_dir_all(path.parent().context("plist dir")?)?;
    let log = paths.home.join("logs").join(format!("{name}.log"));
    let home = (!paths.is_default_home()).then_some(paths.home.as_path());
    std::fs::write(&path, plist_with(name, &exe, args, &log, home, crash_only))?;
    if dry {
        println!("wrote {} (dry run: not loaded)", path.display());
        return Ok(());
    }
    // Replace a previous registration (new arguments, or the binary moved).
    let _ = launchctl(&["bootout", &format!("{}/{}", domain(), label(name))]);
    let out = launchctl(&["bootstrap", &domain(), &path.display().to_string()])?;
    if !out.status.success() {
        bail!("launchctl bootstrap: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let restarts = if crash_only { "restarts if it crashes" } else { "restarts if it exits" };
    println!("{name} installed: runs now and at login, {restarts}; log {}", log.display());
    Ok(())
}

pub fn uninstall(name: &str) -> Result<()> {
    let path = plist_path(name)?;
    let _ = launchctl(&["bootout", &format!("{}/{}", domain(), label(name))]);
    match std::fs::remove_file(&path) {
        Ok(()) => println!("{name} removed ({})", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => println!("{name} was not installed"),
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_install() {
        let a: Vec<String> = ["controller", "serve", "--install", "--db", "/x/fleet.db"].map(String::from).into();
        assert_eq!(strip(&a), ["controller", "serve", "--db", "/x/fleet.db"]);
    }

    #[test]
    fn plist_keeps_alive_and_logs() {
        let p = plist_with(
            "edge",
            Path::new("/opt/r&d/rekcod"),
            &["edge".into(), "serve".into()],
            Path::new("/h/logs/edge.log"),
            Some(Path::new("/h")),
            false,
        );
        assert!(p.contains("<string>/opt/r&amp;d/rekcod</string>\n    <string>edge</string>\n    <string>serve</string>"), "{p}");
        assert!(p.contains("<key>KeepAlive</key><true/>") && p.contains("<string>dev.rekcod.edge</string>"));
        assert!(p.contains("<key>REKCOD_HOME</key><string>/h</string>"));
        // plutil is the authority on whether launchd can read it.
        let f = std::env::temp_dir().join(format!("rekcod-plist-{}.plist", std::process::id()));
        std::fs::write(&f, &p).unwrap();
        let ok = Command::new("plutil").arg("-lint").arg(&f).output().map(|o| o.status.success()).unwrap_or(true);
        std::fs::remove_file(&f).unwrap();
        assert!(ok, "plutil rejected:\n{p}");
        let vm = plist_with("vm", Path::new("/r"), &["start".into()], Path::new("/l"), None, true);
        assert!(vm.contains("<key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>"), "{vm}");
    }
}
