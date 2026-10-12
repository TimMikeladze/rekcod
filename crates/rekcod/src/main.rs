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
        Cmd::Daemon { cpus, memory_mib, project } => daemon::run(paths, daemon::Options { cpus, memory_mib, project }),
    });
    if let Err(e) = result {
        eprintln!("rekcod: {e:#}");
        std::process::exit(1);
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
