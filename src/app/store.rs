//! Local state dir: state.json, the repo cache + index, backups.

use super::Fail;
use crate::host::paths;
use crate::model::{Device, Mesh, State, valid_name};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Mesh-relative path -> content id of the cached copy.
pub(crate) type Index = BTreeMap<String, String>;

fn state_file() -> PathBuf {
    paths::state_dir().join("state.json")
}

pub(crate) fn read_state() -> Result<State, Fail> {
    let p = state_file();
    let bytes = match fs::read(&p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(Fail(
                3,
                format!(
                    "this device isn't in a mesh ({} missing): run `mesh up`",
                    p.display()
                ),
            ));
        }
        Err(e) => return Err(format!("{}: {e}", p.display()).into()),
    };
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", p.display()).into())
}

pub(crate) fn write_state(s: &State) -> Result<(), String> {
    let text = serde_json::to_string_pretty(s).map_err(|e| e.to_string())? + "\n";
    write_file(&state_file(), text.as_bytes())
}

/// Seconds since the last successful sync (state.json is rewritten by each one).
pub(crate) fn state_age_secs() -> Option<u64> {
    fs::metadata(state_file())
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()
        .map(|d| d.as_secs())
}

pub(crate) fn write_file(p: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(dir) = p.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    fs::write(p, bytes).map_err(|e| format!("{}: {e}", p.display()))
}

fn cache_dir() -> PathBuf {
    paths::state_dir().join("cache")
}

pub(crate) fn read_index() -> Index {
    fs::read(cache_dir().join("index.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub(crate) fn write_index(i: &Index) -> Result<(), String> {
    let text = serde_json::to_string_pretty(i).map_err(|e| e.to_string())? + "\n";
    write_file(&cache_dir().join("index.json"), text.as_bytes())
}

/// Cached copy of a mesh file, e.g. "devices/blade.json".
pub(crate) fn cached(rel: &str) -> PathBuf {
    let mut p = cache_dir().join("files");
    p.extend(rel.split('/'));
    p
}

fn read_cached(rel: &str) -> Option<String> {
    fs::read_to_string(cached(rel)).ok()
}

/// The AGENTS.md mesh last wrote to ~/.config/opencode (to detect local edits).
pub(crate) fn applied_agents() -> PathBuf {
    cache_dir().join("applied").join("AGENTS.md")
}

pub(crate) fn new_backup_dir() -> PathBuf {
    paths::state_dir().join("backup").join(timestamp())
}

pub(crate) fn latest_agents_backup() -> Option<PathBuf> {
    let dirs = fs::read_dir(paths::state_dir().join("backup")).ok()?;
    dirs.flatten()
        .map(|e| e.path().join("AGENTS.md"))
        .filter(|p| p.is_file())
        .max()
}

/// Devices in the cache, sorted by name. Bad entries are skipped with a warning.
pub(crate) fn devices(index: &Index, warnings: &mut Vec<String>) -> Vec<Device> {
    let mut out = Vec::new();
    for rel in index.keys() {
        let Some(name) = rel
            .strip_prefix("devices/")
            .and_then(|r| r.strip_suffix(".json"))
        else {
            continue;
        };
        if !valid_name(name) {
            warnings.push(format!("{rel}: invalid device name, ignored"));
            continue;
        }
        let mut d: Device = match serde_json::from_str(&read_cached(rel).unwrap_or_default()) {
            Ok(d) => d,
            Err(e) => {
                warnings.push(format!("{rel}: {e}, ignored"));
                continue;
            }
        };
        d.name = name.to_string();
        if d.host.is_empty() {
            d.host = d.name.clone();
        }
        d.key = read_cached(&format!("devices/{name}.pub"));
        d.host_key = read_cached(&format!("devices/{name}.host.pub"));
        out.push(d);
    }
    out
}

/// The cached devices, reading the index (warnings dropped).
pub(crate) fn all_devices() -> Vec<Device> {
    devices(&read_index(), &mut Vec::new())
}

/// Held while a sync runs; scheduled and manual syncs never overlap.
pub(crate) struct Lock(PathBuf);

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

pub(crate) fn lock_sync() -> Result<Lock, String> {
    use std::io::Write;
    let p = paths::state_dir().join("sync.lock");
    let _ = fs::create_dir_all(paths::state_dir());
    for _ in 0..300 {
        match fs::OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(mut f) => {
                let _ = write!(f, "{}", std::process::id());
                return Ok(Lock(p));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let pid = fs::read_to_string(&p)
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok());
                let holder = pid.and_then(crate::host::process::pid_exe);
                let live =
                    holder.is_some_and(|exe| exe.to_string_lossy().to_lowercase().contains("mesh"));
                if !live {
                    let _ = fs::remove_file(&p); // stale: its process is gone
                    continue;
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
            Err(e) => return Err(format!("{}: {e}", p.display())),
        }
    }
    Err("another mesh sync is still running".into())
}

/// Append-mode log `<state>/log/<name>.log` (rotated at 1 MB) with a header line.
pub(crate) fn log_file(name: &str, header: &str) -> Option<fs::File> {
    use std::io::Write;
    let dir = paths::state_dir().join("log");
    fs::create_dir_all(&dir).ok()?;
    let p = dir.join(format!("{name}.log"));
    if fs::metadata(&p).is_ok_and(|m| m.len() > 1 << 20) {
        let _ = fs::rename(&p, dir.join(format!("{name}.log.1")));
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
        .ok()?;
    let _ = writeln!(f, "== {} {header}", timestamp());
    Some(f)
}

pub(crate) fn mesh() -> Mesh {
    read_cached("mesh.json")
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// UTC, e.g. 20261018T101500Z
fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let (days, t) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    // civil-from-days (H. Hinnant)
    let z = days + 719_468;
    let (era, doe) = (z.div_euclid(146_097), z.rem_euclid(146_097));
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        t / 3600,
        t % 3600 / 60,
        t % 60
    )
}
