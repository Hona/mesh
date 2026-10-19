//! Plain data shared by every layer.

use serde::{Deserialize, Serialize};

/// `<state>/state.json`
#[derive(Serialize, Deserialize)]
pub(crate) struct State {
    pub registry: RegistryConfig,
    pub device: String,
    /// The shell `exec -c` uses here, recorded by `mesh up`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<Shell>,
    #[serde(default)]
    pub synced: Option<String>,
}

#[derive(Serialize, Deserialize, Clone)]
pub(crate) struct Shell {
    pub name: String,
    pub path: String,
}

/// Where the mesh lives (`state.json` `registry`).
#[derive(Serialize, Deserialize, PartialEq)]
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
    #[serde(default)]
    pub interval: Option<String>,
}

/// `devices/<name>.json` plus its `.pub` / `.host.pub`. The name comes from the file name.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub(crate) struct Device {
    #[serde(skip_deserializing)]
    pub name: String,
    /// Network host name; the network backend resolves it to an address.
    pub host: String,
    pub os: String,
    pub arch: String,
    pub shell: String,
    pub user: String,
    /// Absolute path of mesh on that device.
    pub mesh: String,
    #[serde(skip)]
    pub key: Option<String>,
    #[serde(skip)]
    pub host_key: Option<String>,
}

/// What a remote command runs (SPEC §6.2). Unknown fields are ignored.
#[derive(Serialize, Deserialize, Default, Clone)]
#[serde(default)]
pub(crate) struct Envelope {
    pub v: u32,
    pub from: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub argv: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub env: std::collections::BTreeMap<String, String>,
    pub tty: bool,
    /// Run outside the SSH session so what it starts outlives it (SPEC §5.5).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub durable: bool,
    /// Seconds.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout: Option<f64>,
    /// Mux only: which device the command is for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
}

/// How a remote command ended (the `exit` frame).
#[derive(Serialize, Deserialize, Clone, Default)]
pub(crate) struct ExitInfo {
    pub code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// One device's result in `mesh exec --json` / MCP `exec` (SPEC §5.5).
#[derive(Serialize, Default)]
pub(crate) struct ExecResult {
    pub device: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
pub(crate) struct ExecReport {
    pub results: Vec<ExecResult>,
}

/// One line of `mesh up` output.
#[derive(Serialize)]
pub(crate) struct Step {
    /// "ok" ✓, "warn" !, "fail" ✗, "info" …
    pub status: &'static str,
    pub name: String,
    pub detail: String,
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

/// The opencode service (SPEC §5.4).
#[derive(Serialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ServiceState {
    /// Its HTTP server answers.
    Ok,
    /// Not registered (no `service.json`).
    Down,
    /// Registered and alive, but no HTTP answer within 4 s.
    Hung,
    /// `service.json` names a dead pid.
    Stale,
}

impl ServiceState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Down => "down",
            Self::Hung => "hung",
            Self::Stale => "stale",
        }
    }

    /// From a device's status JSON: a state, or the bool older versions sent.
    pub fn from_json(v: Option<&serde_json::Value>) -> Self {
        match v {
            Some(serde_json::Value::Bool(true)) => Self::Ok,
            Some(serde_json::Value::String(s)) => match s.as_str() {
                "ok" => Self::Ok,
                "hung" => Self::Hung,
                "stale" => Self::Stale,
                _ => Self::Down,
            },
            _ => Self::Down,
        }
    }
}

#[derive(Serialize, Clone)]
pub(crate) struct Opencode {
    /// The opencode `mesh exec <device> opencode` runs (SPEC §6.4).
    pub version: Option<String>,
    pub busy: Option<u64>,
    pub service: ServiceState,
    /// The running service's version, only when it differs from `version`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_version: Option<String>,
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

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub(crate) struct SyncReport {
    pub commit: Option<String>,
    pub up_to_date: bool,
    /// "+ device air", "~ skill review", "~ AGENTS.md"
    pub changes: Vec<String>,
    /// Local files regenerated: "ssh config", "known_hosts", "keys"
    pub applied: Vec<String>,
    pub warnings: Vec<String>,
}

/// A release of the tool repo.
pub(crate) struct Release {
    pub tag: String,
    pub prerelease: bool,
}

/// Semver order: `0.1.0-dev.2 < 0.1.0-dev.10 < 0.1.0 < 0.2.0` (a leading `v` is ignored).
pub(crate) fn cmp_versions(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let parts = |v: &str| {
        let v = v.trim().trim_start_matches('v');
        let (core, pre) = v
            .split_once('-')
            .map_or((v, None), |(c, p)| (c, Some(p.to_string())));
        let nums: Vec<u64> = core.split('.').map(|n| n.parse().unwrap_or(0)).collect();
        (nums, pre)
    };
    let ((na, pa), (nb, pb)) = (parts(a), parts(b));
    na.cmp(&nb).then_with(|| match (pa, pb) {
        (None, None) => Ordering::Equal,
        (None, Some(_)) => Ordering::Greater,
        (Some(_), None) => Ordering::Less,
        (Some(x), Some(y)) => {
            let ids = |s: &str| s.split('.').map(String::from).collect::<Vec<_>>();
            let (x, y) = (ids(&x), ids(&y));
            for (i, j) in x.iter().zip(&y) {
                let o = match (i.parse::<u64>(), j.parse::<u64>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    (Ok(_), Err(_)) => Ordering::Less,
                    (Err(_), Ok(_)) => Ordering::Greater,
                    _ => i.cmp(j),
                };
                if o != Ordering::Equal {
                    return o;
                }
            }
            x.len().cmp(&y.len())
        }
    })
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
