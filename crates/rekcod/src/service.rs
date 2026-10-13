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

/// A KeepAlive agent running `exe args...`, logging to `log`.
pub fn plist(name: &str, exe: &Path, args: &[String], log: &Path, rekcod_home: Option<&Path>) -> String {
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
  <key>KeepAlive</key><true/>
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
    let exe = std::env::current_exe()?.canonicalize()?;
    let dry = std::env::var_os("REKCOD_NO_LAUNCHCTL").is_some();
    let path = if dry { paths.home.join(format!("{}.plist", label(name))) } else { plist_path(name)? };
    std::fs::create_dir_all(path.parent().context("plist dir")?)?;
    let log = paths.home.join("logs").join(format!("{name}.log"));
    let home = (!paths.is_default_home()).then_some(paths.home.as_path());
    std::fs::write(&path, plist(name, &exe, args, &log, home))?;
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
    println!("{name} installed: runs now and at login, restarts if it exits; log {}", log.display());
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
        let p = plist(
            "edge",
            Path::new("/opt/r&d/rekcod"),
            &["edge".into(), "serve".into()],
            Path::new("/h/logs/edge.log"),
            Some(Path::new("/h")),
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
    }
}
