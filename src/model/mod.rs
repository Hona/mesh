//! Plain data shared by every layer.

use serde::{Deserialize, Serialize};

/// `<state>/state.json`
#[derive(Serialize, Deserialize)]
pub(crate) struct State {
    pub registry: RegistryConfig,
    pub device: String,
    #[serde(default)]
    pub synced: Option<String>,
}

/// Where the mesh lives (`state.json` `registry`).
#[derive(Serialize, Deserialize)]
pub(crate) struct RegistryConfig {
    pub kind: String,
    pub repo: String,
    #[serde(default)]
    pub path: String,
    pub branch: String,
}

/// `mesh.json`
#[derive(Deserialize, Default)]
pub(crate) struct Mesh {
    #[serde(default)]
    pub pin: Option<String>,
}

/// `devices/<name>.json` plus its `.pub` / `.host.pub`.
#[derive(Deserialize, Default)]
#[serde(default)]
pub(crate) struct Device {
    #[serde(skip)]
    pub name: String,
    /// Network host name; the network backend resolves it to an address.
    pub host: String,
    pub os: String,
    pub arch: String,
    pub shell: String,
    pub user: String,
    #[serde(skip)]
    pub key: Option<String>,
    #[serde(skip)]
    pub host_key: Option<String>,
}

/// A file in the mesh registry, path relative to the mesh root.
pub(crate) struct RepoFile {
    pub path: String,
    /// Content id (changes whenever the content does).
    pub id: String,
    pub exec: bool,
}

/// What the network knows about a host.
pub(crate) struct Peer {
    pub address: Option<String>,
    pub online: bool,
}

#[derive(Serialize)]
pub(crate) struct Opencode {
    pub version: Option<String>,
    pub busy: Option<u64>,
    pub service: bool,
}

#[derive(Serialize)]
pub(crate) struct DeviceStatus {
    pub name: String,
    #[serde(rename = "self")]
    pub me: bool,
    pub os: String,
    pub arch: String,
    pub shell: String,
    pub user: String,
    pub online: Option<bool>,
    pub reachable: Option<bool>,
    pub latency_ms: Option<u64>,
    pub opencode: Option<Opencode>,
    pub mesh: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct Status {
    #[serde(rename = "self")]
    pub me: String,
    pub devices: Vec<DeviceStatus>,
    #[serde(skip)]
    pub synced: Option<String>,
    #[serde(skip)]
    pub synced_ago_secs: Option<u64>,
    #[serde(skip)]
    pub pin: Option<String>,
    #[serde(skip)]
    pub agents_backup: Option<String>,
    #[serde(skip)]
    pub warnings: Vec<String>,
}

#[derive(Serialize, Default)]
pub(crate) struct SyncReport {
    pub commit: Option<String>,
    pub up_to_date: bool,
    /// "+ device air", "~ skill review", "~ AGENTS.md"
    pub changes: Vec<String>,
    /// Local files regenerated: "ssh config", "known_hosts", "keys", "opencode reload"
    pub applied: Vec<String>,
    pub warnings: Vec<String>,
}

pub(crate) fn valid_name(n: &str) -> bool {
    let b = n.as_bytes();
    (2..=63).contains(&b.len())
        && b[0] != b'-'
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
        && n != "all"
        && n != "self"
}

enum Sel {
    All,
    Me,
    Os(String),
    Name(String),
}

/// SPEC §5.1 target: `name`, `a,b`, `all`, `self`, `os:<os>`, minus `--exclude`.
pub(crate) struct Target {
    include: Vec<Sel>,
    exclude: Vec<Sel>,
}

impl Target {
    pub(crate) fn parse(spec: &str, exclude: Option<&str>) -> Result<Target, String> {
        Ok(Target {
            include: parse_list(spec)?,
            exclude: exclude.map(parse_list).transpose()?.unwrap_or_default(),
        })
    }

    /// Device names selected, in `devices` order. Unknown names are an error.
    pub(crate) fn resolve(&self, devices: &[Device], me: &str) -> Result<Vec<String>, String> {
        let pick = |sels: &[Sel]| -> Result<Vec<String>, String> {
            let mut out = Vec::new();
            for sel in sels {
                let hit = |d: &Device| match sel {
                    Sel::All => true,
                    Sel::Me => d.name == me,
                    Sel::Os(os) => d.os == *os,
                    Sel::Name(n) => d.name == *n,
                };
                let names: Vec<&str> = devices
                    .iter()
                    .filter(|d| hit(d))
                    .map(|d| d.name.as_str())
                    .collect();
                match sel {
                    Sel::Me if names.is_empty() => {
                        return Err(format!("this device ({me}) is not in the mesh"));
                    }
                    Sel::Name(n) if names.is_empty() => return Err(format!("unknown device: {n}")),
                    _ => {}
                }
                out.extend(names.into_iter().map(String::from));
            }
            Ok(out)
        };
        let (inc, exc) = (pick(&self.include)?, pick(&self.exclude)?);
        Ok(devices
            .iter()
            .map(|d| d.name.clone())
            .filter(|n| inc.contains(n) && !exc.contains(n))
            .collect())
    }
}

fn parse_list(spec: &str) -> Result<Vec<Sel>, String> {
    spec.split(',')
        .map(|item| match item.trim() {
            "all" => Ok(Sel::All),
            "self" => Ok(Sel::Me),
            s if s.starts_with("os:") => match &s[3..] {
                os @ ("linux" | "macos" | "windows") => Ok(Sel::Os(os.to_string())),
                os => Err(format!("unknown os: {os} (linux, macos, windows)")),
            },
            s if valid_name(s) => Ok(Sel::Name(s.to_string())),
            s => Err(format!("bad target: {s:?}")),
        })
        .collect()
}
