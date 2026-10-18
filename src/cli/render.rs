//! Text and JSON output.

use crate::model::{Status, SyncReport};
use serde::Serialize;

pub(super) struct Out {
    pub json: bool,
    pub quiet: bool,
}

fn json(v: &impl Serialize) {
    println!("{}", serde_json::to_string_pretty(v).unwrap_or_default());
}

pub(super) fn error(msg: &str, out: &Out) {
    if out.json {
        json(&serde_json::json!({ "error": msg }));
    } else {
        eprintln!("mesh: {msg}");
    }
}

pub(super) fn sync(r: &SyncReport, out: &Out) {
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

pub(super) fn status(s: &Status, out: &Out) {
    if out.json {
        return json(s);
    }
    for w in &s.warnings {
        eprintln!("! {w}");
    }
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
            match d.online {
                Some(true) => "yes",
                Some(false) => "no",
                None => "-",
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
    for r in &rows {
        let line: String = r
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c:<w$}  "))
            .collect();
        println!("{}", line.trim_end());
    }
    if out.quiet {
        return;
    }
    match (&s.synced, s.synced_ago_secs) {
        (Some(c), Some(secs)) => println!("\nsynced {} · {}", &c[..c.len().min(7)], ago(secs)),
        (Some(c), None) => println!("\nsynced {}", &c[..c.len().min(7)]),
        (None, _) => println!("\nnever synced"),
    }
    if let Some(b) = &s.agents_backup {
        println!("! local AGENTS.md edits were backed up to {b}");
    }
}

fn ago(secs: u64) -> String {
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}
