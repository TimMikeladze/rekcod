//! Guest agent. Runs under busybox init (respawn) inside the rekcod VM.
//!
//! vsock listeners, one per job (ports in `rekcod-proto`):
//! - DOCKER_PORT: byte pipe to `/var/run/docker.sock` (half-close preserved,
//!   which `docker attach`/`exec -i` rely on).
//! - FORWARD_PORT: reads `tcp:<port>\n` or `udp:<port>\n`, then pipes TCP to the
//!   container behind that published port (proxyport.rs), else `127.0.0.1:<port>`.
//! - CONTROL_PORT: line commands (ping, time, notify, warm, trim, shutdown).
//! - DNS_PORT: the host's resolver channel behind the 127.0.0.1:5353 stub.
//! - REVERSE_PORT: connections the host parks for `host.docker.internal`.
//! - META_PORT: research prototype (meta.rs).

#[cfg(target_os = "linux")]
mod agent;
#[cfg(target_os = "linux")]
mod meta;
#[cfg(target_os = "linux")]
mod proxyport;

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    agent::run()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("rekcod-agent only runs inside the Linux guest");
    std::process::exit(1);
}
