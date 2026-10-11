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

#[cfg(target_os = "macos")]
mod cli;
#[cfg(target_os = "macos")]
mod daemon;
#[cfg(target_os = "macos")]
mod vm;
#[cfg(target_os = "macos")]
mod watch;

#[derive(Parser)]
#[command(name = "rekcod", version, about = "Docker on macOS: real dockerd in a fast VM, switched in one command")]
struct Args {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Boot the VM and make `rekcod` the active docker context.
    Start {
        /// vCPUs (default: all host cores).
        #[arg(long)]
        cpus: Option<usize>,
        /// Memory in GiB (default: half of host RAM, at most 8). Unused memory is not reserved.
        #[arg(long)]
        memory: Option<u64>,
        /// Size of the data disk in GiB, used when it is first created (sparse).
        #[arg(long, default_value_t = 64)]
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
    /// Show VM state, versions and forwarded ports.
    Status,
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
    #[command(hide = true, subcommand)]
    Research(Research),
    #[command(hide = true)]
    Daemon {
        #[arg(long)]
        cpus: usize,
        #[arg(long)]
        memory_mib: u64,
    },
}

/// Phase 6 prototypes (docs/research.md). The daemon must run with REKCOD_RESEARCH=1.
#[derive(Subcommand)]
enum Research {
    /// Cold lstat latency: host metadata server over vsock vs virtio-fs.
    BenchMeta { dir: String },
}

#[cfg(target_os = "macos")]
fn main() {
    let args = Args::parse();
    if !matches!(args.command, Cmd::Daemon { .. }) {
        // Behave like other CLIs when piped into `head` (the daemon keeps Rust's SIGPIPE-ignore).
        unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) };
    }
    let result = paths::Paths::new().and_then(|paths| match args.command {
        Cmd::Start { cpus, memory, disk, no_switch } => cli::start(&paths, cpus, memory, disk, no_switch),
        Cmd::Stop { no_switch } => cli::stop(&paths, no_switch),
        Cmd::Status => cli::status(&paths),
        Cmd::Import { from, names, images_only, volumes_only, force } => {
            cli::import(&paths, &from, &names, !volumes_only, !images_only, force)
        }
        Cmd::Shell => cli::shell(&paths),
        Cmd::Compact => cli::compact(&paths),
        Cmd::Research(Research::BenchMeta { dir }) => {
            daemon::send_control(&paths, &rekcod_proto::Control::BenchMeta(dir)).map(|r| println!("{r}"))
        }
        Cmd::Daemon { cpus, memory_mib } => daemon::run(paths, daemon::Options { cpus, memory_mib }),
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
