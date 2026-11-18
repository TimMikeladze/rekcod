//! `rekcod.toml`, an app's deploy settings: TOML into the `section.key` map the deploy code
//! reads. Unknown sections and keys are errors, so a typo fails the deploy instead of
//! deploying the default.

use std::collections::{BTreeMap, HashMap};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct File {
    app: Option<App>,
    resources: Option<Resources>,
    env: Option<BTreeMap<String, Scalar>>,
    storage: Option<Storage>,
    http: Option<Http>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct App {
    name: Option<String>,
    port: Option<u16>,
    health: Option<String>,
    start: Option<String>,
    public: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resources {
    instances: Option<u32>,
    spread: Option<String>,
    cpus: Option<u32>,
    /// `"2GiB"`, or a number of GiB.
    memory: Option<Scalar>,
    /// `"15m"`, or seconds.
    idle_stop: Option<Scalar>,
    recycle: Option<Scalar>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Storage {
    bucket: Option<bool>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Http {
    rate: Option<String>,
    burst: Option<u32>,
    max_body: Option<Scalar>,
}

/// A value an env var or a size can be written as.
#[derive(Deserialize)]
#[serde(untagged)]
enum Scalar {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

impl Scalar {
    fn text(&self) -> String {
        match self {
            Scalar::Str(s) => s.clone(),
            Scalar::Int(i) => i.to_string(),
            Scalar::Float(f) => f.to_string(),
            Scalar::Bool(b) => b.to_string(),
        }
    }
}

/// Whole GiB from `"2GiB"`, `"2048MiB"` or `2`; the VM gets memory in GiB.
fn memory_gib(v: &Scalar) -> Result<u64> {
    let bytes = match v {
        Scalar::Int(n) if *n > 0 => return Ok(*n as u64),
        Scalar::Str(s) => crate::edge::limit::parse_size(s)?,
        _ => bail!("give it as \"2GiB\" or a number of GiB"),
    };
    if bytes == 0 || bytes % (1 << 30) != 0 {
        bail!("{} isn't a whole number of GiB", v.text());
    }
    Ok(bytes >> 30)
}

/// `text` as the deploy code reads it: `app.port` -> `3000`, `env.NAME` -> its value.
pub fn parse(text: &str) -> Result<HashMap<String, String>> {
    let f: File = toml::from_str(text).context("rekcod.toml")?;
    let mut out = HashMap::new();
    let mut put = |k: &str, v: Option<String>| {
        if let Some(v) = v {
            out.insert(k.to_string(), v);
        }
    };
    if let Some(a) = f.app {
        put("app.name", a.name);
        put("app.port", a.port.map(|p| p.to_string()));
        put("app.health", a.health);
        put("app.start", a.start);
        put("app.public", a.public.map(|b| b.to_string()));
    }
    if let Some(r) = f.resources {
        put("resources.instances", r.instances.map(|n| n.to_string()));
        put("resources.spread", r.spread);
        put("resources.cpus", r.cpus.map(|n| n.to_string()));
        put(
            "resources.memory",
            r.memory.as_ref().map(memory_gib).transpose().context("rekcod.toml resources.memory")?.map(|g| g.to_string()),
        );
        put("resources.idle_stop", r.idle_stop.map(|v| v.text()));
        put("resources.recycle", r.recycle.map(|v| v.text()));
    }
    for (k, v) in f.env.unwrap_or_default() {
        put(&format!("env.{k}"), Some(v.text()));
    }
    if let Some(s) = f.storage {
        put("storage.bucket", s.bucket.map(|b| b.to_string()));
    }
    if let Some(h) = f.http {
        put("http.rate", h.rate);
        put("http.burst", h.burst.map(|b| b.to_string()));
        put("http.max_body", h.max_body.map(|v| v.text()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the line splitter this replaced got wrong.
    #[test]
    fn values_are_toml_values() {
        let t = parse(
            r#"[app]
start = "sh -c 'node server.js # prod'"
port = 8080 # a comment
[resources]
memory = "2GiB"
[env]
NODE_ENV = 'production'
TITLE = "Issue #1 tracker"
GREETING = "say \"hi\""
WORKERS = 4
"#,
        )
        .unwrap();
        assert_eq!(t["app.start"], "sh -c 'node server.js # prod'");
        assert_eq!(t["app.port"], "8080");
        assert_eq!(t["resources.memory"], "2");
        assert_eq!(t["env.NODE_ENV"], "production");
        assert_eq!(t["env.TITLE"], "Issue #1 tracker");
        assert_eq!(t["env.GREETING"], r#"say "hi""#);
        assert_eq!(t["env.WORKERS"], "4");
    }

    #[test]
    fn mistakes_are_errors() {
        for bad in [
            "[resources]\nmemory = \"512MiB\"\n",
            "[resources]\nmemory = \"1.5GiB\"\n",
            "[resource]\nmemory = \"1GiB\"\n",
            "[resources]\nidle-stop = \"15m\"\n",
            "[app]\nport = \"web\"\n",
            "[app]\ncpus = 2\n",
            "[app\n",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        assert_eq!(parse("[resources]\nmemory = 4\n").unwrap()["resources.memory"], "4");
        assert!(parse("").unwrap().is_empty());
    }

    /// The README's example deploys as written.
    #[test]
    fn readme_example() {
        let readme = include_str!("../../../README.md");
        let start = readme.find("```toml\n[app]").expect("README has a rekcod.toml example") + "```toml\n".len();
        let end = start + readme[start..].find("```").unwrap();
        let t = parse(&readme[start..end]).unwrap();
        assert_eq!((t["app.name"].as_str(), t["resources.memory"].as_str()), ("blog", "1"));
    }
}
