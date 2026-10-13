//! A spare Mac as a one-machine fleet that runs headless (docs/spare.md).
//!
//! `rekcod spare setup` runs on the spare; `rekcod host add --fleet` runs it over SSH from
//! your Mac and adds the remote it prints.

use std::io::{IsTerminal, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::paths::Paths;

/// `sudo pmset` arguments: no sleep, even with the lid shut; boot again after a power cut.
pub const PMSET: &[&str] = &["pmset", "-a", "sleep", "0", "disablesleep", "1", "autorestart", "1", "womp", "1"];

fn tailscale() -> String {
    which("tailscale").unwrap_or_else(|| "/Applications/Tailscale.app/Contents/MacOS/Tailscale".into())
}

fn which(bin: &str) -> Option<String> {
    let o = Command::new("/bin/zsh").args(["-lc", &format!("command -v {bin}")]).output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !s.is_empty()).then_some(s)
}

/// `Self.DNSName` from `tailscale status --json`, without the trailing dot.
pub fn dns_name(status_json: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(status_json).ok()?;
    let n = v["Self"]["DNSName"].as_str()?.trim_end_matches('.');
    (!n.is_empty()).then(|| n.to_string())
}

fn run(exe: &Path, args: &[&str]) -> Result<()> {
    let st = Command::new(exe).args(args).status().with_context(|| format!("run rekcod {}", args.join(" ")))?;
    if !st.success() {
        bail!("`rekcod {}` failed", args.join(" "));
    }
    Ok(())
}

fn gui_session() -> bool {
    let uid = unsafe { libc::getuid() };
    Command::new("/bin/launchctl")
        .args(["print", &format!("gui/{uid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn auto_login_user() -> Option<String> {
    let o = Command::new("defaults").args(["read", "/Library/Preferences/com.apple.loginwindow", "autoLoginUser"]).output().ok()?;
    let u = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !u.is_empty()).then_some(u)
}

fn power() {
    let can_sudo = std::io::stdin().is_terminal()
        || Command::new("sudo").args(["-n", "true"]).stderr(Stdio::null()).status().is_ok_and(|s| s.success());
    if !can_sudo {
        eprintln!("rekcod: skipped `sudo {}` (sudo needs a terminal); run it once so the lid can stay shut", PMSET.join(" "));
        return;
    }
    eprintln!("rekcod: sudo {}", PMSET.join(" "));
    match Command::new("sudo").args(PMSET).status() {
        Ok(s) if s.success() => eprintln!("rekcod: never sleeps, lid shut or not; boots again after a power cut"),
        _ => eprintln!("rekcod: pmset failed; the Mac sleeps when its lid shuts"),
    }
}

pub fn setup(paths: &Paths, domain: &str, addr: Option<&str>, no_power: bool) -> Result<()> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let ts = tailscale();
    let ip = match addr {
        Some(a) => a.to_string(),
        None => {
            let o = Command::new(&ts).args(["ip", "-4"]).output();
            let ip = o
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string());
            ip.filter(|i| !i.is_empty()).context("no Tailscale address; run `tailscale up`, or pass --addr IP")?
        }
    };
    if std::env::var_os("REKCOD_NO_LAUNCHCTL").is_none() && !gui_session() {
        bail!(
            "nobody is logged in on this Mac's screen, and LaunchAgents need that; turn on automatic login, `sudo reboot`, then run this again"
        );
    }
    if !no_power {
        power();
    }

    let ctl = paths.home.join("controller");
    run(&exe, &["controller", "serve", "--listen", &format!("{ip}:7700"), "--domain", domain, "--install"])?;
    let admin_file = ctl.join("admin-token");
    for _ in 0..60 {
        if std::fs::metadata(&admin_file).is_ok_and(|m| m.len() > 0) {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let admin = std::fs::read_to_string(&admin_file)
        .map(|s| s.trim().to_string())
        .map_err(|_| anyhow::anyhow!("controller didn't start; see {}", paths.home.join("logs/controller.log").display()))?;
    let url = format!("http://{ip}:7700");
    if paths.home.join("node.json").exists() {
        run(&exe, &["node", "serve", "--install"])?;
    } else {
        let join = std::fs::read_to_string(ctl.join("join-token")).context("no join token")?;
        run(&exe, &["node", "join", &url, join.trim(), "--listen", &format!("{ip}:7701"), "--install"])?;
    }
    run(&exe, &["remote", "add", "local", &url, "--token", &admin])?;
    run(
        &exe,
        &["edge", "serve", "--remote", "local", "--listen", &format!("{ip}:8080"), "--public-listen", "127.0.0.1:8081", "--install"],
    )?;

    if auto_login_user().is_none() {
        eprintln!(
            "rekcod: automatic login is off, so after a reboot nothing runs until someone logs in (System Settings › Users & Groups)"
        );
    }
    let name = Command::new(&ts).args(["status", "--json"]).output().ok().and_then(|o| dns_name(&String::from_utf8_lossy(&o.stdout)));
    let host = if addr.is_some() { ip.clone() } else { name.unwrap_or(ip.clone()) };
    eprintln!(
        "\nrekcod: this Mac is a fleet on {ip}: controller :7700, node :7701, edge :8080 (all apps), 127.0.0.1:8081 (public apps)\n\
         Public traffic, once {domain}'s DNS is on Cloudflare:\n\
         \x20 cloudflared tunnel login && cloudflared tunnel create rekcod && cloudflared tunnel route dns rekcod '*.{domain}'\n\
         \x20 # ~/.cloudflared/config.yml: tunnel: rekcod, ingress: *.{domain} -> http://127.0.0.1:8081\n\
         \x20 sudo cloudflared service install\n\
         Logs: {}/logs/{{controller,node,edge}}.log   Undo: rekcod spare uninstall\n",
        paths.home.display()
    );
    println!("remote http://{host}:7700 {admin}");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    for n in ["edge", "node", "controller"] {
        crate::service::uninstall(n)?;
    }
    Ok(())
}

/// The `remote URL TOKEN` line `spare setup` ends with.
pub fn parse_remote(line: &str) -> Option<(String, String)> {
    let mut it = line.trim().strip_prefix("remote ")?.split_whitespace();
    let (url, token) = (it.next()?, it.next()?);
    url.starts_with("http").then(|| (url.to_string(), token.to_string()))
}

const FIND: &str = "command -v rekcod || { [ -x ~/.rekcod/bin/rekcod ] && echo ~/.rekcod/bin/rekcod; }";

/// The spare's rekcod, copying this one over when it has none.
pub fn ensure_binary(dest: &str, ssh: impl Fn() -> Command, scp_dest: &[String]) -> Result<String> {
    let o = ssh().args(["/bin/zsh", "-lc", FIND]).stdin(Stdio::null()).output()?;
    if let Some(p) = String::from_utf8_lossy(&o.stdout).lines().map(str::trim).find(|l| l.starts_with('/')) {
        return Ok(p.to_string());
    }
    let exe = std::env::current_exe()?.canonicalize()?;
    eprintln!("rekcod: no rekcod on {dest}; copying {} to ~/.rekcod/bin/rekcod", exe.display());
    let ok = ssh().args(["mkdir", "-p", ".rekcod/bin"]).stdin(Stdio::null()).status()?.success();
    let (port, host) = match scp_dest {
        [p, port, host] if p == "-p" => (Some(port.clone()), host.clone()),
        [host] => (None, host.clone()),
        _ => bail!("bad destination {dest}"),
    };
    let mut scp = Command::new("scp");
    scp.args(["-q", "-o", "BatchMode=yes"]);
    if let Some(p) = port {
        scp.args(["-P", &p]);
    }
    if !ok || !scp.arg(&exe).arg(format!("{host}:.rekcod/bin/rekcod")).status()?.success() {
        bail!("copying rekcod to {dest} failed");
    }
    let o = ssh().args(["/bin/sh", "-c", "echo $HOME"]).stdin(Stdio::null()).output()?;
    Ok(format!("{}/.rekcod/bin/rekcod", String::from_utf8_lossy(&o.stdout).trim()))
}

/// Characters allowed in a value pasted into the remote shell command.
fn shell_safe(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-._:/~".contains(c))
}

/// Copies `src` to `out` as it arrives, minus the `remote URL TOKEN` line, which it returns.
/// Bytes go through before their newline so sudo's `Password:` prompt shows up.
// Unbuffered on purpose: a buffered reader would wait for more than the prompt.
#[allow(clippy::unbuffered_bytes)]
fn pass_through(src: impl Read, mut out: impl Write) -> Result<Option<(String, String)>> {
    const PREFIX: &[u8] = b"remote http";
    let (mut line, mut held, mut remote) = (Vec::new(), true, None);
    for b in src.bytes() {
        let b = b?;
        line.push(b);
        if held && !PREFIX.starts_with(&line[..line.len().min(PREFIX.len())]) {
            held = false;
            out.write_all(&line)?;
        } else if !held {
            out.write_all(&[b])?;
        }
        if b == b'\n' {
            if held {
                match parse_remote(&String::from_utf8_lossy(&line)) {
                    Some(r) => remote = Some(r),
                    None => out.write_all(&line)?,
                }
            }
            line.clear();
            held = true;
        }
        out.flush()?;
    }
    if held {
        out.write_all(&line)?;
    }
    Ok(remote)
}

/// Runs `spare setup` over SSH, streaming its output, and returns the remote it printed.
/// `ssh` should have `-t` when this terminal is one, so sudo can prompt.
pub fn remote_setup(dest: &str, mut ssh: Command, bin: &str, domain: &str, addr: Option<&str>, no_power: bool) -> Result<(String, String)> {
    if !shell_safe(domain) || addr.is_some_and(|a| !shell_safe(a)) || bin.contains('\'') {
        bail!("--domain and --addr take letters, digits and - . : only");
    }
    let mut cmd = format!("'{bin}' spare setup --domain {domain}");
    if let Some(a) = addr {
        cmd += &format!(" --addr {a}");
    }
    if no_power {
        cmd += " --no-power";
    }
    let mut child = ssh.arg(cmd).stdout(Stdio::piped()).spawn().context("run ssh")?;
    let remote = pass_through(child.stdout.take().context("ssh stdout")?, std::io::stdout())?;
    if !child.wait()?.success() {
        bail!("`rekcod spare setup` failed on {dest}");
    }
    remote.context("spare setup didn't print its remote")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_line() {
        assert_eq!(
            parse_remote("remote http://spare.tail1.ts.net:7700 abc\r"),
            Some(("http://spare.tail1.ts.net:7700".into(), "abc".into()))
        );
        assert_eq!(parse_remote("remote add local"), None);
        assert_eq!(parse_remote("rekcod: remote x"), None);
    }

    /// `sh -c` stands in for ssh: the setup's output streams through and the remote comes back.
    #[test]
    fn remote_setup_returns_remote() {
        let bin = std::env::temp_dir().join(format!("rekcod-fake-{}", std::process::id()));
        std::fs::write(&bin, "#!/bin/sh\necho \"args: $*\"\necho 'remote http://spare:7700 tok'\n").unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let mut sh = Command::new("sh");
        sh.arg("-c");
        let r = remote_setup("spare", sh, &bin.display().to_string(), "apps.test", Some("10.0.0.2"), true);
        std::fs::remove_file(&bin).unwrap();
        assert_eq!(r.unwrap(), ("http://spare:7700".into(), "tok".into()));
    }

    #[test]
    fn prompt_passes_through_before_newline() {
        let mut out = Vec::new();
        let r = pass_through(&b"Password:ok\r\nremote http://s:7700 t\r\nreminder\n"[..], &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "Password:ok\r\nreminder\n");
        assert_eq!(r, Some(("http://s:7700".into(), "t".into())));
        assert!(remote_setup("s", Command::new("true"), "/x", "a.com'; rm -rf ~", None, true).is_err());
    }

    #[test]
    fn tailnet_name() {
        assert_eq!(dns_name(r#"{"Self":{"DNSName":"spare.tail1.ts.net."}}"#).as_deref(), Some("spare.tail1.ts.net"));
        assert_eq!(dns_name(r#"{"Self":{"DNSName":""}}"#), None);
    }
}
