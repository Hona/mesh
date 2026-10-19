//! `mesh sync` (SPEC §5.6): pull-only.

use super::store::{self, Index};
use super::{Fail, R, VERSION};
use crate::host::{install, paths, vlog};
use crate::model::{Device, RepoFile, SyncReport, cmp_versions};
use crate::{network, registry, transport};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

type Files = BTreeMap<String, Vec<u8>>;

/// Pull the registry into the cache without applying anything (`mesh up` reads it first).
/// None if there's no mesh there yet.
pub(crate) fn fetch(reg: &registry::Registry) -> Result<Option<Index>, String> {
    let _lock = store::lock_sync()?;
    let Some(commit) = reg.head_opt()? else {
        return Ok(None);
    };
    match pull(reg, &commit, &store::read_index(), false) {
        Ok(i) => Ok(Some(i)),
        Err(e) if e.starts_with("not a mesh") => Ok(None),
        Err(e) => Err(e),
    }
}

pub(crate) fn run(force: bool, keys_only: bool) -> R<SyncReport> {
    let mut r = SyncReport::default();
    if !keys_only && let Some(e) = super::daemon::ensure() {
        r.warnings.push(format!("daemon: {e}"));
    }
    let _lock = store::lock_sync()?;
    if keys_only {
        let devices = store::devices(&store::read_index(), &mut r.warnings);
        keys(&devices, &mut r);
        return Ok(finish(r));
    }
    let mut state = store::read_state()?;
    let reg = registry::open(&state.registry).map_err(|e| Fail(3, e))?;
    let commit = reg.head()?;
    let old = store::read_index();
    // Fast path: registry unchanged since the last sync -> no listing, no downloads,
    // no network lookup. Only the cheap local re-apply below (no spawns unless something drifted).
    let fresh = !force
        && state.synced.as_deref() == Some(commit.as_str())
        && !old.is_empty()
        && old.keys().all(|p| store::cached(p).is_file());
    let pulled = if fresh {
        None
    } else {
        Some(pull(&reg, &commit, &old, force)?)
    };
    let index = pulled.as_ref().unwrap_or(&old);
    let devices = store::devices(index, &mut r.warnings);
    if !fresh {
        repo_changes(&old, index, &mut r);
        client_files(&devices, &mut r)?;
    }
    keys(&devices, &mut r);
    // opencode watches the skills folders and AGENTS.md itself: no reload (SPEC §1.5)
    skills(index, &mut r)?;
    agents(index, &mut r)?;
    state.synced = Some(commit.clone());
    store::write_state(&state)?;
    r.commit = Some(commit);
    // Step 3: converge to the mesh's pinned version (either direction).
    install::clean_old();
    let pin = store::mesh()
        .pin
        .map(|p| p.trim_start_matches('v').to_string());
    if let Some(pin) = pin.filter(|p| cmp_versions(p, VERSION) != std::cmp::Ordering::Equal) {
        match super::update::converge(&pin) {
            Ok(()) => r.applied.push(format!("mesh {VERSION} → {pin} (pinned)")),
            Err(e) => r
                .warnings
                .push(format!("couldn't move to the pinned mesh {pin}: {e}")),
        }
    }
    Ok(finish(r))
}

fn finish(mut r: SyncReport) -> SyncReport {
    r.up_to_date = r.changes.is_empty() && r.applied.is_empty();
    r
}

fn wanted(path: &str) -> bool {
    path == "mesh.json"
        || path == "AGENTS.md"
        || path.starts_with("devices/")
        || path.starts_with("skills/")
}

/// List the registry, download changed files into the cache, return the new index.
fn pull(reg: &registry::Registry, commit: &str, old: &Index, force: bool) -> Result<Index, String> {
    let files: Vec<RepoFile> = reg
        .list(commit)?
        .into_iter()
        .filter(|f| wanted(&f.path))
        .collect();
    if !files.iter().any(|f| f.path == "mesh.json") {
        return Err("not a mesh: the registry has no mesh.json".into());
    }
    let jobs: Vec<(&RepoFile, PathBuf)> = files
        .iter()
        .filter(|f| force || old.get(&f.path) != Some(&f.id) || !store::cached(&f.path).is_file())
        .map(|f| (f, store::cached(&f.path)))
        .collect();
    vlog!("downloading {} of {} files", jobs.len(), files.len());
    reg.fetch(&jobs)?;
    let index: Index = files.into_iter().map(|f| (f.path, f.id)).collect();
    for gone in old.keys().filter(|p| !index.contains_key(*p)) {
        let _ = fs::remove_file(store::cached(gone));
    }
    store::write_index(&index)?;
    Ok(index)
}

fn repo_changes(old: &Index, new: &Index, r: &mut SyncReport) {
    if old.contains_key("mesh.json") && old.get("mesh.json") != new.get("mesh.json") {
        r.changes.push("~ mesh.json".into());
    }
    let names = |i: &Index| -> BTreeSet<String> {
        i.keys()
            .filter_map(|p| p.strip_prefix("devices/")?.strip_suffix(".json"))
            .map(String::from)
            .collect()
    };
    let (o, n) = (names(old), names(new));
    for d in n.union(&o) {
        let differs = [".json", ".pub", ".host.pub"].iter().any(|x| {
            let k = format!("devices/{d}{x}");
            old.get(&k) != new.get(&k)
        });
        let op = match (o.contains(d), n.contains(d)) {
            (false, _) => "+",
            (_, false) => "-",
            _ if differs => "~",
            _ => continue,
        };
        r.changes.push(format!("{op} device {d}"));
    }
}

fn client_files(devices: &[Device], r: &mut SyncReport) -> Result<(), String> {
    let hosts: Vec<&str> = devices.iter().map(|d| d.host.as_str()).collect();
    let mut addrs = BTreeMap::new();
    match network::peers(&hosts) {
        Ok(peers) => {
            for d in devices {
                match peers.get(&d.host).and_then(|p| p.address.clone()) {
                    Some(a) => {
                        addrs.insert(d.name.clone(), a);
                    }
                    None => r.warnings.push(format!(
                        "device {}: host {} not found on the network",
                        d.name, d.host
                    )),
                }
            }
        }
        Err(e) => r
            .warnings
            .push(format!("{e}: ssh config falls back to host names")),
    }
    let changed = transport::write_client_files(devices, &addrs)?;
    r.applied.extend(changed.into_iter().map(String::from));
    Ok(())
}

fn keys(devices: &[Device], r: &mut SyncReport) {
    match transport::authorize(devices) {
        Ok(true) => r.applied.push("keys".into()),
        Ok(false) => {}
        // The admin keys file is the elevated keys task's job (SPEC §7.3), not a problem here.
        Err(e) if e.contains("needs admin") => vlog!("authorized keys: {e}"),
        Err(e) => r.warnings.push(format!("authorized keys skipped: {e}")),
    }
}

/// Mirror `skills/<id>/…` into the mesh-owned `skills/opencode-mesh/`, replaced wholesale on any drift.
fn skills(index: &Index, r: &mut SyncReport) -> Result<bool, String> {
    let root = paths::config_dir().join("skills").join("opencode-mesh");
    let mut want: BTreeMap<String, Files> = BTreeMap::new();
    for rel in index.keys() {
        let Some((id, file)) = rel.strip_prefix("skills/").and_then(|p| p.split_once('/')) else {
            continue;
        };
        let bytes = fs::read(store::cached(rel)).map_err(|e| format!("cached {rel}: {e}"))?;
        want.entry(id.to_string())
            .or_default()
            .insert(file.to_string(), bytes);
    }
    let mut have: BTreeMap<String, Files> = BTreeMap::new();
    let mut stray = false;
    for e in fs::read_dir(&root).into_iter().flatten().flatten() {
        if e.path().is_dir() {
            let files = have
                .entry(e.file_name().to_string_lossy().into_owned())
                .or_default();
            walk(&e.path(), "", files);
        } else {
            stray = true;
        }
    }
    let before = r.changes.len();
    for id in want.keys().chain(have.keys()).collect::<BTreeSet<_>>() {
        let op = match (want.get(id), have.get(id)) {
            (Some(_), None) => "+",
            (None, Some(_)) => "-",
            (Some(w), Some(h)) if w != h => "~",
            _ => continue,
        };
        r.changes.push(format!("{op} skill {id}"));
    }
    if r.changes.len() == before && !stray {
        return Ok(false);
    }
    if root.exists() {
        fs::remove_dir_all(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    }
    for (id, files) in &want {
        for rel in files.keys() {
            let mut dest = root.join(id);
            dest.extend(rel.split('/'));
            if let Some(dir) = dest.parent() {
                fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            // copy (not write) keeps the exec bit on unix
            fs::copy(store::cached(&format!("skills/{id}/{rel}")), &dest)
                .map_err(|e| format!("{}: {e}", dest.display()))?;
        }
    }
    Ok(true)
}

fn walk(dir: &Path, prefix: &str, out: &mut Files) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = format!("{prefix}{}", e.file_name().to_string_lossy());
        let p = e.path();
        if p.is_dir() {
            walk(&p, &format!("{name}/"), out);
        } else {
            out.insert(name, fs::read(&p).unwrap_or_default());
        }
    }
}

/// SPEC §7.4: ~/.config/opencode/AGENTS.md follows the mesh; local edits are backed up first.
fn agents(index: &Index, r: &mut SyncReport) -> Result<(), String> {
    if !index.contains_key("AGENTS.md") {
        return Ok(());
    }
    let mesh =
        fs::read(store::cached("AGENTS.md")).map_err(|e| format!("cached AGENTS.md: {e}"))?;
    let target = paths::config_dir().join("AGENTS.md");
    let applied = store::applied_agents();
    let local = fs::read(&target).ok();
    let last = fs::read(&applied).ok();
    if local.as_deref() != Some(mesh.as_slice()) {
        if let Some(edited) = local.as_ref().filter(|l| last.as_ref() != Some(*l)) {
            let backup = store::new_backup_dir().join("AGENTS.md");
            store::write_file(&backup, edited)?;
            r.warnings.push(match last {
                // first sync on this device: there is no "last sync" to have edited since
                None => format!("backed up the previous AGENTS.md to {}", backup.display()),
                Some(_) => format!(
                    "local AGENTS.md was edited since the last sync; backed up to {}",
                    backup.display()
                ),
            });
        }
        store::write_file(&target, &mesh)?;
        r.changes.push(format!(
            "{} AGENTS.md",
            if local.is_some() { "~" } else { "+" }
        ));
    }
    if last.as_deref() != Some(mesh.as_slice()) {
        store::write_file(&applied, &mesh)?;
    }
    Ok(())
}
