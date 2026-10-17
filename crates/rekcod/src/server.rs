//! This Mac as a server: a one-machine fleet that runs headless (docs/server.md).
//!
//! `rekcod server setup` runs on the server; `rekcod host add --server` runs it over SSH from
//! your Mac and adds the remote it prints.

use std::io::{IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
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

fn filevault_on() -> bool {
    Command::new("fdesetup").arg("status").output().is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("FileVault is On"))
}

/// What happens after a reboot or power cut. Daemons start once macOS is up, which with
/// FileVault on waits for someone to unlock the disk. Agents also need a login: rekcod
/// doesn't switch automatic login on itself (that takes the user's password), so it says
/// what's in the way and opens the setting.
fn after_reboot(daemons: bool, exe: &Path) {
    if daemons {
        if filevault_on() {
            eprintln!(
                "rekcod: the services start at boot, but FileVault holds a cold boot at its unlock screen until someone \
                 types the password there. For a restart you plan: `rekcod host restart NAME` from your Mac"
            );
        }
        return;
    }
    eprintln!(
        "rekcod: without sudo the services are LaunchAgents, which wait for a login after a reboot. To start them at boot \
         with nobody logged in: sudo {} server at-boot",
        exe.display()
    );
    if auto_login_user().is_some() {
        return;
    }
    let user = std::env::var("USER").unwrap_or_else(|_| "you".into());
    if filevault_on() {
        eprintln!(
            "rekcod: FileVault is on, and macOS never logs in automatically while it is. After a reboot or power cut \
             nothing runs until {user} logs in at this Mac's screen. To come back on its own: `sudo fdesetup disable` \
             (the disk is then readable by whoever holds the Mac; decrypting takes a while), then turn on automatic \
             login in System Settings › Users & Groups"
        );
        return;
    }
    eprintln!(
        "rekcod: automatic login is off, so after a reboot nothing runs until {user} logs in. Turn it on in \
         System Settings › Users & Groups › Automatic login (opened on this Mac's screen), or: \
         sudo sysadminctl -autologin set -userName {user} -password <{user}'s password>"
    );
    let _ = Command::new("open")
        .arg("x-apple.systempreferences:com.apple.Users-Groups-Settings.extension")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn can_sudo() -> bool {
    std::io::stdin().is_terminal() || Command::new("sudo").args(["-n", "true"]).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn power(paths: &Paths) {
    if !can_sudo() {
        eprintln!("rekcod: skipped `sudo {}` (sudo needs a terminal); run it once so the lid can stay shut", PMSET.join(" "));
        return;
    }
    // What `rekcod uninstall` puts back. Written once: a re-run would record rekcod's own values.
    let before = pmset_file(paths);
    if !before.exists() {
        let custom = Command::new("pmset").args(["-g", "custom"]).output().map(|o| String::from_utf8_lossy(&o.stdout).to_string());
        let g = Command::new("pmset").arg("-g").output().map(|o| String::from_utf8_lossy(&o.stdout).to_string());
        if let (Ok(c), Ok(g)) = (custom, g) {
            let undo = pmset_undo(&c, &g);
            let _ = std::fs::create_dir_all(before.parent().unwrap_or(&paths.home));
            let _ = std::fs::write(&before, serde_json::to_vec(&undo).unwrap_or_default());
        }
    }
    eprintln!("rekcod: sudo {}", PMSET.join(" "));
    match Command::new("sudo").args(PMSET).status() {
        Ok(s) if s.success() => eprintln!("rekcod: never sleeps, lid shut or not; boots again after a power cut"),
        _ => eprintln!("rekcod: pmset failed; the Mac sleeps when its lid shuts"),
    }
}

fn pmset_file(paths: &Paths) -> PathBuf {
    paths.home.join("server/pmset-before.json")
}

/// The `pmset` commands that put back what `PMSET` changes, read from `pmset -g custom`
/// (per power source: "Battery Power:", "AC Power:") and `pmset -g` (SleepDisabled is global).
fn pmset_undo(custom: &str, global: &str) -> Vec<Vec<String>> {
    let mut out = Vec::new();
    let mut flag: Option<&str> = None;
    let mut settings: Vec<String> = Vec::new();
    let mut flush = |flag: Option<&str>, settings: &mut Vec<String>| {
        if let Some(f) = flag
            && !settings.is_empty()
        {
            let mut cmd = vec!["pmset".to_string(), f.to_string()];
            cmd.append(settings);
            out.push(cmd);
        }
        settings.clear();
    };
    for line in custom.lines() {
        if !line.starts_with(' ') && line.ends_with(':') {
            flush(flag, &mut settings);
            flag = match line {
                "Battery Power:" => Some("-b"),
                "AC Power:" => Some("-c"),
                "UPS Power:" => Some("-u"),
                _ => None,
            };
            continue;
        }
        let mut it = line.split_whitespace();
        if let (Some(k @ ("sleep" | "autorestart" | "womp")), Some(v)) = (it.next(), it.next()) {
            settings.extend([k.to_string(), v.to_string()]);
        }
    }
    flush(flag, &mut settings);
    let disabled = global.lines().find_map(|l| l.trim().strip_prefix("SleepDisabled")).map(str::trim).unwrap_or("0");
    out.push(["pmset", "-a", "disablesleep", disabled].map(String::from).into());
    out
}

/// `sudo` that asks in a terminal and otherwise only works with cached credentials.
fn sudo(args: &[&str]) -> Result<()> {
    let mut cmd = Command::new("sudo");
    if !std::io::stdin().is_terminal() {
        cmd.arg("-n");
    }
    let out = cmd.args(args).stdout(Stdio::null()).output()?;
    if !out.status.success() {
        bail!("`sudo {}`: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// The `~/.zshenv` block `host add` writes, removed (the rest of the file stays).
pub fn without_zshenv_block(s: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in s.lines() {
        if line.starts_with("# >>> rekcod host") {
            skipping = true;
        } else if skipping && line.starts_with("# <<< rekcod host") {
            skipping = false;
        } else if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// What `rekcod uninstall` undoes outside `~/.rekcod` on a Mac `server setup` or `host add`
/// changed: the `~/.zshenv` block, firewall exceptions for rekcod, and pmset. Each is a
/// step `uninstall` reports.
pub fn undo_system(paths: &Paths) -> Vec<(String, Result<()>)> {
    let mut steps = Vec::new();
    if let Some(home) = std::env::var_os("HOME") {
        let f = PathBuf::from(home).join(".zshenv");
        if let Ok(s) = std::fs::read_to_string(&f)
            && s.contains("# >>> rekcod host")
        {
            steps.push((
                "remove the rekcod block from ~/.zshenv".to_string(),
                std::fs::write(&f, without_zshenv_block(&s)).map_err(Into::into),
            ));
        }
    }
    let list =
        Command::new(SOCKETFILTERFW).arg("--listapps").output().map(|o| String::from_utf8_lossy(&o.stdout).to_string()).unwrap_or_default();
    let ours: Vec<String> = list
        .lines()
        .filter_map(|l| l.trim().split_once(" : ").map(|(_, p)| p.trim().to_string()))
        .filter(|p| p.ends_with("/.rekcod/bin/rekcod") || p.ends_with("/bin/rekcod") || p.contains("/.rekcod/bin/rustfs"))
        .collect();
    for p in ours {
        steps.push((format!("remove the firewall exception for {p}"), sudo(&[SOCKETFILTERFW, "--remove", &p])));
    }
    match std::fs::read(pmset_file(paths)).ok().and_then(|b| serde_json::from_slice::<Vec<Vec<String>>>(&b).ok()) {
        Some(undo) => {
            let r = undo.iter().try_for_each(|c| sudo(&c.iter().map(String::as_str).collect::<Vec<_>>()));
            steps.push(("put pmset back as it was before `server setup`".to_string(), r));
        }
        None => {
            let disabled = Command::new("pmset").arg("-g").output().is_ok_and(|o| {
                String::from_utf8_lossy(&o.stdout).lines().any(|l| l.trim().strip_prefix("SleepDisabled").is_some_and(|v| v.trim() == "1"))
            });
            if disabled {
                steps.push((
                    "let the Mac sleep with the lid shut again (pmset disablesleep 0; sleep, womp and autorestart weren't recorded, so they stay)".to_string(),
                    sudo(&["pmset", "-a", "disablesleep", "0"]),
                ));
            }
        }
    }
    steps
}

const SOCKETFILTERFW: &str = "/usr/libexec/ApplicationFirewall/socketfilterfw";

/// `socketfilterfw` arguments that let `exe` take connections from other Macs.
fn firewall_allow(exe: &str) -> [Vec<String>; 2] {
    [vec![SOCKETFILTERFW.into(), "--add".into(), exe.into()], vec![SOCKETFILTERFW.into(), "--unblockapp".into(), exe.into()]]
}

/// Whether `--listapps` output has `exe` with "Allow incoming connections" (the line after
/// its path). `--getappblocked` is no use: it says "permitted" for apps it has never seen.
fn firewall_allows(listapps: &str, exe: &str) -> bool {
    let mut lines = listapps.lines();
    while let Some(l) = lines.next() {
        if l.trim().split_once(" : ").is_some_and(|(_, p)| p.trim() == exe) {
            return lines.next().is_some_and(|n| n.contains("Allow incoming"));
        }
    }
    false
}

/// With the application firewall on, other Macs' connections to the controller and edge
/// are accepted and then get no data back.
fn firewall(exe: &Path) {
    let out = |arg: &str| Command::new(SOCKETFILTERFW).arg(arg).output().map(|o| String::from_utf8_lossy(&o.stdout).to_string());
    if !out("--getglobalstate").is_ok_and(|s| s.contains("enabled")) {
        return;
    }
    let exe = exe.display().to_string();
    if out("--listapps").is_ok_and(|s| firewall_allows(&s, &exe)) {
        return;
    }
    let cmds = firewall_allow(&exe);
    if !can_sudo() {
        let line = cmds.iter().map(|c| format!("sudo {}", c.join(" "))).collect::<Vec<_>>().join(" && ");
        eprintln!(
            "rekcod: the firewall is on and blocks other Macs from the controller and edge; run once (sudo needs a terminal):\n  {line}"
        );
        return;
    }
    eprintln!("rekcod: the firewall is on; letting {exe} accept connections");
    for c in &cmds {
        if !Command::new("sudo").args(c).stdout(Stdio::null()).status().is_ok_and(|s| s.success()) {
            eprintln!("rekcod: `sudo {}` failed; other Macs can't reach this one", c.join(" "));
        }
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

/// Whether `ip` is on one of this Mac's interfaces.
fn assigned(ip: &str) -> bool {
    ip.parse::<std::net::IpAddr>().is_ok_and(|a| std::net::UdpSocket::bind((a, 0)).is_ok())
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
    // `tailscale ip` still prints the old address while Tailscale is stopped; listening on
    // it then fails with "Can't assign requested address".
    match ip.filter(|i| !i.is_empty() && assigned(i)) {
        Some(ip) => {
            // The tailnet DNS name goes into the controller's certificate, so remotes can use it.
            let name =
                Command::new(&ts).args(["status", "--json"]).output().ok().and_then(|o| dns_name(&String::from_utf8_lossy(&o.stdout)));
            Net::Tailnet { ip, name }
        }
        None => {
            eprintln!("rekcod: no Tailscale address up; the fleet goes on the LAN and is found over Bonjour (docs/lan-discovery.md)");
            Net::Lan
        }
    }
}

pub fn setup(paths: &Paths, domain: &str, addr: Option<&str>, lan: bool, no_power: bool) -> Result<()> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let net = pick_net(addr, lan);
    let l = layout(&net);
    let dry = std::env::var_os("REKCOD_NO_LAUNCHCTL").is_some();
    // LaunchDaemons start at boot with nobody logged in; they need root to install.
    // Without it, LaunchAgents, which need a login (docs/server.md).
    let daemons = !dry && crate::service::can_install_daemons();
    if daemons {
        // SAFETY: nothing else runs yet; the children `run` starts read it to install
        // their jobs as daemons too.
        unsafe { std::env::set_var("REKCOD_LAUNCHD", "system") };
    } else if !dry && !gui_session() {
        bail!(
            "nobody is logged in on this Mac's screen, and without sudo the services can only be LaunchAgents, which need that; run this in a terminal (sudo makes them LaunchDaemons that start at boot)"
        );
    }
    firewall(&exe);
    if !no_power {
        power(paths);
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
    // On a re-run the token and fingerprint files are already there while the controller
    // restarts on the new binary: wait until it listens.
    if std::env::var_os("REKCOD_NO_LAUNCHCTL").is_none() {
        let a: std::net::SocketAddr = crate::fleet::authority(url)?.parse().context("controller address")?;
        for _ in 0..60 {
            if std::net::TcpStream::connect_timeout(&a, Duration::from_millis(500)).is_ok() {
                break;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    }
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
        // `remote add local` ran in a child process; reading the remotes here trusts its CA.
        crate::fleet::remotes(paths);
        let v = crate::fleet::call(url, "POST", "/tokens", Some(&admin), &serde_json::json!({"name": "edge", "edge": true}))?;
        v["token"].as_str().context("no edge token")?.to_string()
    };
    run(&exe, &["remote", "add", "edge", url, "--token", &edge_token, "--ca-fingerprint", &fp])?;
    run(&exe, &["edge", "serve", "--remote", "edge", "--listen", &l.edge_listen, "--public-listen", "127.0.0.1:8081", "--install"])?;

    // The docker VM, so docker over SSH works after a reboot too.
    run(&exe, &["start", "--install"])?;
    after_reboot(daemons, &exe);
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

/// Reads a line from the terminal without echoing it.
fn read_password(prompt: &str) -> Result<String> {
    use std::io::BufRead;
    use std::os::fd::AsRawFd;
    let tty = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").context("no terminal to ask for the password")?;
    let fd = tty.as_raw_fd();
    let mut old: libc::termios = unsafe { std::mem::zeroed() };
    if unsafe { libc::tcgetattr(fd, &mut old) } != 0 {
        bail!("can't read the terminal's settings");
    }
    let mut quiet = old;
    quiet.c_lflag &= !libc::ECHO;
    let mut w = &tty;
    write!(w, "{prompt}")?;
    w.flush()?;
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &quiet) };
    let mut line = String::new();
    let r = std::io::BufReader::new(&tty).read_line(&mut line);
    unsafe { libc::tcsetattr(fd, libc::TCSANOW, &old) };
    writeln!(w)?;
    r?;
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// `fdesetup authrestart -inputplist`'s input: the account that unlocks FileVault.
fn unlock_plist(user: &str, password: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>Username</key><string>{}</string><key>Password</key><string>{}</string></dict></plist>\n",
        xml_escape(user),
        xml_escape(password)
    )
}

/// Runs sudo with `input` on stdin (`-S`: the password, or a command's input).
fn sudo_fed(args: &[&str], input: &str) -> Result<std::process::Output> {
    let mut child = Command::new("sudo").args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
    child.stdin.take().context("sudo stdin")?.write_all(input.as_bytes())?;
    Ok(child.wait_with_output()?)
}

/// `rekcod server restart`: a reboot the server comes back from on its own. With FileVault
/// on that's `fdesetup authrestart`, which unlocks the disk once; a plain restart would stop
/// at the unlock screen. It asks for the password once, checks it with sudo, and gives
/// fdesetup the same one on stdin (never a file or an argument), so macOS doesn't ask its
/// three unlabelled questions.
pub fn restart() -> Result<()> {
    let user = crate::service::RunAs::current()?.user;
    let fv = filevault_on();
    let pw = read_password(&format!("{user}'s login password on this Mac: "))?;
    if !sudo_fed(&["-S", "-p", "", "-v"], &format!("{pw}\n"))?.status.success() {
        bail!("that isn't {user}'s password here (sudo refused it); nothing restarted");
    }
    if !fv {
        eprintln!("rekcod: restarting; the services are back about a minute after it boots");
        let out = Command::new("sudo").args(["-n", "shutdown", "-r", "now"]).output()?;
        if !out.status.success() {
            bail!("shutdown -r failed: {}", String::from_utf8_lossy(&out.stderr).trim());
        }
        return Ok(());
    }
    let list = Command::new("sudo").args(["-n", "fdesetup", "list"]).output()?;
    let users = String::from_utf8_lossy(&list.stdout);
    if !users.lines().any(|l| l.split(',').next() == Some(user.as_str())) {
        bail!(
            "{user} can't unlock FileVault on this Mac, so the restart would stop at its unlock screen; accounts that can: {}",
            users.lines().filter_map(|l| l.split(',').next()).collect::<Vec<_>>().join(", ")
        );
    }
    eprintln!("rekcod: restarting with FileVault unlocked for this one boot; the services are back about a minute after it boots");
    let out = sudo_fed(&["-n", "fdesetup", "authrestart", "-inputplist"], &unlock_plist(&user, &pw))?;
    if !out.status.success() {
        bail!(
            "fdesetup authrestart failed, nothing restarted: {}{}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

/// The jobs `server setup` installs.
pub const SERVICES: &[&str] = &["storage", "controller", "node", "edge", crate::cli::AUTOSTART];

pub fn uninstall() -> Result<()> {
    for n in SERVICES.iter().rev() {
        crate::service::uninstall(n)?;
    }
    Ok(())
}

/// `rekcod server at-boot [--user U]`: a server set up without sudo (LaunchAgents, which
/// wait for a login) switched to LaunchDaemons that start at boot. Root, or sudo.
pub fn at_boot(user: Option<&str>) -> Result<()> {
    let is_root = unsafe { libc::geteuid() == 0 };
    let run_as = match user {
        Some(u) => crate::service::RunAs::named(u)?,
        None if is_root => bail!("as root, say whose server this is: --user NAME"),
        None => crate::service::RunAs::current()?,
    };
    if !is_root {
        // Re-run under sudo: root writes /Library/LaunchDaemons, and it has to read this
        // user's agents.
        let exe = std::env::current_exe()?;
        let st = Command::new("sudo").arg(&exe).args(["server", "at-boot", "--user", &run_as.user]).status()?;
        if !st.success() {
            bail!("`sudo rekcod server at-boot` failed");
        }
        return Ok(());
    }
    let moved = crate::service::agents_to_daemons(&run_as, SERVICES)?;
    if moved.is_empty() {
        println!("no rekcod LaunchAgents for {} (already daemons, or not set up: `rekcod server setup`)", run_as.user);
    } else {
        println!("{} now start at boot as {}, nobody logged in", moved.join(", "), run_as.user);
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
    eprintln!("rekcod: looking for rekcod on {dest}");
    // One quoted string: ssh joins its arguments, and the remote shell would split FIND.
    let o = ssh().arg(format!("/bin/zsh -lc '{FIND}'")).stdin(Stdio::null()).output()?;
    let found = String::from_utf8_lossy(&o.stdout).lines().map(str::trim).find(|l| l.starts_with('/')).map(String::from);
    let exe = std::env::current_exe()?.canonicalize()?;
    if let Some(p) = &found {
        // Same version: use it. Another version would speak another fleet API; ours goes
        // into ~/.rekcod/bin, which FIND prefers next time only if nothing is on PATH.
        let v = ssh().args([p.as_str(), "--version"]).stdin(Stdio::null()).output()?;
        let theirs = String::from_utf8_lossy(&v.stdout).split_whitespace().nth(1).unwrap_or("").to_string();
        // A copy rekcod made is replaced whenever it differs: dev builds all say the same
        // version. One the user installed is theirs to upgrade.
        let ours = p.ends_with("/.rekcod/bin/rekcod");
        if theirs == env!("CARGO_PKG_VERSION") && (!ours || same_file(&exe, &ssh, p)) {
            return Ok(p.clone());
        }
        if theirs == env!("CARGO_PKG_VERSION") {
            eprintln!("rekcod: {dest}'s {p} is another build of {theirs}; copying {} over it", exe.display());
            return copy_binary(dest, &exe, &ssh, scp_dest);
        }
        eprintln!(
            "rekcod: {dest} has rekcod {theirs} at {p}; copying {} ({}) to ~/.rekcod/bin/rekcod",
            exe.display(),
            env!("CARGO_PKG_VERSION")
        );
    } else {
        eprintln!("rekcod: no rekcod on {dest}; copying {} to ~/.rekcod/bin/rekcod", exe.display());
    }
    copy_binary(dest, &exe, &ssh, scp_dest)
}

/// Whether the remote file at `p` has the same sha256 as `local`.
fn same_file(local: &Path, ssh: &impl Fn() -> Command, p: &str) -> bool {
    let sum = |o: std::process::Output| crate::fetch::parse_shasum(&String::from_utf8_lossy(&o.stdout)).map(str::to_lowercase);
    let here = Command::new("shasum").args(["-a", "256"]).arg(local).output().ok().and_then(sum);
    let there = ssh().args(["shasum", "-a", "256", p]).stdin(Stdio::null()).output().ok().and_then(sum);
    here.is_some() && here == there
}

const ENTITLEMENTS: &str = include_str!("../../../rekcod.entitlements");

/// `exe`, or a signed copy of it when it lacks the virtualization entitlement (a plain
/// `cargo build` links it ad hoc without one), so the server's `rekcod start` can boot.
fn signed(exe: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
    let o = Command::new("/usr/bin/codesign").args(["-d", "--entitlements", "-"]).arg(exe).output()?;
    if String::from_utf8_lossy(&o.stdout).contains("com.apple.security.virtualization") {
        return Ok((exe.to_path_buf(), None));
    }
    let dir = std::env::temp_dir().join(format!("rekcod-sign-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let (copy, plist) = (dir.join("rekcod"), dir.join("rekcod.entitlements"));
    std::fs::copy(exe, &copy)?;
    std::fs::write(&plist, ENTITLEMENTS)?;
    let ok = Command::new("/usr/bin/codesign")
        .args(["--force", "--sign", "-", "--entitlements"])
        .arg(&plist)
        .arg(&copy)
        .stderr(Stdio::null())
        .status()?
        .success();
    if !ok {
        let _ = std::fs::remove_dir_all(&dir);
        bail!("{} has no virtualization entitlement and signing a copy failed; run scripts/build.sh", exe.display());
    }
    eprintln!("rekcod: {} isn't signed for Virtualization.framework (scripts/build.sh signs it); sending a signed copy", exe.display());
    Ok((copy, Some(dir)))
}

fn copy_binary(dest: &str, exe: &Path, ssh: &impl Fn() -> Command, scp_dest: &[String]) -> Result<String> {
    let (exe, tmp) = signed(exe)?;
    let r = copy_signed(dest, &exe, ssh, scp_dest);
    if let Some(d) = tmp {
        let _ = std::fs::remove_dir_all(d);
    }
    r
}

fn copy_signed(dest: &str, exe: &Path, ssh: &impl Fn() -> Command, scp_dest: &[String]) -> Result<String> {
    let ok = ssh().args(["mkdir", "-p", ".rekcod/bin"]).stdin(Stdio::null()).status()?.success();
    let (mut scp, host) = scp_target(dest, scp_dest)?;
    // Copied beside it and renamed over: macOS kills a process whose binary changes under
    // it, and the server's agents run this one.
    if !ok
        || !scp.arg(exe).arg(format!("{host}:.rekcod/bin/.rekcod.tmp")).status()?.success()
        || !ssh().args(["mv", "-f", ".rekcod/bin/.rekcod.tmp", ".rekcod/bin/rekcod"]).stdin(Stdio::null()).status()?.success()
    {
        bail!("copying rekcod to {dest} failed");
    }
    // One string: ssh joins its arguments, so `sh -c echo $HOME` would run a bare `echo`.
    let o = ssh().arg("echo $HOME").stdin(Stdio::null()).output()?;
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
    scp.args(["-q", "-o", "BatchMode=yes"]).args(crate::host::mux_opts());
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
    eprintln!("rekcod: running `rekcod server setup` on {dest}; sudo there asks for the password of that Mac's account");
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

    /// The test binary has no entitlements, like a plain `cargo build`: what gets sent is a
    /// signed copy that has the virtualization one.
    #[test]
    fn unsigned_binary_is_sent_signed() {
        let (copy, dir) = signed(&std::env::current_exe().unwrap()).unwrap();
        let o = Command::new("/usr/bin/codesign").args(["-d", "--entitlements", "-"]).arg(&copy).output().unwrap();
        assert!(String::from_utf8_lossy(&o.stdout).contains("com.apple.security.virtualization"));
        let (again, none) = signed(&copy).unwrap();
        assert_eq!((again, none), (copy, None));
        std::fs::remove_dir_all(dir.unwrap()).unwrap();
    }

    /// fdesetup reads the account from stdin; a password with XML in it stays one string.
    /// What uninstall puts back, per power source, from `pmset -g custom` as a MacBook prints it.
    #[test]
    fn pmset_undo_per_source() {
        let custom = "Battery Power:\n Sleep On Power Button 1\n womp                 0\n sleep                1\n displaysleep         2\nAC Power:\n womp                 1\n sleep                10\n autorestart          0\n";
        let undo = pmset_undo(custom, " SleepDisabled\t\t0\n");
        assert_eq!(
            undo,
            [
                vec!["pmset", "-b", "womp", "0", "sleep", "1"],
                vec!["pmset", "-c", "womp", "1", "sleep", "10", "autorestart", "0"],
                vec!["pmset", "-a", "disablesleep", "0"],
            ]
        );
    }

    #[test]
    fn zshenv_block_removed_rest_kept() {
        let s = "export A=1\n# >>> rekcod host (written by rekcod host add)\nexport PATH=x\n# <<< rekcod host\nexport B=2\n";
        assert_eq!(without_zshenv_block(s), "export A=1\nexport B=2\n");
        assert_eq!(without_zshenv_block("export A=1\n"), "export A=1\n");
    }

    #[test]
    fn unlock_plist_escapes() {
        let p = unlock_plist("tim", "a<b&c\"d");
        assert!(p.contains("<key>Username</key><string>tim</string>"));
        assert!(p.contains("<key>Password</key><string>a&lt;b&amp;c&quot;d</string>"), "{p}");
        let f = std::env::temp_dir().join(format!("rekcod-unlock-{}.plist", std::process::id()));
        std::fs::write(&f, &p).unwrap();
        let ok = Command::new("plutil").arg("-lint").arg(&f).output().unwrap().status.success();
        std::fs::remove_file(&f).unwrap();
        assert!(ok);
    }

    #[test]
    fn firewall_list() {
        let list = "Total number of apps = 2 \n1 : /Users/me/.rekcod/bin/rekcod \n             (Allow incoming connections)\n2 : /Applications/X.app \n             (Block incoming connections)\n";
        assert!(firewall_allows(list, "/Users/me/.rekcod/bin/rekcod"));
        assert!(!firewall_allows(list, "/Applications/X.app"));
        assert!(!firewall_allows(list, "/usr/local/bin/rekcod"));
    }

    #[test]
    fn only_local_addresses_are_assigned() {
        assert!(assigned("127.0.0.1"));
        assert!(!assigned("192.0.2.1"));
        assert!(!assigned("not an ip"));
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
