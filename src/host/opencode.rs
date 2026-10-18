//! Finding and asking the local opencode install (SPEC §6.4).

use super::{paths, process, vlog};
use crate::model::Opencode;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::Command;

// Only these fields are read from service.json, never `password`.
#[derive(Deserialize)]
struct ServiceFile {
    version: Option<String>,
    pid: Option<u32>,
}

struct Service {
    version: Option<String>,
    pid: u32,
    /// Executable of the live service process; None if it isn't running.
    exe: Option<PathBuf>,
}

fn service() -> Option<Service> {
    let bytes = std::fs::read(paths::opencode_state_dir().join("service.json")).ok()?;
    let f: ServiceFile = serde_json::from_slice(&bytes).ok()?;
    let pid = f.pid?;
    let exe = process::pid_exe(pid).filter(|p| {
        p.file_name()
            .is_some_and(|n| n.to_string_lossy().to_lowercase().contains("opencode"))
    });
    Some(Service {
        version: f.version,
        pid,
        exe,
    })
}

/// PATH, then the running service's executable, then known install locations.
fn find(svc: Option<&Service>) -> Option<(PathBuf, String)> {
    if let Some(p) = paths::on_path("opencode") {
        return Some((p, "PATH".into()));
    }
    if let Some(Service {
        exe: Some(exe),
        pid,
        ..
    }) = svc
    {
        return Some((exe.clone(), format!("service pid {pid}")));
    }
    known()
        .into_iter()
        .find(|p| p.is_file())
        .map(|p| (p, "known location".into()))
}

fn known() -> Vec<PathBuf> {
    let home = paths::home();
    let cli = if cfg!(windows) {
        "opencode-cli.exe"
    } else {
        "opencode-cli"
    };
    let mut v = Vec::new();
    // Desktop app: newest versioned CLI dir, e.g. cli\2.0.32-x64\opencode-cli.exe
    if let Ok(rd) = std::fs::read_dir(
        paths::app_data_dir()
            .join("ai.opencode.desktop")
            .join("cli"),
    ) {
        let newest = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.join(cli).is_file())
            .max_by_key(|p| version_key(p));
        v.extend(newest.map(|d| d.join(cli)));
    }
    if cfg!(windows) {
        v.extend(
            paths::env_dir("LOCALAPPDATA")
                .map(|l| l.join(r"Programs\@opencodedesktop\resources").join(cli)),
        );
        v.push(home.join(r".opencode\bin\opencode.exe"));
    } else {
        if cfg!(target_os = "macos") {
            v.push(PathBuf::from("/Applications/OpenCode.app/Contents/MacOS").join(cli));
        }
        v.push(home.join(".opencode/bin/opencode"));
        for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
            v.push(PathBuf::from(dir).join("opencode"));
        }
    }
    v
}

/// "2.0.32-x64" -> [2, 0, 32]
fn version_key(p: &Path) -> Vec<u64> {
    let name = p.file_name().unwrap_or_default().to_string_lossy();
    name.split('-')
        .next()
        .unwrap_or("")
        .split('.')
        .map(|s| s.parse().unwrap_or(0))
        .collect()
}

fn version(bin: &Path) -> Option<String> {
    let out = Command::new(bin).arg("--version").output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    Some(
        s.split_whitespace()
            .last()?
            .trim_start_matches('v')
            .to_string(),
    )
}

fn busy(bin: &Path) -> Option<u64> {
    let out = Command::new(bin)
        .args(["api", "session.active"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    match &v["data"] {
        serde_json::Value::Object(m) => Some(m.len() as u64),
        serde_json::Value::Array(a) => Some(a.len() as u64),
        _ => None,
    }
}

/// Local opencode facts; None if opencode isn't installed. Never starts the
/// service: version comes from service.json and busy is only asked of a running one.
pub(crate) fn probe() -> Option<Opencode> {
    let svc = service();
    let (bin, how) = find(svc.as_ref())?;
    vlog!("opencode: {} ({how})", bin.display());
    match svc {
        Some(Service {
            exe: Some(_),
            version: v,
            ..
        }) => Some(Opencode {
            version: v.or_else(|| version(&bin)),
            busy: busy(&bin),
            service: true,
        }),
        _ => Some(Opencode {
            version: version(&bin),
            busy: None,
            service: false,
        }),
    }
}

/// `opencode reload` if the service is running. Ok(false) if it isn't.
pub(crate) fn reload() -> Result<bool, String> {
    let svc = service();
    if !svc.as_ref().is_some_and(|s| s.exe.is_some()) {
        return Ok(false);
    }
    let (bin, how) = find(svc.as_ref()).ok_or("opencode not found")?;
    vlog!("opencode: {} ({how})", bin.display());
    let out = Command::new(bin)
        .arg("reload")
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(true)
}
