//! Another Mac's rekcod as the docker engine, over SSH (docs/remote-docker.md).
//!
//! A host is the docker context `rekcod-<name>` with `host=ssh://<dest>`; nothing else is stored.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{Context, Result, bail};

use crate::docker::{self, CONTEXT};
use crate::paths::Paths;

const PREFIX: &str = "rekcod-";

/// Runs on the spare in a login shell (so Homebrew's PATH is there) and writes the
/// `~/.zshenv` block that docker's non-interactive `ssh ... docker system dial-stdio` reads.
const SETUP: &str = r#"
case $SHELL in */zsh) ;; *) echo "shell:$SHELL"; exit 3;; esac
d=$(command -v docker)
[[ -z $d ]] && d=$(ls -d ~/.rekcod/cli/*/docker 2>/dev/null | tail -1)
[[ -n $d ]] || { echo nodocker; exit 4; }
f=~/.zshenv
touch $f
sed -i '' '/^# >>> rekcod host/,/^# <<< rekcod host/d' $f
cat >> $f <<EOF
# >>> rekcod host (written by rekcod host add): docker over SSH reaches rekcod
if [[ -n \$SSH_CONNECTION && ! -o interactive ]]; then
  export PATH="${d:h}:\$PATH"
  export DOCKER_HOST=unix://\$HOME/.rekcod/run/docker.sock
fi
# <<< rekcod host
EOF
echo "docker:$d"
"#;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Host {
    pub name: String,
    pub endpoint: String,
    pub current: bool,
    /// A LAN spare, kept in the context's description.
    pub bonjour: Option<Lan>,
}

/// How a LAN spare is found again (Bonjour) and recognised (its SSH host key, pinned when
/// it was added; the CA fingerprint is public, so it can't be the proof).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lan {
    pub instance: String,
    pub fp: String,
    /// `ssh-ed25519 AAAA...`
    pub host_key: String,
}

const DESCRIPTION: &str = "rekcod host";

fn description(lan: Option<&Lan>) -> String {
    match lan {
        Some(l) => format!("{DESCRIPTION} bonjour={} fp={} hostkey={}", l.instance, l.fp, l.host_key.replacen(' ', ":", 1)),
        None => DESCRIPTION.into(),
    }
}

fn parse_description(d: &str) -> Option<Lan> {
    let field = |k: &str| d.split_whitespace().find_map(|w| w.strip_prefix(k)).map(String::from);
    let host_key = field("hostkey=")?.replacen(':', " ", 1);
    host_key.contains(' ').then_some(())?;
    Some(Lan { instance: field("bonjour=")?, fp: field("fp=")?, host_key })
}

/// `user@host[:port]` as it appears in known_hosts: `host` or `[host]:port`.
fn known_hosts_name(dest: &str) -> String {
    let rest = dest.split_once('@').map_or(dest, |(_, r)| r);
    match rest.rsplit_once(':') {
        Some((h, p)) if p.parse::<u16>().is_ok() && p != "22" => format!("[{h}]:{p}"),
        Some((h, p)) if p.parse::<u16>().is_ok() => h.to_string(),
        _ => rest.to_string(),
    }
}

/// The spare's ed25519 host key, read over the SSH session `add` just authenticated.
fn host_key(dest: &str) -> Result<String> {
    let out = ssh(dest).arg("cat /etc/ssh/ssh_host_ed25519_key.pub").stdin(Stdio::null()).output().context("run ssh")?;
    let line = String::from_utf8_lossy(&out.stdout);
    let mut it = line.split_whitespace();
    match (it.next(), it.next()) {
        (Some(t @ "ssh-ed25519"), Some(k)) if out.status.success() => Ok(format!("{t} {k}")),
        _ => bail!("can't read {dest}'s SSH host key (/etc/ssh/ssh_host_ed25519_key.pub)"),
    }
}

fn context(name: &str) -> String {
    format!("{PREFIX}{name}")
}

/// `user@host[:port]` (as in an `ssh://` URL) as ssh arguments.
fn ssh_dest(dest: &str) -> Vec<String> {
    match dest.rsplit_once(':') {
        Some((host, port)) if port.parse::<u16>().is_ok() => vec!["-p".into(), port.into(), host.into()],
        _ => vec![dest.into()],
    }
}

fn ssh(dest: &str) -> Command {
    ssh_opts(dest, &[])
}

fn ssh_opts(dest: &str, extra: &[&str]) -> Command {
    let mut cmd = Command::new("ssh");
    cmd.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "StrictHostKeyChecking=accept-new"]).args(extra).args(ssh_dest(dest));
    cmd
}

/// Hosts from `docker context ls` output (one JSON object per line).
pub fn parse_hosts(raw: &str) -> (Vec<Host>, bool) {
    let mut hosts = Vec::new();
    let mut local_current = false;
    for line in raw.lines() {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let name = v["Name"].as_str().unwrap_or_default();
        let current = v["Current"] == true;
        if name == CONTEXT {
            local_current = current;
        } else if let Some(n) = name.strip_prefix(PREFIX) {
            hosts.push(Host {
                name: n.into(),
                endpoint: v["DockerEndpoint"].as_str().unwrap_or_default().into(),
                current,
                bonjour: parse_description(v["Description"].as_str().unwrap_or_default()),
            });
        }
    }
    (hosts, local_current)
}

/// The hosts, and whether the local `rekcod` context is the current one.
pub fn list() -> (Vec<Host>, bool) {
    match docker::run(&["context", "ls", "--format", "{{json .}}"]) {
        Ok(o) if o.status.success() => parse_hosts(&String::from_utf8_lossy(&o.stdout)),
        _ => (Vec::new(), false),
    }
}

/// `host add --fleet`: what to set up on the spare besides docker.
pub struct Fleet {
    pub domain: String,
    pub addr: Option<String>,
    pub lan: bool,
    pub no_power: bool,
    pub docker: bool,
}

fn check(name: &str, dest: &str) -> Result<()> {
    if name == "local" || name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        bail!("host name must be letters, digits, - or _ (and not `local`)");
    }
    let ok = ssh(dest).arg("true").stdin(Stdio::null()).status().context("run ssh")?.success();
    if !ok {
        bail!("can't SSH to {dest} without a password; run `ssh-copy-id {dest}` and turn on Remote Login on that Mac");
    }
    Ok(())
}

/// Provisions the spare as a fleet (docs/spare.md), adds it as remote NAME, then as a docker host.
pub fn add_fleet(paths: &Paths, name: &str, dest: &str, switch: bool, f: &Fleet) -> Result<()> {
    let dest = dest.strip_prefix("ssh://").unwrap_or(dest);
    check(name, dest)?;
    let bin = crate::spare::ensure_binary(dest, || ssh(dest), &ssh_dest(dest))?;
    let tty: &[&str] = if std::io::IsTerminal::is_terminal(&std::io::stdin()) { &["-t"] } else { &[] };
    let (url, token, fp) = crate::spare::remote_setup(dest, ssh_opts(dest, tty), &bin, &f.domain, f.addr.as_deref(), f.lan, f.no_power)?;
    crate::fleet::remote_add(paths, name, &url, &token, None, fp.as_deref())?;
    println!("remote {name}: {url}  (rekcod app deploy --remote {name})");
    if !f.docker {
        return Ok(());
    }
    // The docker socket only exists while the spare's VM runs.
    if !ssh(dest).arg(format!("'{bin}' start")).stdin(Stdio::null()).status()?.success() {
        bail!("`rekcod start` failed on {dest}; the fleet is set up, docker isn't (re-run with --no-docker to skip it)");
    }
    let authority = crate::fleet::authority(&url)?;
    let lan = match (crate::bonjour::instance_of(&authority), fp) {
        (Some(i), Some(fp)) => Some(Lan { instance: i.into(), fp, host_key: host_key(dest)? }),
        _ => None,
    };
    add_described(name, dest, switch, &description(lan.as_ref()))
}

pub fn add(name: &str, dest: &str, switch: bool) -> Result<()> {
    add_described(name, dest, switch, DESCRIPTION)
}

fn add_described(name: &str, dest: &str, switch: bool, description: &str) -> Result<()> {
    let dest = dest.strip_prefix("ssh://").unwrap_or(dest);
    check(name, dest)?;
    let mut child = ssh(dest).args(["/bin/zsh", "-l", "-s"]).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn()?;
    child.stdin.take().context("ssh stdin")?.write_all(SETUP.as_bytes())?;
    let out = child.wait_with_output()?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    match out.status.code() {
        Some(0) => {
            let d = stdout.lines().find_map(|l| l.strip_prefix("docker:")).unwrap_or("docker");
            println!("{dest}: ~/.zshenv sends docker over SSH to rekcod (docker CLI: {d})");
        }
        Some(3) => bail!(
            "{dest}'s login shell is {}, not zsh; add to the file it reads for non-interactive SSH:\n  export PATH=<dir of docker>:$PATH\n  export DOCKER_HOST=unix://$HOME/.rekcod/run/docker.sock",
            stdout.trim().trim_start_matches("shell:")
        ),
        Some(4) => bail!("no docker CLI on {dest}; run `rekcod start` there once (it downloads one if needed)"),
        _ => bail!("setting up {dest} failed: {}", stdout.trim()),
    }
    let ctx = context(name);
    let endpoint = format!("host=ssh://{dest}");
    if docker::context_exists(&ctx) {
        docker::run_ok(&["context", "update", &ctx, "--description", description, "--docker", &endpoint])?;
    } else {
        docker::run_ok(&["context", "create", &ctx, "--description", description, "--docker", &endpoint])?;
    }
    let version = docker::run_ok(&["--context", &ctx, "version", "--format", "{{.Server.Version}}"])
        .map_err(|e| anyhow::anyhow!("{e}\nis rekcod running on {dest}? run `rekcod start` there"))?;
    println!("docker context {ctx}: engine {version} on {dest}");
    if switch {
        use_host(name)?;
    }
    Ok(())
}

/// `user@host[:port]` with the host swapped for `target`.
fn retarget(dest: &str, target: &str) -> String {
    let (user, rest) = dest.split_once('@').map_or((None, dest), |(u, r)| (Some(u), r));
    let port = rest.rsplit_once(':').map(|(_, p)| p).filter(|p| p.parse::<u16>().is_ok());
    let mut out = user.map(|u| format!("{u}@")).unwrap_or_default() + target;
    if let Some(p) = port {
        out += &format!(":{p}");
    }
    out
}

/// A LAN spare's docker context pointed at wherever its Bonjour instance is now, after
/// proving with the SSH host key pinned at `add` that the Mac there is the same one.
/// Hosts without one are left alone.
pub fn refresh(name: &str) -> Result<()> {
    let (hosts, _) = list();
    let host = hosts.iter().find(|h| h.name == name).with_context(|| format!("no host {name}; `rekcod host ls`"))?;
    let Some(lan) = &host.bonjour else { return Ok(()) };
    let dest = host.endpoint.strip_prefix("ssh://").with_context(|| format!("{} isn't an ssh:// host", host.endpoint))?;
    let (target, _) = crate::bonjour::resolve(&lan.instance, Duration::from_secs(3))?;
    let new = retarget(dest, &target);
    if new == dest {
        return Ok(());
    }
    // Anyone on the LAN can answer for a Bonjour name, so the answer only counts once the
    // SSH server there proves it holds the pinned key: a known_hosts with nothing else in it.
    let known = tempfile_known_hosts(&known_hosts_name(&new), &lan.host_key)?;
    let ok = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "StrictHostKeyChecking=yes"])
        .args(["-o", &format!("UserKnownHostsFile={}", known.display()), "-o", "GlobalKnownHostsFile=/dev/null"])
        .args(ssh_dest(&new))
        .arg("true")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("run ssh")?
        .success();
    let _ = std::fs::remove_file(&known);
    if !ok {
        bail!("{new} answered for {name} on Bonjour but doesn't have its SSH host key; leaving {name} on {dest}");
    }
    trust_host_key(&known_hosts_name(&new), &lan.host_key)?;
    docker::run_ok(&["context", "update", &context(name), "--docker", &format!("host=ssh://{new}")])?;
    println!("host {name}: now {new}");
    Ok(())
}

fn tempfile_known_hosts(host: &str, key: &str) -> Result<std::path::PathBuf> {
    let p = std::env::temp_dir().join(format!("rekcod-known-{}-{}", std::process::id(), crate::sandbox::new_id()?));
    std::fs::write(&p, format!("{host} {key}\n"))?;
    Ok(p)
}

/// docker's own ssh reads ~/.ssh/known_hosts and can't answer a prompt: give it the
/// verified key for the new name, replacing whatever that name had.
fn trust_host_key(host: &str, key: &str) -> Result<()> {
    let _ = Command::new("ssh-keygen").args(["-R", host]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    let dir = std::path::PathBuf::from(std::env::var_os("HOME").context("HOME not set")?).join(".ssh");
    std::fs::create_dir_all(&dir)?;
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("known_hosts"))?;
    writeln!(f, "{host} {key}")?;
    Ok(())
}

pub fn use_host(name: &str) -> Result<()> {
    if name != "local"
        && let Err(e) = refresh(name)
    {
        eprintln!("rekcod: {e:#}");
    }
    let ctx = if name == "local" { CONTEXT.to_string() } else { context(name) };
    if !docker::context_exists(&ctx) {
        bail!(if name == "local" {
            "no `rekcod` context yet; run `rekcod start`".to_string()
        } else {
            format!("no host {name}; `rekcod host ls`")
        });
    }
    docker::use_context(&ctx)?;
    println!("docker context: {ctx}");
    Ok(())
}

pub fn rm(name: &str) -> Result<()> {
    let ctx = context(name);
    if !docker::context_exists(&ctx) {
        bail!("no host {name}");
    }
    if docker::current_context().is_ok_and(|c| c == ctx) {
        let to = if docker::context_exists(CONTEXT) { CONTEXT } else { "default" };
        docker::use_context(to)?;
        println!("docker context: {to}");
    }
    docker::run_ok(&["context", "rm", &ctx])?;
    println!("removed {ctx} (the spare's ~/.zshenv block stays)");
    Ok(())
}

pub fn print_list(json: bool) {
    let (hosts, local) = list();
    if json {
        let v: Vec<_> = hosts.iter().map(|h| serde_json::json!({"name": h.name, "endpoint": h.endpoint, "current": h.current})).collect();
        println!("{}", serde_json::json!({"local_current": local, "hosts": v}));
        return;
    }
    println!("{} local  ({})", if local { "*" } else { " " }, CONTEXT);
    for h in &hosts {
        println!("{} {}  ({})", if h.current { "*" } else { " " }, h.name, h.endpoint);
    }
}

/// Published TCP host ports from `docker ps --format '{{.Ports}}'` lines.
pub fn parse_ports(raw: &str) -> BTreeSet<u16> {
    let mut out = BTreeSet::new();
    for entry in raw.split([',', '\n']).map(str::trim) {
        let Some((host, container)) = entry.split_once("->") else { continue };
        if !container.ends_with("/tcp") {
            continue;
        }
        let Some((_, ports)) = host.rsplit_once(':') else { continue };
        let (a, b) = ports.split_once('-').unwrap_or((ports, ports));
        if let (Ok(a), Ok(b)) = (a.parse::<u16>(), b.parse::<u16>()) {
            out.extend(a..=b);
        }
    }
    out
}

static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_signal(_: libc::c_int) {
    STOP.store(true, std::sync::atomic::Ordering::SeqCst);
}

struct Master {
    child: Child,
    ctl: std::path::PathBuf,
}

impl Drop for Master {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.ctl);
    }
}

pub fn forward(paths: &Paths, name: &str, only: &[u16]) -> Result<()> {
    if let Err(e) = refresh(name) {
        eprintln!("rekcod: {e:#}");
    }
    let (hosts, _) = list();
    let host = hosts.iter().find(|h| h.name == name).with_context(|| format!("no host {name}; `rekcod host ls`"))?;
    let dest = host.endpoint.strip_prefix("ssh://").with_context(|| format!("{} isn't an ssh:// host", host.endpoint))?.to_string();
    let ctl = paths.home.join(format!("run/host-{name}.ctl"));
    std::fs::create_dir_all(ctl.parent().unwrap())?;
    let _ = std::fs::remove_file(&ctl);
    let ctl_s = ctl.display().to_string();
    let child = ssh(&dest).args(["-q", "-M", "-S", &ctl_s, "-N"]).stdin(Stdio::null()).spawn().context("run ssh")?;
    let mut master = Master { child, ctl };
    // Kill or a closed Terminal window would otherwise leave the master connection behind.
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        unsafe { libc::signal(sig, on_signal as *const () as libc::sighandler_t) };
    }
    let target = ssh_dest(&dest);
    let ctl_cmd = |args: &[&str]| {
        Command::new("ssh").args(["-S", &ctl_s]).args(args).args(&target).stdin(Stdio::null()).stderr(Stdio::piped()).output()
    };
    for _ in 0..100 {
        if master.ctl.exists() && ctl_cmd(&["-O", "check"]).is_ok_and(|o| o.status.success()) {
            break;
        }
        if let Some(s) = master.child.try_wait()? {
            bail!("ssh to {dest} exited ({s})");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("forwarding {name}'s published ports to 127.0.0.1 (Ctrl-C stops)");
    // port -> forwarded (false: failed once, don't retry until it goes away)
    let mut active: BTreeMap<u16, bool> = BTreeMap::new();
    while !STOP.load(std::sync::atomic::Ordering::SeqCst) {
        if let Some(s) = master.child.try_wait()? {
            bail!("ssh to {dest} exited ({s})");
        }
        let want: BTreeSet<u16> = if only.is_empty() {
            let out = Command::new("ssh")
                .args(["-S", &ctl_s])
                .args(&target)
                .arg("docker ps --format '{{.Ports}}'")
                .stdin(Stdio::null())
                .output()?;
            if !out.status.success() {
                eprintln!("docker ps on {dest}: {}", String::from_utf8_lossy(&out.stderr).trim());
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
            parse_ports(&String::from_utf8_lossy(&out.stdout))
        } else {
            only.iter().copied().collect()
        };
        for (&p, &ok) in &active.clone() {
            if !want.contains(&p) {
                if ok {
                    let _ = ctl_cmd(&["-O", "cancel", "-L", &format!("127.0.0.1:{p}:localhost:{p}")]);
                    println!("- localhost:{p}");
                }
                active.remove(&p);
            }
        }
        for p in want {
            if active.contains_key(&p) {
                continue;
            }
            let out = ctl_cmd(&["-O", "forward", "-L", &format!("127.0.0.1:{p}:localhost:{p}")])?;
            let ok = out.status.success();
            if ok {
                println!("+ localhost:{p}");
            } else {
                println!("! localhost:{p} not forwarded: {}", String::from_utf8_lossy(&out.stderr).trim());
            }
            active.insert(p, ok);
        }
        for _ in 0..20 {
            if STOP.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_from_docker_ps() {
        let raw = "0.0.0.0:3000->3000/tcp, [::]:3000->3000/tcp\n5432/tcp\n127.0.0.1:8000-8002->80-82/tcp, 0.0.0.0:53->53/udp\n\n";
        assert_eq!(parse_ports(raw).into_iter().collect::<Vec<_>>(), [3000, 8000, 8001, 8002]);
    }

    #[test]
    fn dest_with_port() {
        assert_eq!(ssh_dest("me@spare:2222"), ["-p", "2222", "me@spare"]);
        assert_eq!(ssh_dest("me@spare"), ["me@spare"]);
    }

    #[test]
    fn hosts_from_contexts() {
        let raw = r#"{"Name":"default","Current":false,"DockerEndpoint":"unix:///var/run/docker.sock"}
{"Name":"rekcod","Current":false,"DockerEndpoint":"unix:///x"}
{"Name":"rekcod-spare","Current":true,"DockerEndpoint":"ssh://me@spare"}"#;
        let (hosts, local) = parse_hosts(raw);
        assert!(!local);
        assert_eq!(hosts, [Host { name: "spare".into(), endpoint: "ssh://me@spare".into(), current: true, bonjour: None }]);
    }

    #[test]
    fn lan_spare_description() {
        let lan = Lan { instance: "rekcod-3f9a1c2b7d4e".into(), fp: "sha256:3f9a".into(), host_key: "ssh-ed25519 AAAAC3Nza+/=".into() };
        let d = description(Some(&lan));
        assert_eq!(parse_description(&d), Some(lan));
        assert_eq!(parse_description(DESCRIPTION), None);
        // Added before host keys were pinned: no refresh, rather than an unchecked one.
        assert_eq!(parse_description("rekcod host bonjour=rekcod-3f9a fp=sha256:3f9a"), None);
        let raw = format!(r#"{{"Name":"rekcod-mini","Current":false,"DockerEndpoint":"ssh://me@mini.local","Description":"{d}"}}"#);
        assert!(parse_hosts(&raw).0[0].bonjour.is_some());
    }

    #[test]
    fn known_hosts_names() {
        assert_eq!(known_hosts_name("me@Spare.local"), "Spare.local");
        assert_eq!(known_hosts_name("me@Spare.local:2222"), "[Spare.local]:2222");
        assert_eq!(known_hosts_name("Spare.local:22"), "Spare.local");
    }

    #[test]
    fn retarget_keeps_user_and_port() {
        assert_eq!(retarget("me@spare-mac", "Spare-2.local"), "me@Spare-2.local");
        assert_eq!(retarget("me@10.0.0.4:2222", "Spare-2.local"), "me@Spare-2.local:2222");
        assert_eq!(retarget("spare", "Spare-2.local"), "Spare-2.local");
    }
}
