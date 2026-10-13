//! rekcod: Docker on macOS, switched in one command.
//!
//! The user-facing commands live in `cli`; `daemon` is the long-running
//! process that owns the VM (started by `rekcod start`).

use clap::{Parser, Subcommand};

mod dns;
mod docker;
mod http;
mod memory;
mod paths;
mod ports;
mod proxy;
mod route;

#[cfg(target_os = "macos")]
mod actions;
#[cfg(target_os = "macos")]
mod cli;
#[cfg(target_os = "macos")]
mod cli_fetch;
#[cfg(target_os = "macos")]
mod daemon;
#[cfg(target_os = "macos")]
mod edge;
#[cfg(target_os = "macos")]
mod fetch;
#[cfg(target_os = "macos")]
mod fleet;
#[cfg(target_os = "macos")]
mod host;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod menubar;
#[cfg(target_os = "macos")]
mod s3;
#[cfg(target_os = "macos")]
mod sandbox;
#[cfg(target_os = "macos")]
mod sandbox_api;
#[cfg(target_os = "macos")]
mod service;
#[cfg(target_os = "macos")]
mod snap;
#[cfg(target_os = "macos")]
mod spare;
#[cfg(target_os = "macos")]
mod storage;
#[cfg(target_os = "macos")]
mod syssock;
#[cfg(target_os = "macos")]
mod vm;
#[cfg(target_os = "macos")]
mod watch;
#[cfg(target_os = "macos")]
mod xrun;

#[derive(Parser)]
#[command(
    name = "rekcod",
    version,
    about = "Docker on macOS: real dockerd in a fast VM, switched in one command",
    after_help = "Any other command runs as `docker <command>` against rekcod (booting it if needed):\n  \
                  rekcod ps, rekcod run -it alpine sh, rekcod compose up -d, rekcod build -t app .\n\
                  `rekcod start web` / `rekcod stop -t 5 web` are docker's; `rekcod docker <args>` always is."
)]
struct Args {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Boot the VM and make `rekcod` the active docker context.
    Start {
        /// vCPUs (default: the Mac's performance cores, at least 4).
        #[arg(long)]
        cpus: Option<usize>,
        /// Memory in GiB (default: half of host RAM, at most 8). Unused memory is not reserved.
        #[arg(long)]
        memory: Option<u64>,
        /// Data disk size in GiB (sparse: only used space takes room on the Mac). Grows an existing disk; never shrinks.
        #[arg(long, default_value_t = paths::DEFAULT_DISK_GIB)]
        disk: u64,
        /// Do not change the active docker context.
        #[arg(long)]
        no_switch: bool,
    },
    /// Power off the VM and restore the previous docker context.
    Stop {
        /// Do not change the active docker context.
        #[arg(long)]
        no_switch: bool,
    },
    /// Show VM state, versions, forwarded ports, disk and file-cache stats.
    Status {
        /// Machine-readable output (the scriptable API, docs/phase5.md §8).
        #[arg(long)]
        json: bool,
    },
    /// Save and restore a compose project's volumes (seeded databases, per-branch state).
    #[command(subcommand)]
    Snap(Snap),
    /// Run a command in a container, no Dockerfile: `rekcod x npm run dev`.
    #[command(trailing_var_arg = true)]
    X {
        /// Image to use instead of the detected one.
        #[arg(long)]
        image: Option<String>,
        /// Publish this port on the Mac instead of forwarding listeners automatically.
        #[arg(short = 'p', long = "publish")]
        publish: Vec<String>,
        /// Environment variable (KEY=VALUE).
        #[arg(short = 'e', long = "env")]
        env: Vec<String>,
        /// Don't forward listening ports automatically.
        #[arg(long)]
        no_autoports: bool,
        /// Run in a separate VM that sees only this directory (docs/sandbox.md).
        #[arg(long)]
        sandbox: bool,
        /// No network at all.
        #[arg(long)]
        offline: bool,
        /// Pass this variable from your shell into the container (sandbox: the only way in).
        #[arg(long = "secret")]
        secrets: Vec<String>,
        /// Leave the sandbox VM running afterwards.
        #[arg(long)]
        keep: bool,
        /// The command and its arguments.
        #[arg(required = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },
    /// Throwaway VMs for AI agents: a full dockerd that sees a copy of one project (docs/agent-sandboxes.md).
    Sandbox {
        /// Run on a fleet controller (`rekcod remote add`); no NAME means the default remote.
        #[arg(long, global = true, num_args = 0..=1, default_missing_value = "")]
        remote: Option<String>,
        /// Namespace on the controller (default: the remote's, see `rekcod ns use`).
        #[arg(long, global = true, env = "REKCOD_NS")]
        ns: Option<String>,
        #[command(subcommand)]
        cmd: SandboxCmd,
    },
    /// MCP server on stdio exposing the sandbox commands to agents.
    Mcp,
    /// macOS guests: base images for `rekcod sandbox create --os macos`.
    #[command(subcommand)]
    Macos(MacosCmd),
    /// Fleet controllers this CLI talks to (docs/platform.md).
    #[command(subcommand)]
    Remote(RemoteCmd),
    /// Another Mac's rekcod as your docker engine, over SSH (docs/remote-docker.md).
    #[command(subcommand)]
    Host(HostCmd),
    /// This Mac as a headless one-machine fleet (docs/spare.md); `host add --fleet` runs it over SSH.
    #[command(subcommand)]
    Spare(SpareCmd),
    /// Run this Mac as a fleet node.
    #[command(subcommand)]
    Node(NodeCmd),
    /// The fleet controller: sandbox placement, apps, tokens (state in SQLite).
    #[command(subcommand)]
    Controller(ControllerCmd),
    /// HTTP ingress for apps: routes <app>.<domain> to live instances.
    #[command(subcommand)]
    Edge(EdgeCmd),
    /// S3 storage (RustFS): the fleet's bucket, or one in the local VM for development (docs/rustfs.md).
    #[command(subcommand)]
    Storage(StorageCmd),
    /// Apps hosted on the fleet: deploy, rollback, scale, env, logs.
    App {
        #[arg(long, global = true, num_args = 0..=1, default_missing_value = "")]
        remote: Option<String>,
        #[arg(long, global = true, env = "REKCOD_NS")]
        ns: Option<String>,
        #[command(subcommand)]
        cmd: AppCmd,
    },
    /// Commands that run to completion on the fleet, once or on a schedule.
    Job {
        #[arg(long, global = true, num_args = 0..=1, default_missing_value = "")]
        remote: Option<String>,
        #[arg(long, global = true, env = "REKCOD_NS")]
        ns: Option<String>,
        #[command(subcommand)]
        cmd: JobCmd,
    },
    /// Namespaces on the fleet: apps, sandboxes and jobs shared by a team (docs/namespaces.md).
    Ns {
        #[arg(long, global = true, num_args = 0..=1, default_missing_value = "")]
        remote: Option<String>,
        #[command(subcommand)]
        cmd: NsCmd,
    },
    /// Copy images and named volumes from another docker context (desktop-linux, orbstack, ...).
    Import {
        /// Source docker context.
        #[arg(long)]
        from: String,
        /// Only these image refs / volume names (default: everything).
        names: Vec<String>,
        /// Only images.
        #[arg(long, conflicts_with = "volumes_only")]
        images_only: bool,
        /// Only volumes.
        #[arg(long)]
        volumes_only: bool,
        /// Copy volumes even if in use at the source or already non-empty here.
        #[arg(long)]
        force: bool,
    },
    /// Open a root shell in the VM.
    Shell,
    /// Give unused data-disk space back to macOS now (also runs hourly).
    Compact,
    /// Route /var/run/docker.sock to rekcod while it runs, for clients that ignore docker contexts.
    #[command(subcommand)]
    Socket(Socket),
    /// Menu bar item: status, start/stop, forwarded ports.
    Menubar {
        /// Start the menu bar item at login (LaunchAgent).
        #[arg(long, conflicts_with = "uninstall")]
        install: bool,
        /// Remove the login item.
        #[arg(long)]
        uninstall: bool,
    },
    #[command(hide = true, subcommand)]
    Research(Research),
    #[command(hide = true)]
    Daemon {
        #[arg(long)]
        cpus: usize,
        #[arg(long)]
        memory_mib: u64,
        #[arg(long)]
        project: Option<std::path::PathBuf>,
        #[arg(long)]
        workspace: Option<std::path::PathBuf>,
        #[arg(long)]
        allow: Option<String>,
        #[arg(long)]
        reach: Option<String>,
        #[arg(long)]
        idle_ttl: Option<u64>,
    },
}

#[derive(Subcommand)]
enum Snap {
    /// Snapshot the project's volumes (NAME defaults to the git branch).
    Save {
        name: Option<String>,
        /// Compose project (default: the one started from this directory).
        #[arg(short = 'p', long)]
        project: Option<String>,
        /// Also upload it to $S3_ENDPOINT/$S3_BUCKET (AWS_* keys), else to `rekcod storage up`'s rekcod-snaps.
        #[arg(long)]
        push: bool,
    },
    /// Put the project's volumes back to a snapshot.
    Restore {
        name: Option<String>,
        #[arg(short = 'p', long)]
        project: Option<String>,
        /// Download it first from where `save --push` put it.
        #[arg(long)]
        pull: bool,
    },
    /// List snapshots.
    Ls {
        #[arg(short = 'p', long)]
        project: Option<String>,
        /// Every project's snapshots.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        json: bool,
    },
    /// Delete a snapshot.
    Rm {
        name: String,
        #[arg(short = 'p', long)]
        project: Option<String>,
    },
}

/// A quota: a count, or `unlimited` (stored as -1).
fn quota_value(s: &str) -> Result<i64, String> {
    match s {
        "unlimited" => Ok(-1),
        _ => s.parse::<u32>().map(i64::from).map_err(|_| format!("{s}: a number or `unlimited`")),
    }
}

/// `30m`, `2h`, `90s`, `0` (never).
fn duration_secs(s: &str) -> Result<u64, String> {
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = n.parse().map_err(|_| format!("bad duration {s}"))?;
    match unit {
        "" | "s" => Ok(n),
        "m" => Ok(n * 60),
        "h" => Ok(n * 3600),
        _ => Err(format!("bad duration {s} (use s, m or h)")),
    }
}

#[derive(Subcommand)]
enum SandboxCmd {
    /// Create a sandbox for DIR (default: this directory) and print its ID.
    Create {
        dir: Option<std::path::PathBuf>,
        /// linux (default) or macos: a guest cloned from `rekcod macos install` (--image picks it).
        #[arg(long)]
        os: Option<String>,
        /// --remote: repository to clone (default: this directory's origin).
        #[arg(long)]
        git: Option<String>,
        /// --remote: branch, tag or commit (default: this directory's HEAD).
        #[arg(long = "ref")]
        git_ref: Option<String>,
        #[arg(long)]
        name: Option<String>,
        /// Share the live directory instead of a copy (no diff/apply).
        #[arg(long)]
        mount: bool,
        /// Workspace image (default: picked from the project's version files).
        #[arg(long)]
        image: Option<String>,
        /// Golden disk to start from (`rekcod sandbox golden build`).
        #[arg(long, default_value = "default")]
        golden: String,
        #[arg(long, default_value_t = 2)]
        cpus: usize,
        /// Memory in GiB; not returned to macOS until the sandbox stops.
        #[arg(long, default_value_t = 2)]
        memory: u64,
        /// Also allow this host (`*.example.com` for subdomains, `*` for the whole internet).
        #[arg(long = "allow-host")]
        allow_hosts: Vec<String>,
        /// No network at all.
        #[arg(long)]
        offline: bool,
        /// Pass this variable from your shell into the workspace container.
        #[arg(long = "secret")]
        secrets: Vec<String>,
        /// Power off after this long without an exec (`0`: never).
        #[arg(long, default_value = "30m", value_parser = duration_secs)]
        ttl: u64,
        #[arg(long)]
        json: bool,
    },
    /// Run a command in the sandbox; exits with its exit code.
    #[command(trailing_var_arg = true)]
    Exec {
        id: String,
        #[arg(short = 'w', long)]
        workdir: Option<String>,
        #[arg(short = 'e', long = "env")]
        env: Vec<String>,
        /// Kill the command after this long.
        #[arg(long, value_parser = duration_secs)]
        timeout: Option<u64>,
        #[arg(required = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },
    /// Interactive shell in the sandbox.
    Shell { id: String },
    /// Copy files; one side is ID:/path.
    Cp { src: String, dst: String },
    /// New sandbox from this one's disk and workspace (processes don't carry over).
    Fork {
        id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        json: bool,
    },
    /// Workspace changes against the real project.
    Diff {
        id: String,
        #[arg(long)]
        stat: bool,
    },
    /// Write the workspace's changes into the real project.
    Apply {
        id: String,
        /// Only print what would change; exit 1 if anything would.
        #[arg(long)]
        check: bool,
    },
    /// Save a macOS sandbox's screen as PNG.
    Screenshot {
        id: String,
        #[arg(default_value = "screenshot.png")]
        out: std::path::PathBuf,
        /// Open it in Preview afterwards.
        #[arg(long)]
        open: bool,
    },
    /// Mouse and keyboard for a macOS sandbox: click X Y | type TEXT | key [cmd+]KEY.
    Input {
        id: String,
        #[arg(required = true, num_args = 1.., allow_hyphen_values = true)]
        action: Vec<String>,
    },
    /// Forwarded ports and VM status.
    Ports {
        id: String,
        #[arg(long)]
        json: bool,
    },
    /// Commands run in the sandbox (`--net`: blocked lookups).
    Log {
        id: String,
        #[arg(short = 'f', long)]
        follow: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        net: bool,
    },
    /// List sandboxes.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Power off, keeping state; the next exec boots it again.
    Stop { id: String },
    /// Force stop and delete.
    Rm {
        ids: Vec<String>,
        #[arg(long, conflicts_with = "ids")]
        all: bool,
    },
    /// Serve the sandbox API on ~/.rekcod/run/sandbox.sock.
    Serve {
        /// Also listen on 127.0.0.1:PORT, with a bearer token.
        #[arg(long)]
        listen: Option<String>,
    },
    /// Data disks with images already pulled, cloned per sandbox.
    #[command(subcommand)]
    Golden(Golden),
}

#[derive(Subcommand)]
enum MacosCmd {
    /// Download the newest macOS restore image this Mac supports (resumable).
    Fetch,
    /// Install the restore image into a new base image.
    Install {
        #[arg(default_value = "default")]
        name: String,
        #[arg(long, default_value_t = 64)]
        disk: u64,
        #[arg(long, default_value_t = 4)]
        cpus: usize,
        #[arg(long, default_value_t = 8)]
        memory: u64,
    },
    /// Open the image's screen to finish setup by hand (Setup Assistant, Remote Login).
    Setup {
        #[arg(default_value = "default")]
        name: String,
    },
    /// Install rekcod's SSH key in the running image (asks for the guest password).
    Authorize {
        #[arg(default_value = "default")]
        name: String,
    },
    Ls,
    Rm {
        name: String,
    },
    #[command(hide = true)]
    Run {
        home: std::path::PathBuf,
        #[arg(long, default_value_t = 0)]
        idle_ttl: u64,
    },
}

#[derive(Subcommand)]
enum RemoteCmd {
    /// Add a controller; the first one added is the default.
    Add {
        name: String,
        url: String,
        #[arg(long)]
        token: String,
        /// Namespace this remote's commands use (default: default).
        #[arg(long)]
        ns: Option<String>,
    },
    Ls,
    Rm {
        name: String,
    },
}

#[derive(Subcommand)]
enum NodeCmd {
    /// Register with a controller and start serving (saves ~/.rekcod/node.json).
    Join {
        controller: String,
        join_token: String,
        /// Address the node API listens on.
        #[arg(long, default_value = "127.0.0.1:7701")]
        listen: String,
        /// URL the controller should use, when --listen is 0.0.0.0.
        #[arg(long)]
        advertise: Option<String>,
        /// After joining, run `node serve` as a LaunchAgent instead of in this terminal.
        #[arg(long)]
        install: bool,
    },
    /// Serve again after a restart (uses ~/.rekcod/node.json).
    Serve {
        /// Run as a LaunchAgent: now, at login, and again if it exits.
        #[arg(long)]
        install: bool,
    },
    /// Remove the node's LaunchAgent.
    Uninstall,
    /// Nodes known to the controller (admin token).
    Ls {
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        remote: Option<String>,
    },
    /// Stop placing new sandboxes on a node (admin token).
    Drain {
        id: String,
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        remote: Option<String>,
    },
    /// Dedicate a node to namespaces (admin token); no --ns gives it back to everyone.
    Pool {
        id: String,
        #[arg(long)]
        ns: Vec<String>,
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        remote: Option<String>,
    },
}

#[derive(Subcommand)]
enum NsCmd {
    /// Create a namespace (admin token).
    Create {
        name: String,
        #[arg(long, value_parser = quota_value)]
        max_cpus: Option<i64>,
        /// GiB.
        #[arg(long, value_parser = quota_value)]
        max_memory: Option<i64>,
        #[arg(long, value_parser = quota_value)]
        max_sandboxes: Option<i64>,
        #[arg(long, value_parser = quota_value)]
        max_apps: Option<i64>,
    },
    /// Namespaces you're in, with your role, quota and usage.
    Ls,
    /// Change a namespace's quota (admin token).
    Quota {
        name: String,
        #[arg(long, value_parser = quota_value)]
        max_cpus: Option<i64>,
        #[arg(long, value_parser = quota_value)]
        max_memory: Option<i64>,
        #[arg(long, value_parser = quota_value)]
        max_sandboxes: Option<i64>,
        #[arg(long, value_parser = quota_value)]
        max_apps: Option<i64>,
    },
    /// Members (owner role).
    #[command(subcommand)]
    Member(NsMember),
    /// Send this remote's app, sandbox and job commands to NAME from now on.
    Use { name: String },
    /// Delete a namespace (admin token); --force destroys its apps, sandboxes and jobs.
    Rm {
        name: String,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Subcommand)]
enum NsMember {
    Add {
        ns: String,
        who: String,
        /// viewer, member or owner.
        #[arg(long, default_value = "member")]
        role: String,
    },
    Rm {
        ns: String,
        who: String,
    },
}

#[derive(Subcommand)]
enum ControllerCmd {
    /// Run the controller.
    Serve {
        /// SQLite file (default: ~/.rekcod/controller/fleet.db).
        #[arg(long)]
        db: Option<std::path::PathBuf>,
        /// Run as a LaunchAgent: now, at login, and again if it exits.
        #[arg(long)]
        install: bool,
        #[arg(long, default_value = "127.0.0.1:7700")]
        listen: String,
        /// Apps are served at <app>.<domain>.
        #[arg(long, default_value = "apps.localhost")]
        domain: String,
        /// How long old instances keep running after a deploy.
        #[arg(long, default_value = "30s", value_parser = duration_secs)]
        drain: u64,
        /// RustFS URL (`rekcod storage serve`) for app buckets, stored images and backups; remembered once given.
        #[arg(long)]
        storage: Option<String>,
        /// RustFS root keys (default: ~/.rekcod/storage/credentials.json, which `storage serve` writes).
        #[arg(long)]
        storage_credentials: Option<std::path::PathBuf>,
    },
    /// Remove the controller's LaunchAgent.
    Uninstall,
    /// Upload fleet.db to rekcod-backups now (it also does every hour); admin token.
    Backup {
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        remote: Option<String>,
    },
    /// Backups in rekcod-backups.
    Backups {
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        remote: Option<String>,
    },
    /// Replace fleet.db with a backup (the latest unless KEY); stop the controller first.
    Restore {
        key: Option<String>,
        #[arg(long)]
        db: Option<std::path::PathBuf>,
        #[arg(long)]
        storage_credentials: Option<std::path::PathBuf>,
    },
    /// Change an existing token's cap (admin token); applies to every token with that name.
    Limit {
        name: String,
        /// Running sandboxes plus jobs, in every namespace together; `unlimited` for no cap.
        #[arg(long, value_parser = quota_value)]
        max_sandboxes: i64,
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        remote: Option<String>,
    },
    /// Create a user token (admin token).
    Token {
        name: String,
        /// Running sandboxes plus jobs, in every namespace together; `unlimited` for no cap.
        #[arg(long, default_value = "10", value_parser = quota_value)]
        max_sandboxes: i64,
        #[arg(long)]
        admin: bool,
        /// Also make the token a member of this namespace.
        #[arg(long)]
        ns: Option<String>,
        /// Its role there: viewer, member or owner.
        #[arg(long, default_value = "member", requires = "ns")]
        role: String,
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        remote: Option<String>,
    },
}

#[derive(Subcommand)]
enum EdgeCmd {
    /// Serve HTTP ingress (needs an admin-token remote).
    Serve {
        /// Routes every app; bind it to the tailnet or loopback.
        #[arg(long, default_value = "127.0.0.1:8080")]
        listen: String,
        /// Routes only apps marked public; point cloudflared at it.
        #[arg(long)]
        public_listen: Option<String>,
        /// The public listener's clients send a PROXY v2 header (a TCP load balancer in front).
        #[arg(long, requires = "public_listen")]
        proxy_protocol: bool,
        /// Peers (IP or CIDR) whose CF-Connecting-IP / X-Forwarded-For name the client, such
        /// as cloudflared. Repeat it; giving any replaces the loopback default.
        #[arg(long = "trusted-proxy", value_name = "NET", default_values = ["127.0.0.0/8", "::1"])]
        trusted_proxies: Vec<String>,
        /// Requests per client IP across all apps (N/s, N/m or N/h).
        #[arg(long, default_value = "50/s")]
        rate: String,
        #[arg(long, default_value_t = 100)]
        burst: u32,
        /// Print a line per request: client, method, host and path, status, time.
        #[arg(long)]
        access_log: bool,
        /// Run as a LaunchAgent: now, at login, and again if it exits.
        #[arg(long)]
        install: bool,
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        remote: Option<String>,
    },
    /// Remove the edge's LaunchAgent.
    Uninstall,
    /// Ban an IP or CIDR range on every edge (asks for one when left out).
    Ban {
        net: Option<String>,
        /// Lift the ban after this long (default: never).
        #[arg(long = "for", value_parser = duration_secs)]
        duration: Option<u64>,
        #[arg(long, default_value = "")]
        reason: String,
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        remote: Option<String>,
    },
    Unban {
        net: String,
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        remote: Option<String>,
    },
    /// Banned addresses, with reason and expiry.
    Bans {
        #[arg(long, num_args = 0..=1, default_missing_value = "")]
        remote: Option<String>,
    },
}

#[derive(Subcommand)]
enum AppCmd {
    /// Deploy this repository's pushed HEAD (settings from rekcod.toml, flags win).
    Deploy {
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        port: Option<i64>,
        #[arg(long)]
        instances: Option<i64>,
        #[arg(long)]
        git: Option<String>,
        #[arg(long = "ref")]
        git_ref: Option<String>,
        /// Serve it on the edge's public listener too (new apps are private).
        #[arg(long, conflicts_with = "private")]
        public: bool,
        /// Only on the edge's private listener.
        #[arg(long)]
        private: bool,
        /// Stop the VMs after this long without a request; the next one wakes them (0: never).
        #[arg(long, value_parser = duration_secs)]
        idle_stop: Option<u64>,
        /// Replace the instances with fresh VMs this often, so their memory goes back to macOS (0: never).
        #[arg(long, value_parser = duration_secs)]
        recycle: Option<u64>,
    },
    Ls,
    /// Start a sleeping app's instances now.
    Wake {
        name: String,
    },
    /// Replace the instances with fresh VMs (a zero-downtime redeploy of the live commit).
    Recycle {
        name: String,
    },
    /// Serve an app on the edge's public listener.
    Public {
        name: String,
    },
    /// Take an app off the public listener (it stays on the private one).
    Private {
        name: String,
    },
    /// Releases, instances, env and URL.
    Status {
        name: String,
    },
    /// Go back to an earlier good release (default: the one before the live one).
    Rollback {
        name: String,
        release: Option<i64>,
    },
    Scale {
        name: String,
        instances: i64,
    },
    #[command(subcommand)]
    Env(AppVars),
    #[command(subcommand)]
    Secret(AppVars),
    Logs {
        name: String,
    },
    Open {
        name: String,
    },
    /// Delete the app; a non-empty bucket needs --force (asked for on a terminal).
    Destroy {
        name: String,
        #[arg(long)]
        force: bool,
    },
    /// The app's bucket: endpoint, key and size; `ls`, `url KEY`, `rotate`.
    Storage {
        name: String,
        #[command(subcommand)]
        cmd: Option<AppStorageCmd>,
    },
}

#[derive(Subcommand)]
enum AppStorageCmd {
    /// Objects in the bucket.
    Ls,
    /// A presigned GET URL for KEY.
    Url {
        key: String,
        /// How long it works.
        #[arg(long, default_value = "1h", value_parser = duration_secs)]
        expires: u64,
    },
    /// New secret key for the app; redeploys the live release with it.
    Rotate,
}

#[derive(Subcommand)]
enum StorageCmd {
    /// Run RustFS on this Mac for the fleet (downloads the pinned release on first run).
    Serve {
        /// The address nodes and instances reach it on (the Tailscale IP).
        #[arg(long, default_value = "127.0.0.1:7710")]
        listen: String,
        /// Data directory (default: ~/.rekcod/storage/data).
        #[arg(long)]
        data: Option<std::path::PathBuf>,
        /// Run as a LaunchAgent: now, at login, and again if it exits.
        #[arg(long)]
        install: bool,
    },
    /// Remove its LaunchAgent (the data stays).
    Uninstall,
    /// RustFS in the local VM on a free port (from 9000); prints the env block.
    Up,
    /// Stop the local one (the data stays in volume rekcod-storage).
    Down,
    /// The local one's env block.
    Env,
}

#[derive(Subcommand)]
enum JobCmd {
    /// Run CMD in a new sandbox on the fleet, from this repo's pushed HEAD.
    Run {
        #[arg(long)]
        name: Option<String>,
        /// Default: from the repo's version files (as `rekcod x`), else alpine.
        #[arg(long)]
        image: Option<String>,
        #[arg(long, default_value_t = 2)]
        cpus: u64,
        /// GiB.
        #[arg(long, default_value_t = 2)]
        memory: u64,
        /// KEY=VALUE, repeatable.
        #[arg(long)]
        env: Vec<String>,
        /// Kill it after this long (exit 124).
        #[arg(long, value_parser = duration_secs)]
        timeout: Option<u64>,
        /// Run it on this schedule instead of once (needs --name).
        #[arg(long, value_parser = duration_secs)]
        every: Option<u64>,
        #[arg(long)]
        git: Option<String>,
        #[arg(long = "ref")]
        git_ref: Option<String>,
        /// Print the job ID and return instead of following its output.
        #[arg(long, short)]
        detach: bool,
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },
    /// Recent jobs and schedules.
    Ls,
    /// A job's output (the last 64 KiB once it's finished).
    Logs {
        id: String,
    },
    Cancel {
        id: String,
    },
    /// Delete a schedule (its running job keeps going).
    Unschedule {
        name: String,
    },
}

#[derive(Subcommand)]
enum AppVars {
    /// KEY=VALUE...; redeploys the live release.
    Set {
        name: String,
        vars: Vec<String>,
    },
    Unset {
        name: String,
        keys: Vec<String>,
    },
}

#[derive(Subcommand)]
enum Golden {
    /// Build (or rebuild) a golden disk.
    Build {
        #[arg(default_value = "default")]
        name: String,
        #[arg(long = "image")]
        images: Vec<String>,
    },
    Ls,
    Rm {
        name: String,
    },
}

#[derive(Subcommand)]
enum HostCmd {
    /// Set up DEST (user@mac) for docker over SSH and switch the docker context to it.
    Add {
        name: String,
        dest: String,
        /// Create the context without switching to it.
        #[arg(long)]
        no_use: bool,
        /// Also make DEST a fleet you deploy apps to, running headless (docs/spare.md).
        #[arg(long, requires = "domain")]
        fleet: bool,
        /// Apps are served at <app>.<domain> (with --fleet).
        #[arg(long)]
        domain: Option<String>,
        /// The spare's address for the fleet (default: its Tailscale IP).
        #[arg(long, requires = "fleet")]
        addr: Option<String>,
        /// Leave the spare's sleep settings alone.
        #[arg(long, requires = "fleet")]
        no_power: bool,
        /// Only the fleet; no docker context.
        #[arg(long, requires = "fleet")]
        no_docker: bool,
    },
    /// Hosts (docker contexts rekcod-*); * marks the one in use.
    Ls {
        #[arg(long)]
        json: bool,
    },
    /// Switch the docker context to a host, or `local` for this Mac's rekcod.
    Use { name: String },
    /// Delete a host's docker context.
    Rm { name: String },
    /// Forward the host's published ports to 127.0.0.1 here until Ctrl-C (default: all, followed live).
    Forward { name: String, ports: Vec<u16> },
}

#[derive(Subcommand)]
enum SpareCmd {
    /// Controller, node, edge and RustFS as LaunchAgents on the Tailscale address; no sleep with the lid shut.
    Setup {
        /// Apps are served at <app>.<domain>.
        #[arg(long)]
        domain: String,
        /// Listen here instead of the Tailscale IP.
        #[arg(long)]
        addr: Option<String>,
        /// Skip `sudo pmset`.
        #[arg(long)]
        no_power: bool,
    },
    /// Remove the controller, node, edge and RustFS LaunchAgents (data stays).
    Uninstall,
}

#[derive(Subcommand)]
enum Socket {
    /// Point /var/run/docker.sock at ~/.rekcod/default.sock (sudo, once); start/stop then switch it.
    Install,
    /// Put /var/run/docker.sock back to what it was before `install` (sudo).
    Uninstall,
}

/// Phase 6 prototypes (docs/research.md). The daemon must run with REKCOD_RESEARCH=1.
#[derive(Subcommand)]
enum Research {
    /// Cold lstat latency: host metadata server over vsock vs virtio-fs.
    BenchMeta { dir: String },
}

#[cfg(target_os = "macos")]
fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    if let Some(docker_args) = route::docker_args(&argv) {
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
        let err = paths::Paths::new().and_then(|paths| cli::passthrough(&paths, docker_args)).unwrap_err();
        eprintln!("rekcod: {err:#}");
        std::process::exit(1);
    }
    let args = Args::parse();
    let command = args.command;
    if !matches!(command, Cmd::Daemon { .. }) {
        // Behave like other CLIs when piped into `head` (the daemon keeps Rust's SIGPIPE-ignore).
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    }
    let result = paths::Paths::new().and_then(|paths| match command {
        Cmd::Start { cpus, memory, disk, no_switch } => cli::start(&paths, cpus, memory, disk, no_switch),
        Cmd::Stop { no_switch } => cli::stop(&paths, no_switch),
        Cmd::Status { json } => cli::status(&paths, json),
        Cmd::Snap(Snap::Save { name, project, push }) => snap::save(&paths, project.as_deref(), name.as_deref(), push),
        Cmd::Snap(Snap::Restore { name, project, pull }) => snap::restore(&paths, project.as_deref(), name.as_deref(), pull),
        Cmd::Snap(Snap::Ls { project, all, json }) => snap::list(&paths, project.as_deref(), all, json),
        Cmd::Snap(Snap::Rm { name, project }) => snap::remove(&paths, project.as_deref(), &name),
        Cmd::X { image, publish, env, no_autoports, sandbox, offline, secrets, keep, cmd } => {
            let opts = xrun::Options { image, publish, env, no_autoports, sandbox, offline, secrets, keep };
            xrun::run(&paths, &cmd, opts).map(|_| ())
        }
        Cmd::Sandbox { remote: Some(r), ns, cmd } => {
            set_ns(ns);
            run_remote_sandbox(&paths, &r, cmd)
        }
        Cmd::Sandbox { remote: None, ns: Some(_), .. } => Err(anyhow::anyhow!("--ns is for fleet sandboxes; add --remote")),
        Cmd::Sandbox { remote: None, cmd, .. } => run_sandbox(&paths, cmd),
        Cmd::Mcp => sandbox_api::mcp(paths),
        Cmd::Macos(MacosCmd::Fetch) => macos::fetch(&paths),
        Cmd::Macos(MacosCmd::Install { name, disk, cpus, memory }) => macos::install(&paths, &name, disk, cpus, memory),
        Cmd::Macos(MacosCmd::Setup { name }) => macos::setup(&paths, &name),
        Cmd::Macos(MacosCmd::Authorize { name }) => macos::authorize(&paths, &name),
        Cmd::Macos(MacosCmd::Ls) => macos::list(&paths),
        Cmd::Macos(MacosCmd::Rm { name }) => macos::remove(&paths, &name),
        Cmd::Macos(MacosCmd::Run { home, idle_ttl }) => macos::run(home, idle_ttl),
        Cmd::Remote(RemoteCmd::Add { name, url, token, ns }) => fleet::remote_add(&paths, &name, &url, &token, ns.as_deref()),
        Cmd::Remote(RemoteCmd::Ls) => {
            fleet::remote_list(&paths);
            Ok(())
        }
        Cmd::Remote(RemoteCmd::Rm { name }) => fleet::remote_rm(&paths, &name),
        Cmd::Node(NodeCmd::Join { controller, join_token, listen, advertise, install }) => {
            fleet::node_join(&paths, &controller, &join_token, &listen, advertise.as_deref(), install)
        }
        Cmd::Node(NodeCmd::Serve { install: true }) => service::install(&paths, "node", &["node".into(), "serve".into()]),
        Cmd::Node(NodeCmd::Serve { install: false }) => fleet::node_serve(&paths),
        Cmd::Node(NodeCmd::Uninstall) => service::uninstall("node"),
        Cmd::Controller(ControllerCmd::Uninstall) => service::uninstall("controller"),
        Cmd::Node(NodeCmd::Ls { remote }) => remote_call(&paths, &remote, "GET", "/nodes", &serde_json::Value::Null),
        Cmd::Node(NodeCmd::Pool { id, ns, remote }) => {
            remote_call(&paths, &remote, "POST", &format!("/nodes/{id}/pool"), &serde_json::json!({"ns": ns}))
        }
        Cmd::Node(NodeCmd::Drain { id, remote }) => {
            remote_call(&paths, &remote, "POST", &format!("/nodes/{id}/drain"), &serde_json::json!({}))
        }
        Cmd::Controller(ControllerCmd::Serve { install: true, .. }) => {
            service::install(&paths, "controller", &service::args_without_install())
        }
        Cmd::Controller(ControllerCmd::Serve { db, listen, domain, drain, storage, storage_credentials, .. }) => fleet::controller_serve(
            &paths,
            fleet::ControllerOptions { db, listen, domain, drain_secs: drain, storage, storage_credentials },
        ),
        Cmd::Controller(ControllerCmd::Backup { remote }) => remote_call(&paths, &remote, "POST", "/backups", &serde_json::json!({})),
        Cmd::Controller(ControllerCmd::Backups { remote }) => remote_call(&paths, &remote, "GET", "/backups", &serde_json::Value::Null),
        Cmd::Controller(ControllerCmd::Restore { key, db, storage_credentials }) => {
            fleet::controller_restore(&paths, db, key.as_deref(), storage_credentials)
        }
        Cmd::Storage(StorageCmd::Serve { install: true, .. }) => service::install(&paths, "storage", &service::args_without_install()),
        Cmd::Storage(StorageCmd::Serve { listen, data, .. }) => storage::serve(&paths, &listen, data.as_deref()),
        Cmd::Storage(StorageCmd::Uninstall) => service::uninstall("storage"),
        Cmd::Storage(StorageCmd::Up) => storage::up(&paths),
        Cmd::Storage(StorageCmd::Down) => storage::down(&paths),
        Cmd::Storage(StorageCmd::Env) => storage::env(&paths),
        Cmd::Controller(ControllerCmd::Limit { name, max_sandboxes, remote }) => {
            remote_call(&paths, &remote, "POST", &format!("/tokens/{name}"), &serde_json::json!({"max_sandboxes": max_sandboxes}))
        }
        Cmd::Controller(ControllerCmd::Token { name, max_sandboxes, admin, ns, role, remote }) => remote_call(
            &paths,
            &remote,
            "POST",
            "/tokens",
            &serde_json::json!({"name": name, "max_sandboxes": max_sandboxes, "admin": admin, "ns": ns, "role": role}),
        ),
        Cmd::Edge(EdgeCmd::Serve { install: true, .. }) => service::install(&paths, "edge", &service::args_without_install()),
        Cmd::Edge(EdgeCmd::Serve { listen, public_listen, proxy_protocol, trusted_proxies, rate, burst, access_log, remote, .. }) => {
            let rate = edge::limit::parse_rate(&rate)?;
            let trusted_proxies = trusted_proxies.iter().map(|n| edge::ban::parse_net(n)).collect::<anyhow::Result<_>>()?;
            let settings = edge::Settings { rate, burst, access_log, trusted_proxies };
            let o = edge::Options { listen, public_listen, proxy_protocol, settings };
            edge::serve(&paths, remote.as_deref().filter(|r| !r.is_empty()), o)
        }
        Cmd::Edge(EdgeCmd::Uninstall) => service::uninstall("edge"),
        Cmd::Edge(EdgeCmd::Ban { net, duration, reason, remote }) => {
            let net = match net {
                Some(n) => n,
                None => {
                    use std::io::Write;
                    eprint!("IP or CIDR range to ban: ");
                    std::io::stderr().flush()?;
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line)?;
                    line.trim().to_string()
                }
            };
            remote_call(&paths, &remote, "POST", "/bans", &serde_json::json!({"net": net, "secs": duration, "reason": reason}))
        }
        Cmd::Edge(EdgeCmd::Unban { net, remote }) => remote_call(&paths, &remote, "DELETE", "/bans", &serde_json::json!({"net": net})),
        Cmd::Edge(EdgeCmd::Bans { remote }) => remote_call(&paths, &remote, "GET", "/bans", &serde_json::Value::Null),
        Cmd::App { remote, ns, cmd } => {
            set_ns(ns);
            run_app(&paths, &remote, cmd)
        }
        Cmd::Job { remote, ns, cmd } => {
            set_ns(ns);
            run_job(&paths, &remote, cmd)
        }
        Cmd::Ns { remote, cmd } => run_ns(&paths, &remote, cmd),
        Cmd::Import { from, names, images_only, volumes_only, force } => {
            cli::import(&paths, &from, &names, !volumes_only, !images_only, force)
        }
        Cmd::Shell => cli::shell(&paths),
        Cmd::Compact => cli::compact(&paths),
        Cmd::Host(HostCmd::Add { name, dest, no_use, fleet: false, .. }) => host::add(&name, &dest, !no_use),
        Cmd::Host(HostCmd::Add { name, dest, no_use, domain, addr, no_power, no_docker, .. }) => {
            let f = host::Fleet { domain: domain.unwrap_or_default(), addr, no_power, docker: !no_docker };
            host::add_fleet(&paths, &name, &dest, !no_use, &f)
        }
        Cmd::Spare(SpareCmd::Setup { domain, addr, no_power }) => spare::setup(&paths, &domain, addr.as_deref(), no_power),
        Cmd::Spare(SpareCmd::Uninstall) => spare::uninstall(),
        Cmd::Host(HostCmd::Ls { json }) => {
            host::print_list(json);
            Ok(())
        }
        Cmd::Host(HostCmd::Use { name }) => host::use_host(&name),
        Cmd::Host(HostCmd::Rm { name }) => host::rm(&name),
        Cmd::Host(HostCmd::Forward { name, ports }) => host::forward(&paths, &name, &ports),
        Cmd::Socket(Socket::Install) => syssock::install(&paths, cli::status_value(&paths)["running"] == true),
        Cmd::Socket(Socket::Uninstall) => syssock::uninstall(&paths),
        Cmd::Menubar { install: true, .. } => menubar::install(),
        Cmd::Menubar { uninstall: true, .. } => menubar::uninstall(),
        Cmd::Menubar { .. } => menubar::run(paths),
        Cmd::Research(Research::BenchMeta { dir }) => {
            daemon::send_control(&paths, &rekcod_proto::Control::BenchMeta(dir)).map(|r| println!("{r}"))
        }
        Cmd::Daemon { cpus, memory_mib, project, workspace, allow, reach, idle_ttl } => {
            daemon::run(paths, daemon::Options { cpus, memory_mib, project, workspace, allow, reach, idle_ttl })
        }
    });
    if let Err(e) = result {
        eprintln!("rekcod: {e:#}");
        std::process::exit(if e.downcast_ref::<sandbox::NoSandbox>().is_some() { 2 } else { 1 });
    }
}

/// `--ns` (or `REKCOD_NS`) for this run; `fleet::Remote::ns` reads it back.
#[cfg(target_os = "macos")]
fn set_ns(ns: Option<String>) {
    if let Some(n) = ns {
        // Single-threaded here, before any command runs.
        unsafe { std::env::set_var("REKCOD_NS", n) };
    }
}

#[cfg(target_os = "macos")]
fn run_ns(paths: &paths::Paths, remote: &Option<String>, c: NsCmd) -> anyhow::Result<()> {
    use serde_json::json;
    let quota = |c: Option<i64>, m: Option<i64>, s: Option<i64>, a: Option<i64>| json!({"max_cpus": c, "max_memory_gib": m, "max_sandboxes": s, "max_apps": a});
    match c {
        NsCmd::Create { name, max_cpus, max_memory, max_sandboxes, max_apps } => {
            let mut b = quota(max_cpus, max_memory, max_sandboxes, max_apps);
            b["name"] = json!(name);
            remote_call(paths, remote, "POST", "/namespaces", &b)
        }
        NsCmd::Ls => remote_call(paths, remote, "GET", "/namespaces", &serde_json::Value::Null),
        NsCmd::Quota { name, max_cpus, max_memory, max_sandboxes, max_apps } => {
            remote_call(paths, remote, "POST", &format!("/namespaces/{name}/quota"), &quota(max_cpus, max_memory, max_sandboxes, max_apps))
        }
        NsCmd::Member(NsMember::Add { ns, who, role }) => {
            remote_call(paths, remote, "POST", &format!("/namespaces/{ns}/members"), &json!({"who": who, "role": role}))
        }
        NsCmd::Member(NsMember::Rm { ns, who }) => {
            remote_call(paths, remote, "DELETE", &format!("/namespaces/{ns}/members/{who}"), &serde_json::Value::Null)
        }
        NsCmd::Use { name } => fleet::remote_use_ns(paths, remote.as_deref().filter(|r| !r.is_empty()), &name),
        NsCmd::Rm { name, force } => remote_call(paths, remote, "DELETE", &format!("/namespaces/{name}"), &json!({"force": force})),
    }
}

/// Calls the remote controller and prints the JSON answer.
#[cfg(target_os = "macos")]
/// `[http]` from rekcod.toml; null (keep the app's current settings) when there's none.
#[cfg(target_os = "macos")]
fn http_settings(t: &std::collections::HashMap<String, String>) -> anyhow::Result<serde_json::Value> {
    let (rate, burst, max_body) = (t.get("http.rate"), t.get("http.burst"), t.get("http.max_body"));
    if rate.is_none() && burst.is_none() && max_body.is_none() {
        return Ok(serde_json::Value::Null);
    }
    if let Some(r) = rate {
        edge::limit::parse_rate(r)?;
    }
    let burst = burst.map(|b| b.parse::<u32>().map_err(|_| anyhow::anyhow!("rekcod.toml http.burst: {b} isn't a number"))).transpose()?;
    let max_body = max_body.map(|m| edge::limit::parse_size(m)).transpose()?;
    Ok(serde_json::json!({"rate": rate, "burst": burst, "max_body": max_body}))
}

fn remote_call(paths: &paths::Paths, remote: &Option<String>, method: &str, path: &str, body: &serde_json::Value) -> anyhow::Result<()> {
    let r = fleet::remote(paths, remote.as_deref().filter(|r| !r.is_empty()))?;
    let path = if fleet::is_scoped(path) { r.scoped(path) } else { path.to_string() };
    let v = fleet::call(&r.url, method, &path, Some(&r.token), body)?;
    match v.get("logs").and_then(|l| l.as_str()).or(v.get("patch").and_then(|p| p.as_str())) {
        Some(text) => print!("{text}"),
        None => println!("{}", serde_json::to_string_pretty(&v)?),
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn run_remote_sandbox(paths: &paths::Paths, remote: &str, c: SandboxCmd) -> anyhow::Result<()> {
    use anyhow::Context;
    use serde_json::json;
    let remote = Some(remote.to_string());
    let r = fleet::remote(paths, remote.as_deref().filter(|r| !r.is_empty()))?;
    match c {
        SandboxCmd::Create { dir, git, git_ref, name, image, cpus, memory, allow_hosts, offline, ttl, json: as_json, os, .. } => {
            let (origin, head) = match (&git, &git_ref) {
                (Some(g), Some(r)) => (g.clone(), r.clone()),
                _ => {
                    let d = dir.map_or_else(std::env::current_dir, Ok)?;
                    let (o, h) = fleet::git_origin(&d)?;
                    (git.unwrap_or(o), git_ref.unwrap_or(h))
                }
            };
            let body = json!({"git": {"url": origin, "ref": head}, "name": name, "image": image, "cpus": cpus,
                              "memory_gib": memory, "allow_hosts": allow_hosts, "offline": offline, "ttl_secs": ttl, "os": os});
            let v = fleet::call(&r.url, "POST", &r.scoped("/sandboxes"), Some(&r.token), &body)?;
            if as_json {
                println!("{}", serde_json::to_string_pretty(&v)?)
            } else {
                println!("{}", v["id"].as_str().unwrap_or(""))
            }
            Ok(())
        }
        SandboxCmd::Exec { id, workdir, env, timeout, cmd } => {
            let body = serde_json::to_vec(&json!({"cmd": cmd, "workdir": workdir, "env": env, "timeout_secs": timeout}))?;
            let (code, reader) = fleet::send(&r.url, "POST", &r.scoped(format!("/sandboxes/{id}/exec")), Some(&r.token), &body)?;
            let mut exit = 1;
            for line in std::io::BufRead::lines(reader) {
                let v: serde_json::Value = serde_json::from_str(&line?).unwrap_or_default();
                if code >= 400 {
                    anyhow::bail!("{}", v["error"].as_str().unwrap_or("request failed"));
                }
                if let Some(o) = v["stdout"].as_str() {
                    print!("{o}");
                } else if let Some(e) = v["stderr"].as_str() {
                    eprint!("{e}");
                } else if let Some(x) = v["exit"].as_i64() {
                    exit = x as i32;
                }
            }
            std::process::exit(exit)
        }
        SandboxCmd::Ls { .. } => remote_call(paths, &remote, "GET", "/sandboxes", &serde_json::Value::Null),
        SandboxCmd::Diff { id, .. } => remote_call(paths, &remote, "GET", &format!("/sandboxes/{id}/diff"), &serde_json::Value::Null),
        SandboxCmd::Fork { id, name, .. } => remote_call(paths, &remote, "POST", &format!("/sandboxes/{id}/fork"), &json!({"name": name})),
        SandboxCmd::Stop { id } => remote_call(paths, &remote, "POST", &format!("/sandboxes/{id}/stop"), &json!({})),
        SandboxCmd::Rm { ids, .. } => {
            for id in ids {
                fleet::call(&r.url, "DELETE", &r.scoped(format!("/sandboxes/{id}")), Some(&r.token), &serde_json::Value::Null)
                    .with_context(|| id.clone())?;
                println!("{id}");
            }
            Ok(())
        }
        _ => anyhow::bail!("this command isn't offered with --remote (create, exec, ls, diff, fork, stop and rm are)"),
    }
}

#[cfg(target_os = "macos")]
fn run_job(paths: &paths::Paths, remote: &Option<String>, c: JobCmd) -> anyhow::Result<()> {
    use serde_json::json;
    let r = fleet::remote(paths, remote.as_deref().filter(|r| !r.is_empty()))?;
    match c {
        JobCmd::Run { name, image, cpus, memory, env, timeout, every, git, git_ref, detach, cmd } => {
            let (origin, head) = match (&git, &git_ref) {
                (Some(g), Some(r)) => (g.clone(), r.clone()),
                _ => {
                    let (o, h) = fleet::git_origin(&std::env::current_dir()?)?;
                    (git.unwrap_or(o), git_ref.unwrap_or(h))
                }
            };
            let body = json!({"name": name, "image": image, "cpus": cpus, "memory_gib": memory, "env": env,
                              "timeout_secs": timeout, "every_secs": every, "git": {"url": origin, "ref": head}, "cmd": cmd});
            let v = fleet::call(&r.url, "POST", &r.scoped("/jobs"), Some(&r.token), &body)?;
            let Some(id) = v["id"].as_str().filter(|_| !detach) else {
                println!("{}", serde_json::to_string_pretty(&v)?);
                return Ok(());
            };
            eprintln!("rekcod: job {id} on {}", v["node"].as_str().unwrap_or("?"));
            let mut shown = String::new();
            loop {
                let j = fleet::call(&r.url, "GET", &r.scoped(format!("/jobs/{id}")), Some(&r.token), &serde_json::Value::Null)?;
                let log = j["log"].as_str().unwrap_or("");
                print!("{}", new_output(&shown, log));
                shown = log.to_string();
                if j["state"] != "running" {
                    let exit = j["exit"].as_i64().unwrap_or(1);
                    eprintln!("rekcod: job {id} {} (exit {exit})", j["state"].as_str().unwrap_or(""));
                    std::process::exit(exit as i32);
                }
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
        JobCmd::Ls => remote_call(paths, remote, "GET", "/jobs", &serde_json::Value::Null),
        JobCmd::Logs { id } => {
            let j = fleet::call(&r.url, "GET", &r.scoped(format!("/jobs/{id}")), Some(&r.token), &serde_json::Value::Null)?;
            print!("{}", j["log"].as_str().unwrap_or(""));
            Ok(())
        }
        JobCmd::Cancel { id } => remote_call(paths, remote, "POST", &format!("/jobs/{id}/cancel"), &json!({})),
        JobCmd::Unschedule { name } => remote_call(paths, remote, "DELETE", &format!("/schedules/{name}"), &serde_json::Value::Null),
    }
}

/// The part of `log` not printed yet, given the previous poll's `shown`. The node only
/// returns the last 64 KiB, so a long log slides.
fn new_output<'a>(shown: &str, log: &'a str) -> &'a str {
    // The window slid by k bytes: log = shown[k..] + new output. Smallest k wins.
    for (k, _) in shown.char_indices() {
        if log.starts_with(&shown[k..]) {
            return &log[shown.len() - k..];
        }
    }
    log
}

#[cfg(target_os = "macos")]
fn run_app(paths: &paths::Paths, remote: &Option<String>, c: AppCmd) -> anyhow::Result<()> {
    use serde_json::json;
    let vars = |v: &[String]| -> serde_json::Value {
        json!(v.iter().filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), json!(v))).collect::<serde_json::Map<_, _>>())
    };
    match c {
        AppCmd::Deploy { name, port, instances, git, git_ref, public, private, idle_stop, recycle } => {
            let dir = std::env::current_dir()?;
            let t = fleet::parse_toml(&std::fs::read_to_string(dir.join("rekcod.toml")).unwrap_or_default());
            let (origin, head) = match (&git, &git_ref) {
                (Some(g), Some(r)) => (g.clone(), r.clone()),
                _ => {
                    let (o, h) = fleet::git_origin(&dir)?;
                    (git.unwrap_or(o), git_ref.unwrap_or(h))
                }
            };
            let dir_name = dir.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
            let num = |k: &str| t.get(k).and_then(|v| v.parse::<i64>().ok());
            let dur = |k: &str| t.get(k).map(|v| duration_secs(v).map_err(|e| anyhow::anyhow!("rekcod.toml {k}: {e}"))).transpose();
            let env: serde_json::Map<String, serde_json::Value> =
                t.iter().filter_map(|(k, v)| k.strip_prefix("env.").map(|k| (k.to_string(), json!(v)))).collect();
            let body = json!({
                "name": name.or(t.get("app.name").cloned()).unwrap_or(dir_name),
                "git": {"url": origin, "ref": head},
                "port": port.or(num("app.port")),
                "instances": instances.or(num("resources.instances")).unwrap_or(1),
                "cpus": num("resources.cpus").unwrap_or(1),
                "memory_gib": t.get("resources.memory").and_then(|m| m.trim_end_matches("GiB").parse::<i64>().ok()).unwrap_or(1),
                "health": t.get("app.health").cloned(),
                "start": t.get("app.start"),
                "env": env,
                // Flags win over rekcod.toml; neither keeps the app's current visibility.
                "public": if public { Some(true) } else if private { Some(false) } else { t.get("app.public").map(|v| v == "true") },
                "idle_stop_secs": idle_stop.or(dur("resources.idle_stop")?),
                "recycle_secs": recycle.or(dur("resources.recycle")?),
                // Absent keeps the app's setting.
                "bucket": t.get("storage.bucket").map(|v| v == "true"),
                "http": http_settings(&t)?,
            });
            remote_call(paths, remote, "POST", "/apps", &body)
        }
        AppCmd::Ls => remote_call(paths, remote, "GET", "/apps", &serde_json::Value::Null),
        AppCmd::Wake { name } => remote_call(paths, remote, "POST", &format!("/apps/{name}/wake"), &json!({})),
        AppCmd::Recycle { name } => remote_call(paths, remote, "POST", &format!("/apps/{name}/recycle"), &json!({})),
        AppCmd::Public { name } => remote_call(paths, remote, "POST", &format!("/apps/{name}/visibility"), &json!({"public": true})),
        AppCmd::Private { name } => remote_call(paths, remote, "POST", &format!("/apps/{name}/visibility"), &json!({"public": false})),
        AppCmd::Status { name } => remote_call(paths, remote, "GET", &format!("/apps/{name}"), &serde_json::Value::Null),
        AppCmd::Rollback { name, release } => {
            remote_call(paths, remote, "POST", &format!("/apps/{name}/rollback"), &json!({"release": release}))
        }
        AppCmd::Scale { name, instances } => {
            remote_call(paths, remote, "POST", &format!("/apps/{name}/scale"), &json!({"instances": instances}))
        }
        AppCmd::Env(AppVars::Set { name, vars: v }) => {
            remote_call(paths, remote, "POST", &format!("/apps/{name}/env"), &json!({"set": vars(&v)}))
        }
        AppCmd::Env(AppVars::Unset { name, keys }) => {
            remote_call(paths, remote, "POST", &format!("/apps/{name}/env"), &json!({"unset": keys}))
        }
        AppCmd::Secret(AppVars::Set { name, vars: v }) => {
            remote_call(paths, remote, "POST", &format!("/apps/{name}/secrets"), &json!({"set": vars(&v)}))
        }
        AppCmd::Secret(AppVars::Unset { name, keys }) => {
            remote_call(paths, remote, "POST", &format!("/apps/{name}/secrets"), &json!({"unset": keys}))
        }
        AppCmd::Logs { name } => remote_call(paths, remote, "GET", &format!("/apps/{name}/logs"), &serde_json::Value::Null),
        AppCmd::Open { name } => {
            let r = fleet::remote(paths, remote.as_deref().filter(|r| !r.is_empty()))?;
            let v = fleet::call(&r.url, "GET", &r.scoped(format!("/apps/{name}")), Some(&r.token), &serde_json::Value::Null)?;
            let url = v["url"].as_str().unwrap_or_default().to_string();
            std::process::Command::new("/usr/bin/open").arg(&url).status()?;
            println!("{url}");
            Ok(())
        }
        AppCmd::Destroy { name, force } => {
            let path = format!("/apps/{name}");
            match remote_call(paths, remote, "DELETE", &path, &json!({"force": force})) {
                Err(e) if !force && e.to_string().contains("destroy --force") && std::io::IsTerminal::is_terminal(&std::io::stdin()) => {
                    use std::io::Write;
                    eprint!("{e:#}\nDelete the bucket and everything in it? [y/N] ");
                    std::io::stderr().flush()?;
                    let mut line = String::new();
                    std::io::stdin().read_line(&mut line)?;
                    if !line.trim().eq_ignore_ascii_case("y") {
                        anyhow::bail!("kept {name}");
                    }
                    remote_call(paths, remote, "DELETE", &path, &json!({"force": true}))
                }
                r => r,
            }
        }
        AppCmd::Storage { name, cmd: None } => {
            let r = fleet::remote(paths, remote.as_deref().filter(|r| !r.is_empty()))?;
            let mut v = fleet::call(&r.url, "GET", &r.scoped(format!("/apps/{name}/storage")), Some(&r.token), &serde_json::Value::Null)?;
            v.as_object_mut().map(|o| o.remove("list"));
            println!("{}", serde_json::to_string_pretty(&v)?);
            Ok(())
        }
        AppCmd::Storage { name, cmd: Some(AppStorageCmd::Ls) } => {
            let r = fleet::remote(paths, remote.as_deref().filter(|r| !r.is_empty()))?;
            let v = fleet::call(&r.url, "GET", &r.scoped(format!("/apps/{name}/storage")), Some(&r.token), &serde_json::Value::Null)?;
            for o in v["list"].as_array().into_iter().flatten() {
                println!("{:>12}  {}  {}", o["size"], o["modified"].as_str().unwrap_or(""), o["key"].as_str().unwrap_or(""));
            }
            Ok(())
        }
        AppCmd::Storage { name, cmd: Some(AppStorageCmd::Url { key, expires }) } => {
            let r = fleet::remote(paths, remote.as_deref().filter(|r| !r.is_empty()))?;
            let v = fleet::call(
                &r.url,
                "POST",
                &r.scoped(format!("/apps/{name}/storage/url")),
                Some(&r.token),
                &json!({"key": key, "secs": expires}),
            )?;
            println!("{}", v["url"].as_str().unwrap_or(""));
            Ok(())
        }
        AppCmd::Storage { name, cmd: Some(AppStorageCmd::Rotate) } => {
            remote_call(paths, remote, "POST", &format!("/apps/{name}/storage/rotate"), &json!({}))
        }
    }
}

#[cfg(target_os = "macos")]
fn run_sandbox(paths: &paths::Paths, c: SandboxCmd) -> anyhow::Result<()> {
    match c {
        SandboxCmd::Create { dir, name, mount, image, golden, cpus, memory, allow_hosts, offline, secrets, ttl, json, os, .. } => {
            let o = sandbox::CreateOptions {
                dir,
                name,
                mount,
                image,
                golden,
                cpus,
                memory_gib: memory,
                allow_hosts,
                offline,
                secrets,
                ttl_secs: ttl,
                os,
                reach: vec![],
            };
            let m = sandbox::create(paths, o)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&m)?)
            } else {
                println!("{}", m.id)
            }
            Ok(())
        }
        SandboxCmd::Exec { id, workdir, env, timeout, cmd } => {
            let tty = unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 };
            let code = sandbox::exec(paths, &id, sandbox::ExecOptions { workdir, env, timeout_secs: timeout, tty }, &cmd)?;
            std::process::exit(code)
        }
        SandboxCmd::Shell { id } => sandbox::shell(paths, &id),
        SandboxCmd::Cp { src, dst } => sandbox::cp(paths, &src, &dst),
        SandboxCmd::Fork { id, name, json } => {
            let m = sandbox::fork(paths, &id, name)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&m)?)
            } else {
                println!("{}", m.id)
            }
            Ok(())
        }
        SandboxCmd::Diff { id, stat } => sandbox::diff(paths, &id, stat),
        SandboxCmd::Apply { id, check } => {
            let ch = sandbox::apply(paths, &id, check)?;
            sandbox::print_changes(&ch);
            if check && !ch.is_empty() {
                std::process::exit(1);
            }
            Ok(())
        }
        SandboxCmd::Ports { id, json } => sandbox::ports(paths, &id, json),
        SandboxCmd::Screenshot { id, out, open } => {
            sandbox::screenshot(paths, &id, &out)?;
            if open {
                std::process::Command::new("/usr/bin/open").arg(&out).status()?;
            }
            println!("{}", out.display());
            Ok(())
        }
        SandboxCmd::Input { id, action } => sandbox::input(paths, &id, &action),
        SandboxCmd::Log { id, follow, json, net } => sandbox::log(paths, &id, follow, json, net),
        SandboxCmd::Ls { json } => sandbox::print_list(paths, json),
        SandboxCmd::Stop { id } => sandbox::stop(paths, &id),
        SandboxCmd::Rm { ids, all } => sandbox::rm(paths, &ids, all),
        SandboxCmd::Serve { listen } => sandbox_api::serve(paths.clone(), listen),
        SandboxCmd::Golden(Golden::Build { name, images }) => sandbox::golden_build(paths, &name, &images),
        SandboxCmd::Golden(Golden::Ls) => sandbox::golden_list(paths),
        SandboxCmd::Golden(Golden::Rm { name }) => sandbox::golden_rm(paths, &name),
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    let _ = Args::parse();
    eprintln!("rekcod currently supports macOS on Apple Silicon only");
    std::process::exit(1);
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    /// Every action the menu bar can run is a real CLI command line.
    #[test]
    fn every_subcommand_reaches_rekcod() {
        use clap::CommandFactory;
        for c in Args::command().get_subcommands() {
            let name = c.get_name().to_string();
            // start/stop/import are shared with docker and decided by their flags.
            if ["start", "stop", "import"].contains(&name.as_str()) {
                continue;
            }
            assert!(route::docker_args(std::slice::from_ref(&name)).is_none(), "`rekcod {name}` would go to docker; add it to route::OWN");
        }
    }

    #[test]
    fn quota_values() {
        assert_eq!(super::quota_value("unlimited"), Ok(-1));
        assert_eq!(super::quota_value("8"), Ok(8));
        assert!(super::quota_value("-1").is_err() && super::quota_value("lots").is_err());
    }

    #[test]
    fn job_output_follows_a_sliding_log() {
        assert_eq!(super::new_output("", "a\nb\n"), "a\nb\n");
        assert_eq!(super::new_output("a\n", "a\nb\n"), "b\n");
        // The node's 64 KiB window moved past the start of what was shown.
        assert_eq!(super::new_output("xxhello\n", "hello\nworld\n"), "world\n");
        assert_eq!(super::new_output("gone", "fresh\n"), "fresh\n");
    }

    #[test]
    fn every_action_is_a_cli_command() {
        use clap::Parser;
        for a in actions::tests::reachable_kinds() {
            let actions::Run::Rekcod(argv) = &a.run else { continue };
            if route::docker_args(argv).is_some() {
                continue; // docker passthrough (container start/stop/rm/logs)
            }
            let full: Vec<&str> = std::iter::once("rekcod").chain(argv.iter().map(String::as_str)).collect();
            assert!(Args::try_parse_from(&full).is_ok(), "{:?} doesn't parse: {argv:?}", a.kind);
        }
    }
}
