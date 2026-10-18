//! Finding and asking the local opencode install (SPEC §6.4).

use super::{paths, process, vlog};
use crate::model::Opencode;
use serde::Deserialize;
use std::path::{Path, PathBuf};

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
    // Desktop apps (stable and beta): each versioned CLI dir, e.g.
    // ai.opencode.desktop.beta\cli\0.0.0-beta-21153-x64\opencode-cli.exe, and the bundled copies.
    let mut desktop: Vec<PathBuf> = ["ai.opencode.desktop", "ai.opencode.desktop.beta"]
        .iter()
        .flat_map(|app| {
            std::fs::read_dir(paths::app_data_dir().join(app).join("cli"))
                .into_iter()
                .flatten()
        })
        .flatten()
        .map(|e| e.path().join(cli))
        .collect();
    if cfg!(windows) {
        desktop.extend(
            paths::env_dir("LOCALAPPDATA")
                .map(|l| l.join(r"Programs\@opencodedesktop\resources").join(cli)),
        );
    } else if cfg!(target_os = "macos") {
        for app in ["OpenCode", "OpenCode Beta"] {
            desktop
                .push(PathBuf::from(format!("/Applications/{app}.app/Contents/MacOS")).join(cli));
        }
    }
    // Beta versions (0.0.0-beta-N) sort below stable ones, so "newest" = most recently installed.
    let modified = |p: &PathBuf| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    let mut v: Vec<PathBuf> = desktop
        .into_iter()
        .filter(|p| p.is_file())
        .max_by_key(modified)
        .into_iter()
        .collect();
    if cfg!(windows) {
        v.push(home.join(r".opencode\bin\opencode.exe"));
    } else {
        v.push(home.join(".opencode/bin/opencode"));
        v.extend(paths::unix_bin_dirs("opencode"));
    }
    v
}

fn version(bin: &Path) -> Option<String> {
    let out = process::command(bin).arg("--version").output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    Some(
        s.split_whitespace()
            .last()?
            .trim_start_matches('v')
            .to_string(),
    )
}

fn busy(bin: &Path) -> Option<u64> {
    let out = process::command(bin)
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

/// Local opencode facts; None if opencode isn't installed. `version` is the opencode that
/// `mesh exec … opencode` runs (§6.4); the running service's version is added only when it
/// differs. Never starts the service: busy is only asked of a running one.
pub(crate) fn probe() -> Option<Opencode> {
    let svc = service();
    let (bin, how) = find(svc.as_ref())?;
    vlog!("opencode: {} ({how})", bin.display());
    let running = svc
        .as_ref()
        .and_then(|s| s.exe.as_ref().map(|exe| (exe.clone(), s.version.clone())));
    let up = running.is_some();
    // exec runs the service's own binary: service.json already has its version, no spawn
    let known = running
        .as_ref()
        .filter(|(exe, _)| *exe == bin)
        .and_then(|(_, v)| v.clone());
    let (exec_version, busy) = std::thread::scope(|s| {
        let v = known.is_none().then(|| s.spawn(|| version(&bin)));
        let b = up.then(|| busy(&bin)).flatten();
        (
            known
                .clone()
                .or_else(|| v.and_then(|h| h.join().ok().flatten())),
            b,
        )
    });
    let service_version = running
        .and_then(|(_, v)| v)
        .filter(|v| Some(v) != exec_version.as_ref());
    Some(Opencode {
        version: exec_version,
        busy,
        service: up,
        service_version,
    })
}

/// Started by opencode (an agent's shell, a terminal in the app, an MCP server): opencode puts
/// `OPENCODE=1` in the environment of everything it starts.
pub(crate) fn inside() -> bool {
    std::env::var_os("OPENCODE").is_some() || std::env::var_os("OPENCODE_SESSION_ID").is_some()
}

/// `opencode service restart` (the service's own binary, so the desktop app accepts the new one)
/// with `dir` added to PATH: the new service, and the shells and MCP servers it starts, inherit
/// it. Refused while any session runs. Ok(false): no service is running, nothing to restart.
pub(crate) fn restart_service(dir: &Path) -> Result<bool, String> {
    let Some(Service {
        exe: Some(bin),
        pid: old,
        ..
    }) = service()
    else {
        return Ok(false);
    };
    match busy(&bin) {
        Some(0) => {}
        Some(n) => return Err(format!("{n} session(s) running")),
        None => return Err("can't tell whether sessions are running".into()),
    }
    let cur = std::env::var_os("PATH").unwrap_or_default();
    let mut path: Vec<PathBuf> = std::env::split_paths(&cur).collect();
    if !path.iter().any(|p| p == dir) {
        path.push(dir.to_path_buf());
    }
    let path = std::env::join_paths(path).map_err(|e| e.to_string())?;
    // No pipes: the new service could inherit them and keep this call from ever returning.
    let st = process::command(&bin)
        .args(["service", "restart"])
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    match service() {
        Some(s) if s.exe.is_some() && s.pid != old => Ok(true),
        _ => Err(format!("`opencode service restart` failed ({st})")),
    }
}

/// The opencode binary `mesh exec <device> opencode …` runs (§6.4).
pub(crate) fn bin() -> Option<PathBuf> {
    find(service().as_ref()).map(|(p, _)| p)
}

/// (version, running service pid) for `mesh up`'s preflight line.
pub(crate) fn installed() -> Option<(String, Option<u32>)> {
    let svc = service();
    let (bin, _) = find(svc.as_ref())?;
    let pid = svc.as_ref().filter(|s| s.exe.is_some()).map(|s| s.pid);
    let v = svc
        .and_then(|s| s.exe.and(s.version))
        .or_else(|| version(&bin));
    Some((v.unwrap_or_else(|| "?".into()), pid))
}

/// The global config document holding MCP server `name`, and its command.
fn mcp_doc(name: &str) -> Result<Option<(PathBuf, Vec<String>)>, String> {
    let bin = bin().ok_or("opencode not found")?;
    let out = process::command(bin)
        .args(["debug", "config"])
        .current_dir(paths::home())
        .output()
        .map_err(|e| e.to_string())?;
    let v: serde_json::Value =
        serde_json::from_slice(&out.stdout).map_err(|e| format!("opencode debug config: {e}"))?;
    let global = paths::config_dir();
    for src in v.as_array().into_iter().flatten() {
        let Some(path) = src["path"].as_str().map(PathBuf::from) else {
            continue;
        };
        if path.parent() != Some(global.as_path()) {
            continue;
        }
        for entry in [
            &src["info"]["mcp"]["servers"][name],
            &src["info"]["mcp"][name],
        ] {
            if let Some(cmd) = entry["command"].as_array() {
                return Ok(Some((
                    path,
                    cmd.iter()
                        .filter_map(|c| c.as_str().map(String::from))
                        .collect(),
                )));
            }
        }
    }
    Ok(None)
}

/// Command of the global MCP server entry `name`, if any.
pub(crate) fn mcp_entry(name: &str) -> Result<Option<Vec<String>>, String> {
    Ok(mcp_doc(name)?.map(|(_, cmd)| cmd))
}

/// `opencode mcp add <name> --global -- <command…>`
pub(crate) fn mcp_add(name: &str, command: &[&str]) -> Result<(), String> {
    let bin = bin().ok_or("opencode not found")?;
    let out = process::command(bin)
        .args(["mcp", "add", name, "--global", "--"])
        .args(command)
        .current_dir(paths::home())
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr).to_string()
            + &String::from_utf8_lossy(&out.stdout);
        return Err(msg.trim().to_string());
    }
    Ok(())
}

/// Run the `api` operations `ops` for MCP server `name` in every loaded location, in parallel.
/// None if the service isn't running or the locations can't be listed; else how many succeeded.
fn each_location(name: &str, ops: &[&str]) -> Option<usize> {
    let svc = service();
    if !svc.as_ref().is_some_and(|s| s.exe.is_some()) {
        return None;
    }
    let (bin, _) = find(svc.as_ref())?;
    let out = process::command(&bin)
        .args(["api", "debug.location.list"])
        .output()
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    let locations: Vec<String> = v
        .as_array()?
        .iter()
        .filter_map(|l| l["directory"].as_str().map(String::from))
        .collect();
    let server = format!("server={name}");
    let done = std::thread::scope(|s| {
        let handles: Vec<_> = locations
            .iter()
            .map(|loc| {
                let (bin, server) = (&bin, &server);
                s.spawn(move || {
                    let loc = format!("location[directory]={loc}");
                    ops.iter().all(|op| {
                        process::command(bin)
                            .args(["api", op, "--param", server, "--param", &loc])
                            .output()
                            .is_ok_and(|o| o.status.success())
                    })
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().unwrap_or(false))
            .filter(|ok| *ok)
            .count()
    });
    Some(done)
}

/// Restart MCP server `name` in every loaded opencode location so a replaced binary is used
/// (`opencode reload` if the locations can't be listed). Ok(n locations).
pub(crate) fn reconnect_mcp(name: &str) -> Result<usize, String> {
    if service().is_none_or(|s| s.exe.is_none()) {
        return Ok(0);
    }
    match each_location(
        name,
        &["experimental.mcp.disconnect", "experimental.mcp.connect"],
    ) {
        Some(n) => Ok(n),
        None => reload().map(usize::from),
    }
}

/// Undo `mcp_add` (mesh leave): cut the entry out of the global config text (only if its command
/// is `ours`; everything else stays byte for byte), then drop the running servers.
pub(crate) fn mcp_unregister(name: &str, ours: &[String]) -> Result<bool, String> {
    let Some((path, cmd)) = mcp_doc(name)? else {
        return Ok(false);
    };
    if cmd != ours {
        return Err(format!(
            "the \"{name}\" entry ({}) isn't mesh's; left alone",
            cmd.join(" ")
        ));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let new = jsonc_remove(&text, &["mcp", "servers", name])
        .or_else(|| jsonc_remove(&text, &["mcp", name]))
        .ok_or_else(|| format!("couldn't find \"{name}\" in {}", path.display()))?;
    std::fs::write(&path, new).map_err(|e| format!("{}: {e}", path.display()))?;
    let _ = each_location(name, &["experimental.mcp.remove"]);
    Ok(true)
}

/// Cut object member `path` out of JSONC `text` (with its line and comma); None if absent.
fn jsonc_remove(text: &str, path: &[&str]) -> Option<String> {
    let b = text.as_bytes();
    let mut i = 0;
    let (start, end) = member(b, &mut i, path)?;
    let mut s = start;
    while s > 0 && matches!(b[s - 1], b' ' | b'\t') {
        s -= 1;
    }
    let mut e = end;
    while e < b.len() && matches!(b[e], b' ' | b'\t') {
        e += 1;
    }
    if e < b.len() && b[e] == b',' {
        e += 1;
        while e < b.len() && matches!(b[e], b' ' | b'\t' | b'\r') {
            e += 1;
        }
        if e < b.len() && b[e] == b'\n' {
            e += 1;
        }
        return Some(format!("{}{}", &text[..s], &text[e..]));
    }
    // last member: drop the comma before it instead (strict JSON has no trailing commas)
    let mut p = s;
    while p > 0 && b[p - 1].is_ascii_whitespace() {
        p -= 1;
    }
    let cut = if p > 0 && b[p - 1] == b',' { p - 1 } else { s };
    Some(format!("{}{}", &text[..cut], &text[end..]))
}

fn skip_ws(b: &[u8], i: &mut usize) {
    loop {
        while *i < b.len() && b[*i].is_ascii_whitespace() {
            *i += 1;
        }
        if b[*i..].starts_with(b"//") {
            while *i < b.len() && b[*i] != b'\n' {
                *i += 1;
            }
        } else if b[*i..].starts_with(b"/*") {
            *i += 2;
            while *i < b.len() && !b[*i..].starts_with(b"*/") {
                *i += 1;
            }
            *i = (*i + 2).min(b.len());
        } else {
            return;
        }
    }
}

fn string<'a>(b: &'a [u8], i: &mut usize) -> Option<&'a [u8]> {
    if b.get(*i) != Some(&b'"') {
        return None;
    }
    *i += 1;
    let start = *i;
    while *i < b.len() && b[*i] != b'"' {
        *i += if b[*i] == b'\\' { 2 } else { 1 };
    }
    let s = b.get(start..*i)?;
    *i += 1;
    Some(s)
}

fn skip_value(b: &[u8], i: &mut usize) -> Option<()> {
    skip_ws(b, i);
    match *b.get(*i)? {
        b'"' => string(b, i).map(drop),
        b'{' | b'[' => {
            let mut depth = 0;
            loop {
                skip_ws(b, i);
                match *b.get(*i)? {
                    b'"' => {
                        string(b, i)?;
                    }
                    b'{' | b'[' => {
                        depth += 1;
                        *i += 1;
                    }
                    b'}' | b']' => {
                        depth -= 1;
                        *i += 1;
                        if depth == 0 {
                            return Some(());
                        }
                    }
                    _ => *i += 1,
                }
            }
        }
        _ => {
            while *i < b.len()
                && !matches!(b[*i], b',' | b'}' | b']')
                && !b[*i].is_ascii_whitespace()
            {
                *i += 1;
            }
            Some(())
        }
    }
}

/// Span (key start, value end) of member `path` inside the object at `i`.
fn member(b: &[u8], i: &mut usize, path: &[&str]) -> Option<(usize, usize)> {
    skip_ws(b, i);
    if b.get(*i) != Some(&b'{') {
        return None;
    }
    *i += 1;
    loop {
        skip_ws(b, i);
        if b.get(*i)? == &b'}' {
            return None;
        }
        let key_start = *i;
        let key = string(b, i)?;
        skip_ws(b, i);
        if b.get(*i)? != &b':' {
            return None;
        }
        *i += 1;
        if key == path[0].as_bytes() {
            if path.len() == 1 {
                skip_value(b, i)?;
                return Some((key_start, *i));
            }
            return member(b, i, &path[1..]);
        }
        skip_value(b, i)?;
        skip_ws(b, i);
        if b.get(*i) == Some(&b',') {
            *i += 1;
        }
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
    let out = process::command(bin)
        .arg("reload")
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(true)
}
