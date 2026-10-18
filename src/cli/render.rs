//! Text and JSON output (shared by the CLI and the MCP server).

use crate::app::exec::{self, Event, Input};
use crate::model::{Device, Envelope, ExecResult, Status, Step, SyncReport};
use serde::Serialize;
use std::io::Write;

#[derive(Default)]
pub(crate) struct Out {
    pub json: bool,
    pub quiet: bool,
}

pub(crate) fn json(v: &impl Serialize) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

pub(crate) fn error(msg: &str, out: &Out) {
    if out.json {
        json(&serde_json::json!({ "error": msg }));
    } else {
        eprintln!("mesh: {msg}");
    }
}

pub(crate) fn sync(r: &SyncReport, out: &Out) {
    if out.json {
        return json(r);
    }
    if !out.quiet {
        for c in &r.changes {
            println!("{c}");
        }
        if !r.applied.is_empty() {
            println!("✓ {}", r.applied.join(" · "));
        }
        if r.up_to_date {
            println!("up to date");
        }
    }
    for w in &r.warnings {
        eprintln!("! {w}");
    }
}

pub(crate) fn status(s: &Status, out: &Out) {
    if out.json {
        return json(s);
    }
    for w in &s.warnings {
        eprintln!("! {w}");
    }
    print!("{}", status_text(s));
}

/// The SPEC §5.4 table plus last sync and backup notes.
pub(crate) fn status_text(s: &Status) -> String {
    let dash = |v: &str| {
        if v.is_empty() {
            "-".to_string()
        } else {
            v.to_string()
        }
    };
    let mut rows = vec![
        [
            "DEVICE", "OS", "SHELL", "ONLINE", "OPENCODE", "BUSY", "MESH",
        ]
        .map(String::from),
    ];
    for d in &s.devices {
        let oc = d.opencode.as_ref();
        let mut mesh = d.mesh.clone().unwrap_or_else(|| "-".into());
        if let Some(pin) = s
            .pin
            .as_ref()
            .filter(|p| d.me && d.mesh.as_ref() != Some(*p))
        {
            mesh = format!("{mesh} → {pin} (pending)");
        }
        rows.push([
            if d.me {
                format!("{} *", d.name)
            } else {
                d.name.clone()
            },
            dash(&d.os),
            dash(&d.shell),
            match (d.online, d.reachable) {
                (Some(true), Some(false)) => "unreachable",
                (Some(true), _) => "yes",
                (Some(false), _) => "no",
                (None, _) => "-",
            }
            .into(),
            oc.map_or("-".into(), |o| {
                format!(
                    "{}{}",
                    o.version.as_deref().unwrap_or("?"),
                    if o.service { "" } else { " (down)" }
                )
            }),
            oc.and_then(|o| o.busy)
                .map_or("-".into(), |b| b.to_string()),
            mesh,
        ]);
    }
    let widths: Vec<usize> = (0..7)
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let mut text = String::new();
    for r in &rows {
        let line: String = r
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c:<w$}  "))
            .collect();
        text += line.trim_end();
        text.push('\n');
    }
    text += &match (&s.synced, s.synced_ago_secs) {
        (Some(c), Some(secs)) => format!("\nsynced {} · {}\n", &c[..c.len().min(7)], ago(secs)),
        (Some(c), None) => format!("\nsynced {}\n", &c[..c.len().min(7)]),
        (None, _) => "\nnever synced\n".into(),
    };
    if let Some(b) = &s.agents_backup {
        text += &format!("! local AGENTS.md edits were backed up to {b}\n");
    }
    text
}

fn ago(secs: u64) -> String {
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

fn secs(ms: u64) -> String {
    format!("{:.1}s", ms as f64 / 1000.0)
}

/// Text rendering for the model (SPEC §8.2).
pub(crate) fn exec_text(results: &[ExecResult]) -> String {
    let mut t = String::new();
    for r in results {
        let mut head = format!("── {}", r.device);
        if let Some(c) = r.exit {
            head += &format!(" · exit {c}");
        }
        if let Some(ms) = r.ms {
            head += &format!(" · {}", secs(ms));
        }
        if let Some(e) = &r.error {
            head += &format!(" · {e}");
        }
        if r.truncated == Some(true) {
            head += " · truncated";
        }
        t += &head;
        t.push('\n');
        for (label, s) in [("", &r.stdout), ("stderr:\n", &r.stderr)] {
            if let Some(s) = s.as_deref().filter(|s| !s.is_empty()) {
                t += label;
                t += s;
                if !s.ends_with('\n') {
                    t.push('\n');
                }
            }
        }
    }
    t
}

/// Live exec output: one device streams raw (exit code passed through); several get
/// `device │ ` line prefixes and a summary line (SPEC §5.5).
pub(crate) fn exec_live(me: &str, devices: &[Device], env: &Envelope) -> i32 {
    if let [d] = devices {
        let mut code = 255;
        let mut so = std::io::stdout().lock();
        exec::run(me, devices, env, Input::Inherit, &mut |ev| match ev {
            Event::Out(_, b) => {
                let _ = so.write_all(&b);
                let _ = so.flush();
            }
            Event::Err(_, b) => {
                let _ = std::io::stderr().write_all(&b);
            }
            Event::Exit(_, info, _) => {
                if let Some(e) = info.error {
                    let _ = so.flush();
                    eprintln!("mesh: {}: {e}", d.name);
                }
                code = info.code.unwrap_or(255);
            }
        });
        return code;
    }
    let w = devices
        .iter()
        .map(|d| d.name.chars().count())
        .max()
        .unwrap_or(0);
    let prefix: Vec<String> = devices
        .iter()
        .map(|d| format!("{:<w$} │ ", d.name))
        .collect();
    let mut bufs: Vec<[Vec<u8>; 2]> = devices.iter().map(|_| [Vec::new(), Vec::new()]).collect();
    let mut summary = vec![String::new(); devices.len()];
    let mut all_ok = true;
    let emit = |pre: &str, line: &[u8], err: bool| {
        let mut v = pre.as_bytes().to_vec();
        v.extend_from_slice(line);
        if err {
            let _ = std::io::stderr().write_all(&v);
        } else {
            let _ = std::io::stdout().write_all(&v);
        }
    };
    exec::run(me, devices, env, Input::Inherit, &mut |ev| match ev {
        Event::Out(i, b) => lines(&mut bufs[i][0], &b, &prefix[i], false, &emit),
        Event::Err(i, b) => lines(&mut bufs[i][1], &b, &prefix[i], true, &emit),
        Event::Exit(i, info, ms) => {
            for (k, buf) in bufs[i].iter_mut().enumerate() {
                if !buf.is_empty() {
                    buf.push(b'\n');
                    emit(&prefix[i], buf, k == 1);
                    buf.clear();
                }
            }
            let name = &devices[i].name;
            summary[i] = match (info.code, info.error) {
                (_, Some(e)) if e == "offline" => format!("⨯ {name} offline"),
                (Some(0), None) => format!("✓ {name} 0 {}", secs(ms)),
                (c, e) => {
                    let code = c.map_or("-".into(), |c| c.to_string());
                    format!(
                        "✗ {name} {code} {}{}",
                        secs(ms),
                        e.map(|e| format!(" ({e})")).unwrap_or_default()
                    )
                }
            };
            all_ok &= summary[i].starts_with('✓');
        }
    });
    eprintln!("{}", summary.join(" · "));
    if all_ok { 0 } else { 1 }
}

fn lines(buf: &mut Vec<u8>, b: &[u8], pre: &str, err: bool, emit: &dyn Fn(&str, &[u8], bool)) {
    buf.extend_from_slice(b);
    while let Some(pos) = buf.iter().position(|&c| c == b'\n') {
        let line: Vec<u8> = buf.drain(..=pos).collect();
        emit(pre, &line, err);
    }
}

static PROGRESS_SHOWN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Heartbeat while an `up` step blocks: rewrites one line on a terminal, else a line every ~30s.
pub(crate) fn progress(name: &str, elapsed: std::time::Duration) {
    use std::io::IsTerminal;
    let s = elapsed.as_secs();
    let line = format!("  … {name:<10} still working · {}m{:02}s", s / 60, s % 60);
    if std::io::stdout().is_terminal() {
        print!("\r{line}");
        let _ = std::io::stdout().flush();
        PROGRESS_SHOWN.store(true, std::sync::atomic::Ordering::Relaxed);
    } else if s % 30 < 5 {
        println!("{line}");
    }
}

/// One `mesh up` step line.
pub(crate) fn step(s: &Step) {
    if PROGRESS_SHOWN.swap(false, std::sync::atomic::Ordering::Relaxed) {
        print!("\r{:60}\r", "");
    }
    if s.status == "head" {
        return println!("{}", s.detail);
    }
    let icon = match s.status {
        "ok" => "✓",
        "warn" => "!",
        "fail" => "✗",
        _ => "…",
    };
    println!("  {icon} {:<10} {}", s.name, s.detail);
}
