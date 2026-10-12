//! Decides whether `rekcod <args>` is a rekcod command or a docker one
//! (docs/cli-passthrough.md).

/// rekcod's own top-level commands that docker doesn't have.
const OWN: &[&str] = &["status", "shell", "compact", "help", "daemon", "research", "-h", "--help", "-V", "--version"];

/// `start`/`stop` flags; anything else means docker's container start/stop.
const VM_FLAGS_WITH_VALUE: &[&str] = &["--cpus", "--memory", "--disk"];
const VM_FLAGS: &[&str] = &["--no-switch", "-h", "--help"];

/// The docker arguments for `args` (argv without the program name), or `None`
/// when rekcod should handle it itself.
pub fn docker_args(args: &[String]) -> Option<&[String]> {
    let first = args.first()?.as_str();
    match first {
        "docker" => Some(&args[1..]),
        _ if OWN.contains(&first) => None,
        "start" | "stop" => (!only_vm_flags(&args[1..])).then_some(args),
        "import" => (!args[1..].iter().any(|a| a == "--from" || a.starts_with("--from="))).then_some(args),
        _ => Some(args),
    }
}

fn only_vm_flags(rest: &[String]) -> bool {
    let mut it = rest.iter();
    while let Some(a) = it.next() {
        let flag = a.split('=').next().unwrap_or(a);
        if VM_FLAGS_WITH_VALUE.contains(&flag) {
            if !a.contains('=') && it.next().is_none() {
                return true; // let clap report the missing value
            }
        } else if !VM_FLAGS.contains(&a.as_str()) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(s: &str) -> Option<String> {
        let args: Vec<String> = s.split_whitespace().map(String::from).collect();
        docker_args(&args).map(|a| a.join(" "))
    }

    #[test]
    fn own_commands_stay() {
        for s in ["", "start", "stop", "start --cpus 4 --memory=8", "stop --no-switch", "status", "shell", "--version", "import --from orbstack", "import --from=x redis:7"] {
            assert_eq!(route(s), None, "{s:?}");
        }
    }

    #[test]
    fn docker_commands_pass_through() {
        assert_eq!(route("ps -a").as_deref(), Some("ps -a"));
        assert_eq!(route("compose up -d").as_deref(), Some("compose up -d"));
        assert_eq!(route("version").as_deref(), Some("version"));
        assert_eq!(route("start web").as_deref(), Some("start web"));
        assert_eq!(route("stop -t 5 web").as_deref(), Some("stop -t 5 web"));
        assert_eq!(route("start --cpus 2 web").as_deref(), Some("start --cpus 2 web"));
        assert_eq!(route("import rootfs.tar app:1").as_deref(), Some("import rootfs.tar app:1"));
        assert_eq!(route("docker start").as_deref(), Some("start"));
        assert_eq!(route("docker status").as_deref(), Some("status"));
    }
}
