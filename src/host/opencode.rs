//! Finding and asking the local opencode install (SPEC §6.4).

use super::{paths, process, vlog};
use crate::model::{Opencode, ServiceState};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

// Only these fields are read from service.json, never `password`.
#[derive(Deserialize)]
struct ServiceFile {
    version: Option<String>,
    pid: Option<u32>,
    url: Option<String>,
}

struct Service {
    version: Option<String>,
    pid: u32,
    /// Executable of the live service process; None if it isn't running.
    exe: Option<PathBuf>,
    url: Option<String>,
}

/// Every opencode spawn has a hard limit: a broken service must never hang mesh.
const QUICK: Duration = Duration::from_secs(3);
const SLOW: Duration = Duration::from_secs(20);

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
        url: f.url,
    })
}

/// Does the service's HTTP server answer `GET /api/info` within 4 s? Any HTTP status counts
/// (it may want auth; the password is never read). A suspended or wedged server accepts the
/// connection but never answers; a busy one can take ~2 s (seen right after MCP restarts).
fn answers(url: &str) -> bool {
    use std::io::{Read, Write};
    use std::net::{TcpStream, ToSocketAddrs};
    let host = url
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or("")
        .replace("0.0.0.0", "127.0.0.1");
    let Some(addr) = host.to_socket_addrs().ok().and_then(|mut a| a.next()) else {
        return false;
    };
    let Ok(mut s) = TcpStream::connect_timeout(&addr, Duration::from_secs(1)) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_secs(4)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(1)));
    let req = format!("GET /api/info HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    let mut head = [0u8; 5];
    s.write_all(req.as_bytes()).is_ok() && s.read_exact(&mut head).is_ok() && &head == b"HTTP/"
}

/// ok / down / stale / hung (SPEC §5.4).
fn state(svc: Option<&Service>) -> ServiceState {
    match svc {
        None => ServiceState::Down,
        Some(Service { exe: None, .. }) => ServiceState::Stale,
        Some(Service { url: Some(u), .. }) if !answers(u) => ServiceState::Hung,
        Some(_) => ServiceState::Ok,
    }
}

/// Where the last live service's executable is remembered (§6.4 step 2).
fn last_service_file() -> PathBuf {
    paths::state_dir().join("opencode-service")
}

/// The running service's executable, then the last one seen, then PATH, then known install
/// locations. Every opencode CLI is a client of the one background service, and another
/// version can replace it with an incompatible one (seen: an old PATH CLI's `service restart`
/// left a service answering 500), so the service's own build comes first.
fn find(svc: Option<&Service>) -> Option<(PathBuf, String)> {
    if let Some(Service {
        exe: Some(exe),
        pid,
        ..
    }) = svc
    {
        let f = last_service_file();
        let text = exe.display().to_string();
        if std::fs::read_to_string(&f).ok().as_deref() != Some(text.as_str()) {
            let _ = std::fs::write(&f, &text);
        }
        return Some((exe.clone(), format!("service pid {pid}")));
    }
    if let Some(p) = std::fs::read_to_string(last_service_file())
        .ok()
        .map(PathBuf::from)
        .filter(|p| p.is_file())
    {
        return Some((p, "last service".into()));
    }
    if let Some(p) = paths::on_path("opencode") {
        return Some((p, "PATH".into()));
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
    let (_, out, _) = process::output_within(process::command(bin).arg("--version"), QUICK)?;
    let s = String::from_utf8_lossy(&out);
    Some(
        s.split_whitespace()
            .last()?
            .trim_start_matches('v')
            .to_string(),
    )
}

/// Only asked of a service that answers (`opencode api` could start one otherwise).
fn busy(bin: &Path) -> Option<u64> {
    let (ok, out, _) =
        process::output_within(process::command(bin).args(["api", "session.active"]), QUICK)?;
    if !ok {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&out).ok()?;
    match &v["data"] {
        serde_json::Value::Object(m) => Some(m.len() as u64),
        serde_json::Value::Array(a) => Some(a.len() as u64),
        _ => None,
    }
}

/// Local opencode facts; None if opencode isn't installed. `version` is the opencode that
/// `mesh exec … opencode` runs (§6.4); the running service's version is added only when it
/// differs. Never starts the service and never hangs on a broken one: a direct HTTP check
/// decides the state, busy is only asked of a service that answers, every spawn is time-boxed.
pub(crate) fn probe() -> Option<Opencode> {
    let svc = service();
    let (bin, how) = find(svc.as_ref())?;
    vlog!("opencode: {} ({how})", bin.display());
    let running = svc
        .as_ref()
        .and_then(|s| s.exe.as_ref().map(|exe| (exe.clone(), s.version.clone())));
    // exec runs the service's own binary: service.json already has its version, no spawn
    let known = running
        .as_ref()
        .filter(|(exe, _)| *exe == bin)
        .and_then(|(_, v)| v.clone());
    let (exec_version, st, busy) = std::thread::scope(|s| {
        let v = known.is_none().then(|| s.spawn(|| version(&bin)));
        let st = state(svc.as_ref());
        let b = (st == ServiceState::Ok).then(|| busy(&bin)).flatten();
        (
            known
                .clone()
                .or_else(|| v.and_then(|h| h.join().ok().flatten())),
            st,
            b,
        )
    });
    let service_version = running
        .and_then(|(_, v)| v)
        .filter(|v| Some(v) != exec_version.as_ref());
    Some(Opencode {
        version: exec_version,
        busy,
        service: st,
        service_version,
    })
}

/// Started by opencode (an agent's shell, a terminal in the app, an MCP server): opencode puts
/// `OPENCODE=1` in the environment of everything it starts.
pub(crate) fn inside() -> bool {
    std::env::var_os("OPENCODE").is_some() || std::env::var_os("OPENCODE_SESSION_ID").is_some()
}

/// The live opencode client (desktop app, TUI) that started process `pid`, if any. Such a client
/// starts the service again itself, from its own environment, whenever it goes away.
fn client_parent(pid: u32) -> Option<u32> {
    let pp = process::parent(pid)?;
    let exe = process::pid_exe(pp)?;
    let name = exe.file_name()?.to_string_lossy().to_lowercase();
    name.contains("opencode").then_some(pp)
}

/// `opencode service restart` (the service's own binary, so the desktop app accepts the new one)
/// with `dir` added to PATH: the new service, and the shells and MCP servers it starts, inherit
/// it. Refused while any session runs, and when a live opencode client owns the service (it would
/// start the replacement with its old PATH). Ok(false): no service is running.
pub(crate) fn restart_service(dir: &Path) -> Result<bool, String> {
    let Some(Service {
        exe: Some(bin),
        pid: old,
        ..
    }) = service()
    else {
        return Ok(false);
    };
    if let Some(c) = client_parent(old) {
        return Err(format!(
            "the service belongs to opencode pid {c}, which starts it with its own PATH"
        ));
    }
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
    let mut child = process::command(&bin)
        .args(["service", "restart"])
        .env("PATH", path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let t0 = std::time::Instant::now();
    let st = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.to_string(),
            Ok(None) if t0.elapsed() < Duration::from_secs(60) => {
                std::thread::sleep(Duration::from_millis(50))
            }
            _ => {
                let _ = child.kill();
                break "didn't finish in 60 s".into();
            }
        }
    };
    match service() {
        // a connected client may have won the race and started it from its own environment
        Some(s) if s.exe.is_some() && s.pid != old => match client_parent(s.pid) {
            None => Ok(true),
            Some(c) => Err(format!(
                "opencode pid {c} started the service again with its own PATH"
            )),
        },
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
    let (_, out, _) = process::output_within(
        process::command(bin)
            .args(["debug", "config"])
            .current_dir(paths::home()),
        SLOW,
    )
    .ok_or("`opencode debug config` didn't finish in 20 s")?;
    let v: serde_json::Value =
        serde_json::from_slice(&out).map_err(|e| format!("opencode debug config: {e}"))?;
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
    let (ok, out, err) = process::output_within(
        process::command(bin)
            .args(["mcp", "add", name, "--global", "--"])
            .args(command)
            .current_dir(paths::home()),
        SLOW,
    )
    .ok_or("`opencode mcp add` didn't finish in 20 s")?;
    if !ok {
        let msg = String::from_utf8_lossy(&err).to_string() + &String::from_utf8_lossy(&out);
        return Err(msg.trim().to_string());
    }
    Ok(())
}

/// Run the `api` operations `ops` for MCP server `name` in every loaded location, in parallel.
/// None if the service isn't running or the locations can't be listed; else how many succeeded.
fn each_location(name: &str, ops: &[&str]) -> Option<usize> {
    let svc = service();
    if state(svc.as_ref()) != ServiceState::Ok {
        return None;
    }
    let (bin, _) = find(svc.as_ref())?;
    let (_, out, _) = process::output_within(
        process::command(&bin).args(["api", "debug.location.list"]),
        SLOW,
    )?;
    let v: serde_json::Value = serde_json::from_slice(&out).ok()?;
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
                        process::output_within(
                            process::command(bin)
                                .args(["api", op, "--param", server, "--param", &loc]),
                            SLOW,
                        )
                        .is_some_and(|(ok, _, _)| ok)
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
    match state(svc.as_ref()) {
        ServiceState::Ok => {}
        ServiceState::Hung => return Err("the opencode service doesn't answer (hung)".into()),
        _ => return Ok(false),
    }
    let (bin, how) = find(svc.as_ref()).ok_or("opencode not found")?;
    vlog!("opencode: {} ({how})", bin.display());
    let (ok, _, err) = process::output_within(process::command(bin).arg("reload"), SLOW)
        .ok_or("`opencode reload` didn't finish in 20 s")?;
    if !ok {
        return Err(String::from_utf8_lossy(&err).trim().to_string());
    }
    Ok(true)
}
