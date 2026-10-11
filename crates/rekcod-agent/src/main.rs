//! Guest agent. Runs under busybox init (respawn) inside the rekcod VM.
//!
//! Three vsock listeners, one per job (ports in `rekcod-proto`):
//! - DOCKER_PORT: byte pipe to `/var/run/docker.sock` (half-close preserved,
//!   which `docker attach`/`exec -i` rely on).
//! - FORWARD_PORT: reads `tcp:<port>\n`, then pipes to `127.0.0.1:<port>`,
//!   where docker-proxy listens for published ports.
//! - CONTROL_PORT: line commands (ping, time, shutdown).

#[cfg(target_os = "linux")]
mod agent;

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    agent::run()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("rekcod-agent only runs inside the Linux guest");
    std::process::exit(1);
}
