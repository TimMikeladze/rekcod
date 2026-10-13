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
mod fetch;
#[cfg(target_os = "macos")]
mod menubar;
#[cfg(target_os = "macos")]
mod sandbox;
#[cfg(target_os = "macos")]
mod sandbox_api;
#[cfg(target_os = "macos")]
mod snap;
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
    #[command(subcommand)]
    Sandbox(SandboxCmd),
    /// MCP server on stdio exposing the sandbox commands to agents.
    Mcp,
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
    },
    /// Put the project's volumes back to a snapshot.
    Restore {
        name: Option<String>,
        #[arg(short = 'p', long)]
        project: Option<String>,
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
        Cmd::Snap(Snap::Save { name, project }) => snap::save(&paths, project.as_deref(), name.as_deref()),
        Cmd::Snap(Snap::Restore { name, project }) => snap::restore(&paths, project.as_deref(), name.as_deref()),
        Cmd::Snap(Snap::Ls { project, all, json }) => snap::list(&paths, project.as_deref(), all, json),
        Cmd::Snap(Snap::Rm { name, project }) => snap::remove(&paths, project.as_deref(), &name),
        Cmd::X { image, publish, env, no_autoports, sandbox, offline, secrets, keep, cmd } => {
            let opts = xrun::Options { image, publish, env, no_autoports, sandbox, offline, secrets, keep };
            xrun::run(&paths, &cmd, opts).map(|_| ())
        }
        Cmd::Sandbox(c) => run_sandbox(&paths, c),
        Cmd::Mcp => sandbox_api::mcp(paths),
        Cmd::Import { from, names, images_only, volumes_only, force } => {
            cli::import(&paths, &from, &names, !volumes_only, !images_only, force)
        }
        Cmd::Shell => cli::shell(&paths),
        Cmd::Compact => cli::compact(&paths),
        Cmd::Socket(Socket::Install) => syssock::install(&paths, cli::status_value(&paths)["running"] == true),
        Cmd::Socket(Socket::Uninstall) => syssock::uninstall(&paths),
        Cmd::Menubar { install: true, .. } => menubar::install(),
        Cmd::Menubar { uninstall: true, .. } => menubar::uninstall(),
        Cmd::Menubar { .. } => menubar::run(paths),
        Cmd::Research(Research::BenchMeta { dir }) => {
            daemon::send_control(&paths, &rekcod_proto::Control::BenchMeta(dir)).map(|r| println!("{r}"))
        }
        Cmd::Daemon { cpus, memory_mib, project, workspace, allow, idle_ttl } => {
            daemon::run(paths, daemon::Options { cpus, memory_mib, project, workspace, allow, idle_ttl })
        }
    });
    if let Err(e) = result {
        eprintln!("rekcod: {e:#}");
        std::process::exit(if e.downcast_ref::<sandbox::NoSandbox>().is_some() { 2 } else { 1 });
    }
}

#[cfg(target_os = "macos")]
fn run_sandbox(paths: &paths::Paths, c: SandboxCmd) -> anyhow::Result<()> {
    match c {
        SandboxCmd::Create { dir, name, mount, image, golden, cpus, memory, allow_hosts, offline, secrets, ttl, json } => {
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
