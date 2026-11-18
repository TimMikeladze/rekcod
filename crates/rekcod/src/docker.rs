//! Thin wrappers over the `docker` CLI: the user's own for contexts and
//! save/load, or a bundled one when there is none.

use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

use anyhow::{Context, Result, bail};

pub const CONTEXT: &str = "rekcod";

/// The user's docker CLI: `$REKCOD_DOCKER_CLI`, else `docker` on `PATH`.
pub fn system_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("REKCOD_DOCKER_CLI") {
        return Some(PathBuf::from(p));
    }
    std::env::split_paths(&std::env::var_os("PATH")?).map(|d| d.join("docker")).find(|p| p.is_file())
}

pub fn cli() -> Command {
    let mut cmd = Command::new(system_path().unwrap_or_else(|| "docker".into()));
    // An inherited DOCKER_HOST/DOCKER_CONTEXT would silently override --context.
    cmd.env_remove("DOCKER_HOST").env_remove("DOCKER_CONTEXT");
    cmd
}

/// A docker CLI aimed at nothing in particular: the user's, else the bundled one
/// (downloaded on first use) with its own `DOCKER_CONFIG` so its plugins are found.
#[cfg(target_os = "macos")]
pub fn any_cli(paths: &crate::paths::Paths) -> anyhow::Result<Command> {
    if system_path().is_some() {
        return Ok(cli());
    }
    let dir = crate::cli_fetch::ensure_cli(&paths.home)?;
    let mut cmd = Command::new(dir.join("docker"));
    cmd.env_remove("DOCKER_HOST").env_remove("DOCKER_CONTEXT");
    if std::env::var_os("DOCKER_CONFIG").is_none() {
        let config = paths.home.join("docker");
        std::fs::create_dir_all(&config)?;
        let plugins = config.join("cli-plugins");
        let target = dir.join("cli-plugins");
        if std::fs::read_link(&plugins).ok().as_deref() != Some(target.as_path()) {
            let _ = std::fs::remove_file(&plugins);
            std::os::unix::fs::symlink(&target, &plugins).context("link cli-plugins")?;
        }
        cmd.env("DOCKER_CONFIG", config);
    }
    Ok(cmd)
}

pub fn available() -> bool {
    cli().arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

pub fn run(args: &[&str]) -> Result<Output> {
    let out = cli().args(args).stdin(Stdio::null()).output().context("run docker CLI")?;
    Ok(out)
}

pub fn run_ok(args: &[&str]) -> Result<String> {
    let out = run(args)?;
    if !out.status.success() {
        bail!("docker {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

pub fn current_context() -> Result<String> {
    run_ok(&["context", "show"])
}

pub fn context_exists(name: &str) -> bool {
    run(&["context", "inspect", name]).is_ok_and(|o| o.status.success())
}

/// Creates or repoints the `rekcod` context at `host`.
pub fn ensure_context(host: &str) -> Result<()> {
    let endpoint = format!("host={host}");
    if context_exists(CONTEXT) {
        run_ok(&["context", "update", CONTEXT, "--docker", &endpoint])?;
    } else {
        run_ok(&["context", "create", CONTEXT, "--description", "rekcod", "--docker", &endpoint])?;
    }
    Ok(())
}

pub fn use_context(name: &str) -> Result<()> {
    run_ok(&["context", "use", name]).map(|_| ())
}

/// Contexts as the docker CLI keeps them on disk, for when there's no CLI left to ask
/// (Docker Desktop or OrbStack removed before rekcod): `$DOCKER_CONFIG` or `~/.docker`,
/// `contexts/meta/<sha256 of the name>/meta.json`, and `currentContext` in `config.json`.
pub mod stored {
    use std::path::PathBuf;

    use anyhow::{Context, Result};
    use sha2::{Digest, Sha256};

    fn config_dir() -> Option<PathBuf> {
        std::env::var_os("DOCKER_CONFIG").map(PathBuf::from).or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".docker")))
    }

    fn id(name: &str) -> String {
        Sha256::digest(name.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
    }

    /// Every stored context: (name, description).
    pub fn contexts() -> Vec<(String, String)> {
        contexts_in(config_dir())
    }

    fn contexts_in(dir: Option<PathBuf>) -> Vec<(String, String)> {
        let Some(dir) = dir else { return Vec::new() };
        let Ok(entries) = std::fs::read_dir(dir.join("contexts/meta")) else { return Vec::new() };
        entries
            .flatten()
            .filter_map(|e| serde_json::from_slice::<serde_json::Value>(&std::fs::read(e.path().join("meta.json")).ok()?).ok())
            .filter_map(|v| Some((v["Name"].as_str()?.to_string(), v["Metadata"]["Description"].as_str().unwrap_or("").to_string())))
            .collect()
    }

    /// `currentContext` from config.json; "default" when it's unset.
    pub fn current() -> String {
        current_in(config_dir())
    }

    fn current_in(dir: Option<PathBuf>) -> String {
        dir.and_then(|d| std::fs::read(d.join("config.json")).ok())
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| v["currentContext"].as_str().map(String::from))
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "default".into())
    }

    /// Makes `name` current; "default" removes the setting. Other keys are kept.
    pub fn use_context(name: &str) -> Result<()> {
        use_context_in(config_dir().context("no HOME")?, name)
    }

    fn use_context_in(dir: PathBuf, name: &str) -> Result<()> {
        let file = dir.join("config.json");
        let Ok(bytes) = std::fs::read(&file) else {
            return if name == "default" { Ok(()) } else { anyhow::bail!("no {}", file.display()) };
        };
        let mut v: serde_json::Value = serde_json::from_slice(&bytes).with_context(|| format!("read {}", file.display()))?;
        let obj = v.as_object_mut().with_context(|| format!("{} isn't a JSON object", file.display()))?;
        if name == "default" {
            if obj.remove("currentContext").is_none() {
                return Ok(());
            }
        } else {
            obj.insert("currentContext".into(), name.into());
        }
        let tmp = file.with_extension("json.rekcod");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&v)?)?;
        std::fs::rename(&tmp, &file).with_context(|| format!("write {}", file.display()))
    }

    /// Deletes `name`'s metadata and TLS files, and stops it being current.
    pub fn remove(name: &str) -> Result<()> {
        remove_in(config_dir().context("no HOME")?, name)
    }

    fn remove_in(dir: PathBuf, name: &str) -> Result<()> {
        if current_in(Some(dir.clone())) == name {
            use_context_in(dir.clone(), "default")?;
        }
        for kind in ["meta", "tls"] {
            let d = dir.join("contexts").join(kind).join(id(name));
            if d.exists() {
                std::fs::remove_dir_all(&d).with_context(|| format!("remove {}", d.display()))?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn contexts_are_read_and_removed_without_the_cli() {
            let dir = std::env::temp_dir().join(format!("rekcod-stored-ctx-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            // The id the docker CLI uses for "rekcod" (checked against a real ~/.docker).
            assert_eq!(id("rekcod"), "fbfedbb7e0dc41171d8168dab4ce703eabcbeacc1c855c2439b632c4ee18cc1f");
            for (n, d) in [("rekcod", "rekcod"), ("rekcod-mini", "rekcod host mini"), ("colima", "")] {
                let m = dir.join("contexts/meta").join(id(n));
                std::fs::create_dir_all(&m).unwrap();
                std::fs::write(m.join("meta.json"), format!(r#"{{"Name":"{n}","Metadata":{{"Description":"{d}"}}}}"#)).unwrap();
            }
            std::fs::create_dir_all(dir.join("contexts/tls").join(id("rekcod"))).unwrap();
            std::fs::write(dir.join("config.json"), r#"{"auths":{"x":{}},"currentContext":"rekcod"}"#).unwrap();
            let mut all = contexts_in(Some(dir.clone()));
            all.sort();
            assert_eq!(all[1], ("rekcod".into(), "rekcod".into()));
            assert_eq!(current_in(Some(dir.clone())), "rekcod");

            remove_in(dir.clone(), "rekcod").unwrap();
            assert_eq!(current_in(Some(dir.clone())), "default");
            assert!(!dir.join("contexts/meta").join(id("rekcod")).exists() && !dir.join("contexts/tls").join(id("rekcod")).exists());
            let config: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.join("config.json")).unwrap()).unwrap();
            assert_eq!(config["auths"]["x"], serde_json::json!({}), "other settings kept");
            assert_eq!(contexts_in(Some(dir.clone())).len(), 2);

            use_context_in(dir.clone(), "colima").unwrap();
            assert_eq!(current_in(Some(dir.clone())), "colima");
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }
}
