//! This Mac as a server: a one-machine fleet that runs headless (docs/server.md).
//!
//! `rekcod server setup` runs on the server; `rekcod host add --server` runs it over SSH from
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
    let st = Command::new(exe).args(args).status().with_context(|| format!("run rekcod {}", redacted(args)))?;
    if !st.success() {
        bail!("`rekcod {}` failed", redacted(args));
    }
    Ok(())
}

/// `args` for an error message, with tokens hidden (the value after `--token`, and
/// `node join`'s join token): errors end up in install logs other accounts can read.
fn redacted(args: &[&str]) -> String {
    let join = args.starts_with(&["node", "join"]);
    let mut out = Vec::with_capacity(args.len());
    for (i, a) in args.iter().enumerate() {
        out.push(if (i > 0 && args[i - 1] == "--token") || (join && i == 3) { "***" } else { a });
    }
    out.join(" ")
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

/// How the server is addressed: a fixed IP, its tailnet, or nothing fixed (Bonjour).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Net {
    Fixed(String),
    Tailnet { ip: String, name: Option<String> },
    Lan,
}

/// Where each service listens and how the others reach it.
#[derive(Debug, PartialEq, Eq)]
pub struct Layout {
    pub storage_listen: String,
    pub storage_url: String,
    pub controller_listen: String,
    /// The certificate name remotes use (the Bonjour name is added by the controller).
    pub tls_name: Option<String>,
    pub bonjour: bool,
    /// The controller as the node and edge on this Mac reach it.
    pub controller_url: String,
    pub node_listen: String,
    pub edge_listen: String,
}

pub fn layout(net: &Net) -> Layout {
    match net {
        // Nothing on this Mac may depend on its LAN address: everything local goes over
        // loopback, and only the controller and edge listen on every interface.
        Net::Lan => Layout {
            storage_listen: "127.0.0.1:7710".into(),
            storage_url: "http://127.0.0.1:7710".into(),
            controller_listen: "0.0.0.0:7700".into(),
            tls_name: None,
            bonjour: true,
            controller_url: "https://127.0.0.1:7700".into(),
            node_listen: "127.0.0.1:7701".into(),
            edge_listen: "0.0.0.0:8080".into(),
        },
        Net::Fixed(ip) | Net::Tailnet { ip, .. } => {
            let host = match net {
                Net::Tailnet { name: Some(n), .. } => n.clone(),
                _ => ip.clone(),
            };
            Layout {
                storage_listen: format!("{ip}:7710"),
                storage_url: format!("http://{ip}:7710"),
                controller_listen: format!("{ip}:7700"),
                tls_name: Some(host),
                bonjour: false,
                controller_url: format!("https://{ip}:7700"),
                node_listen: format!("{ip}:7701"),
                edge_listen: format!("{ip}:8080"),
            }
        }
    }
}

/// `--addr`, else the Tailscale address, else (or with `--lan`) Bonjour.
fn pick_net(addr: Option<&str>, lan: bool) -> Net {
    if let Some(a) = addr {
        return Net::Fixed(a.to_string());
    }
    if lan {
        return Net::Lan;
    }
    let ts = tailscale();
    let o = Command::new(&ts).args(["ip", "-4"]).output();
    let ip =
        o.ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).lines().next().unwrap_or("").trim().to_string());
    match ip.filter(|i| !i.is_empty()) {
        Some(ip) => {
            // The tailnet DNS name goes into the controller's certificate, so remotes can use it.
            let name =
                Command::new(&ts).args(["status", "--json"]).output().ok().and_then(|o| dns_name(&String::from_utf8_lossy(&o.stdout)));
            Net::Tailnet { ip, name }
        }
        None => {
            eprintln!("rekcod: no Tailscale address; the fleet goes on the LAN and is found over Bonjour (docs/lan-discovery.md)");
            Net::Lan
        }
    }
}

pub fn setup(paths: &Paths, domain: &str, addr: Option<&str>, lan: bool, no_power: bool) -> Result<()> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let net = pick_net(addr, lan);
    let l = layout(&net);
    if std::env::var_os("REKCOD_NO_LAUNCHCTL").is_none() && !gui_session() {
        bail!(
            "nobody is logged in on this Mac's screen, and LaunchAgents need that; turn on automatic login, `sudo reboot`, then run this again"
        );
    }
    if !no_power {
        power();
    }

    let ctl = paths.home.join("controller");
    // RustFS first: the controller reads its keys on start.
    run(&exe, &["storage", "serve", "--listen", &l.storage_listen, "--install"])?;
    let creds = crate::storage::credentials_file(paths);
    for _ in 0..240 {
        if creds.exists() || std::env::var_os("REKCOD_NO_LAUNCHCTL").is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let mut args =
        vec!["controller", "serve", "--listen", &l.controller_listen, "--domain", domain, "--storage", &l.storage_url, "--auto-approve"];
    if let Some(n) = &l.tls_name {
        args.extend(["--tls-name", n]);
    }
    if l.bonjour {
        args.push("--bonjour");
    }
    args.push("--install");
    run(&exe, &args)?;
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
    let fp_file = ctl.join("ca-fingerprint");
    for _ in 0..60 {
        if fp_file.exists() || std::env::var_os("REKCOD_NO_LAUNCHCTL").is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    let fp = match std::fs::read_to_string(&fp_file) {
        Ok(f) => f.trim().to_string(),
        Err(_) if std::env::var_os("REKCOD_NO_LAUNCHCTL").is_some() => "sha256:dry-run".into(),
        Err(_) => bail!("the controller wrote no CA fingerprint; see {}", paths.home.join("logs/controller.log").display()),
    };
    let url = &l.controller_url;
    run(&exe, &["remote", "add", "local", url, "--token", &admin, "--ca-fingerprint", &fp])?;
    // A node that joined on another address (the LAN IP it had, a switch to or from --lan)
    // would serve its old one forever: forget it and join again.
    let node_file = paths.home.join("node.json");
    let joined = std::fs::read(&node_file).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    if let Some(j) =
        joined.as_ref().filter(|j| j["listen"].as_str() != Some(l.node_listen.as_str()) || j["controller"].as_str() != Some(url.as_str()))
    {
        eprintln!("rekcod: the node joined as {}; joining again on {}", j["listen"].as_str().unwrap_or("?"), l.node_listen);
        crate::service::uninstall("node")?;
        if let Some(id) = j["id"].as_str() {
            let _ = run(&exe, &["node", "rm", id, "--remote", "local"]);
        }
        std::fs::remove_file(&node_file)?;
    }
    if node_file.exists() {
        run(&exe, &["node", "serve", "--install"])?;
    } else {
        let join = std::fs::read_to_string(ctl.join("join-token")).context("no join token")?;
        run(&exe, &["node", "join", url, join.trim(), "--listen", &l.node_listen, "--ca-fingerprint", &fp, "--install"])?;
    }
    // The edge gets a token that can only read routes and bans: taking over whatever runs
    // the edge shouldn't mean owning the fleet.
    let edge_token = if std::env::var_os("REKCOD_NO_LAUNCHCTL").is_some() {
        "dry-run".to_string()
    } else {
        let v = crate::fleet::call(url, "POST", "/tokens", Some(&admin), &serde_json::json!({"name": "edge", "edge": true}))?;
        v["token"].as_str().context("no edge token")?.to_string()
    };
    run(&exe, &["remote", "add", "edge", url, "--token", &edge_token, "--ca-fingerprint", &fp])?;
    run(&exe, &["edge", "serve", "--remote", "edge", "--listen", &l.edge_listen, "--public-listen", "127.0.0.1:8081", "--install"])?;

    if auto_login_user().is_none() {
        eprintln!(
            "rekcod: automatic login is off, so after a reboot nothing runs until someone logs in (System Settings › Users & Groups)"
        );
    }
    let (on, remote) = match &net {
        Net::Lan => {
            let i = crate::bonjour::instance(&fp);
            (format!("the LAN as {i} (Bonjour)"), format!("https://{i}{}", crate::bonjour::SUFFIX))
        }
        _ => (
            l.controller_listen.trim_end_matches(":7700").to_string(),
            format!("https://{}:7700", l.tls_name.as_deref().unwrap_or_default()),
        ),
    };
    eprintln!(
        "\nrekcod: this Mac is a fleet on {on}: controller :7700, node :7701, RustFS :7710, edge :8080 (all apps), 127.0.0.1:8081 (public apps)\n\
         Public traffic, once {domain}'s DNS is on Cloudflare:\n\
         \x20 cloudflared tunnel login && cloudflared tunnel create rekcod && cloudflared tunnel route dns rekcod '*.{domain}'\n\
         \x20 # ~/.cloudflared/config.yml: tunnel: rekcod, ingress: *.{domain} -> http://127.0.0.1:8081\n\
         \x20 sudo cloudflared service install\n\
         Logs: {}/logs/{{controller,node,edge,storage}}.log   Undo: rekcod server uninstall\n",
        paths.home.display()
    );
    println!("remote {remote} {admin} {fp}");
    Ok(())
}

pub fn uninstall() -> Result<()> {
    for n in ["edge", "node", "controller", "storage"] {
        crate::service::uninstall(n)?;
    }
    Ok(())
}

/// `remote URL TOKEN [CA-FINGERPRINT]`, the last line `server setup` prints.
pub fn parse_remote(line: &str) -> Option<(String, String, Option<String>)> {
    let mut it = line.trim().strip_prefix("remote ")?.split_whitespace();
    let (url, token) = (it.next()?, it.next()?);
    let fp = it.next().filter(|f| f.starts_with("sha256:")).map(String::from);
    url.starts_with("http").then(|| (url.to_string(), token.to_string(), fp))
}

const FIND: &str = "command -v rekcod || { [ -x ~/.rekcod/bin/rekcod ] && echo ~/.rekcod/bin/rekcod; }";

/// The server's rekcod, copying this one over when it has none.
pub fn ensure_binary(dest: &str, ssh: impl Fn() -> Command, scp_dest: &[String]) -> Result<String> {
    let o = ssh().args(["/bin/zsh", "-lc", FIND]).stdin(Stdio::null()).output()?;
    let found = String::from_utf8_lossy(&o.stdout).lines().map(str::trim).find(|l| l.starts_with('/')).map(String::from);
    let exe = std::env::current_exe()?.canonicalize()?;
    if let Some(p) = &found {
        // Same version: use it. Another version would speak another fleet API; ours goes
        // into ~/.rekcod/bin, which FIND prefers next time only if nothing is on PATH.
        let v = ssh().args([p.as_str(), "--version"]).stdin(Stdio::null()).output()?;
        let theirs = String::from_utf8_lossy(&v.stdout).split_whitespace().nth(1).unwrap_or("").to_string();
        if theirs == env!("CARGO_PKG_VERSION") {
            return Ok(p.clone());
        }
        eprintln!(
            "rekcod: {dest} has rekcod {theirs} at {p}; copying {} ({}) to ~/.rekcod/bin/rekcod",
            exe.display(),
            env!("CARGO_PKG_VERSION")
        );
    } else {
        eprintln!("rekcod: no rekcod on {dest}; copying {} to ~/.rekcod/bin/rekcod", exe.display());
    }
    let ok = ssh().args(["mkdir", "-p", ".rekcod/bin"]).stdin(Stdio::null()).status()?.success();
    let (mut scp, host) = scp_target(dest, scp_dest)?;
    // Copied beside it and renamed over: macOS kills a process whose binary changes under
    // it, and the server's agents run this one.
    if !ok
        || !scp.arg(&exe).arg(format!("{host}:.rekcod/bin/.rekcod.tmp")).status()?.success()
        || !ssh().args(["mv", "-f", ".rekcod/bin/.rekcod.tmp", ".rekcod/bin/rekcod"]).stdin(Stdio::null()).status()?.success()
    {
        bail!("copying rekcod to {dest} failed");
    }
    let o = ssh().args(["/bin/sh", "-c", "echo $HOME"]).stdin(Stdio::null()).output()?;
    Ok(format!("{}/.rekcod/bin/rekcod", String::from_utf8_lossy(&o.stdout).trim()))
}

/// `scp` arguments for `scp_dest` (`[host]` or `["-p", port, host]`): the options and the
/// `host:` prefix remote paths go after.
fn scp_target(dest: &str, scp_dest: &[String]) -> Result<(Command, String)> {
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
    Ok((scp, host))
}

/// A dev build pins no guest, so the server's `rekcod start` would find none: copy this
/// Mac's to its `~/.rekcod/guest` unless the `VERSION` there matches. Release builds
/// download theirs on first start.
pub fn ensure_guest(paths: &Paths, dest: &str, ssh: impl Fn() -> Command, scp_dest: &[String]) -> Result<()> {
    if crate::fetch::GUEST_VERSION.is_some() {
        return Ok(());
    }
    let local = paths.guest_dir().context("this dev build has no guest to copy to the server; scripts/build.sh builds one")?;
    let ours = std::fs::read_to_string(local.join("VERSION")).unwrap_or_default().trim().to_string();
    let o = ssh().args(["cat", ".rekcod/guest/VERSION"]).stdin(Stdio::null()).stderr(Stdio::null()).output()?;
    if !ours.is_empty() && String::from_utf8_lossy(&o.stdout).trim() == ours {
        return Ok(());
    }
    let files: Vec<&str> = ["Image", "initramfs", "root.img", "VERSION"].into_iter().filter(|f| local.join(f).exists()).collect();
    let size: u64 = files.iter().filter_map(|f| std::fs::metadata(local.join(f)).ok()).map(|m| m.len()).sum();
    eprintln!("rekcod: copying this dev build's guest {ours} ({} MB) to {dest}:~/.rekcod/guest", size >> 20);
    if !ssh().args(["mkdir", "-p", ".rekcod/guest"]).stdin(Stdio::null()).status()?.success() {
        bail!("mkdir ~/.rekcod/guest on {dest} failed");
    }
    // Under temporary names, then renamed with VERSION last: an interrupted copy is copied
    // again next time instead of booting half a guest.
    for f in &files {
        let (mut scp, host) = scp_target(dest, scp_dest)?;
        if !scp.arg(local.join(f)).arg(format!("{host}:.rekcod/guest/.{f}.tmp")).status()?.success() {
            bail!("copying the guest's {f} to {dest} failed");
        }
    }
    let mv = files.iter().map(|f| format!("mv -f .{f}.tmp {f}")).collect::<Vec<_>>().join(" && ");
    if !ssh().arg(format!("cd .rekcod/guest && {mv}")).stdin(Stdio::null()).status()?.success() {
        bail!("installing the guest on {dest} failed");
    }
    Ok(())
}

/// Characters allowed in a value pasted into the remote shell command.
fn shell_safe(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-._:/~".contains(c))
}

/// Copies `src` to `out` as it arrives, minus the `remote URL TOKEN` line, which it returns.
/// Bytes go through before their newline so sudo's `Password:` prompt shows up.
// Unbuffered on purpose: a buffered reader would wait for more than the prompt.
#[allow(clippy::unbuffered_bytes)]
fn pass_through(src: impl Read, mut out: impl Write) -> Result<Option<(String, String, Option<String>)>> {
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

/// Runs `server setup` over SSH, streaming its output, and returns the remote it printed.
/// `ssh` should have `-t` when this terminal is one, so sudo can prompt.
pub fn remote_setup(
    dest: &str,
    mut ssh: Command,
    bin: &str,
    domain: &str,
    addr: Option<&str>,
    lan: bool,
    no_power: bool,
) -> Result<(String, String, Option<String>)> {
    if !shell_safe(domain) || addr.is_some_and(|a| !shell_safe(a)) || bin.contains('\'') {
        bail!("--domain and --addr take letters, digits and - . : only");
    }
    let mut cmd = format!("'{bin}' server setup --domain {domain}");
    if let Some(a) = addr {
        cmd += &format!(" --addr {a}");
    }
    if lan {
        cmd += " --lan";
    }
    if no_power {
        cmd += " --no-power";
    }
    let mut child = ssh.arg(cmd).stdout(Stdio::piped()).spawn().context("run ssh")?;
    let remote = pass_through(child.stdout.take().context("ssh stdout")?, std::io::stdout())?;
    if !child.wait()?.success() {
        bail!("`rekcod server setup` failed on {dest}");
    }
    remote.context("server setup didn't print its remote")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On a LAN nothing listens on, or points at, an address that can change.
    #[test]
    fn lan_layout_uses_no_lan_address() {
        let l = layout(&Net::Lan);
        assert!(l.bonjour && l.tls_name.is_none());
        assert_eq!(l.controller_listen, "0.0.0.0:7700");
        assert_eq!(l.edge_listen, "0.0.0.0:8080");
        for a in [&l.storage_listen, &l.storage_url, &l.controller_url, &l.node_listen] {
            assert!(a.contains("127.0.0.1"), "{a}");
        }
        let t = layout(&Net::Tailnet { ip: "100.64.0.9".into(), name: Some("mini.tail1.ts.net".into()) });
        assert_eq!(t.tls_name.as_deref(), Some("mini.tail1.ts.net"));
        assert_eq!((t.node_listen.as_str(), t.bonjour), ("100.64.0.9:7701", false));
        assert_eq!(layout(&Net::Fixed("10.0.0.4".into())).tls_name.as_deref(), Some("10.0.0.4"));
    }

    #[test]
    fn errors_hide_tokens() {
        assert_eq!(
            redacted(&["remote", "add", "local", "https://x", "--token", "secret", "--ca-fingerprint", "sha256:ab"]),
            "remote add local https://x --token *** --ca-fingerprint sha256:ab"
        );
        assert_eq!(redacted(&["node", "join", "https://x", "jtok", "--listen", "a"]), "node join https://x *** --listen a");
    }

    #[test]
    fn remote_line() {
        assert_eq!(
            parse_remote("remote http://mini.tail1.ts.net:7700 abc\r"),
            Some(("http://mini.tail1.ts.net:7700".into(), "abc".into(), None))
        );
        assert_eq!(parse_remote("remote add local"), None);
        assert_eq!(
            parse_remote("remote https://s:7700 tok sha256:ab"),
            Some(("https://s:7700".into(), "tok".into(), Some("sha256:ab".into())))
        );
        assert_eq!(parse_remote("rekcod: remote x"), None);
    }

    /// `sh -c` stands in for ssh: the setup's output streams through and the remote comes back.
    #[test]
    fn remote_setup_returns_remote() {
        let bin = std::env::temp_dir().join(format!("rekcod-fake-{}", std::process::id()));
        std::fs::write(&bin, "#!/bin/sh\necho \"args: $*\"\necho 'remote http://mini:7700 tok'\n").unwrap();
        std::fs::set_permissions(&bin, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let mut sh = Command::new("sh");
        sh.arg("-c");
        let r = remote_setup("mini", sh, &bin.display().to_string(), "apps.test", Some("10.0.0.2"), false, true);
        std::fs::remove_file(&bin).unwrap();
        assert_eq!(r.unwrap(), ("http://mini:7700".into(), "tok".into(), None));
    }

    #[test]
    fn prompt_passes_through_before_newline() {
        let mut out = Vec::new();
        let r = pass_through(&b"Password:ok\r\nremote http://s:7700 t\r\nreminder\n"[..], &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "Password:ok\r\nreminder\n");
        assert_eq!(r, Some(("http://s:7700".into(), "t".into(), None)));
        assert!(remote_setup("s", Command::new("true"), "/x", "a.com'; rm -rf ~", None, false, true).is_err());
    }

    #[test]
    fn tailnet_name() {
        assert_eq!(dns_name(r#"{"Self":{"DNSName":"mini.tail1.ts.net."}}"#).as_deref(), Some("mini.tail1.ts.net"));
        assert_eq!(dns_name(r#"{"Self":{"DNSName":""}}"#), None);
    }
}
