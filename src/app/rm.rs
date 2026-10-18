//! `mesh rm <device>` and `mesh leave` (SPEC §5.8).

use super::{Fail, R, exec, store, sync};
use crate::host::{install, opencode, paths, schedule};
use crate::model::{Step, valid_name};
use crate::registry;
use crate::transport::ssh;

type Say<'a> = &'a dyn Fn(&'static str, &str, String);

/// `devices/<name>.*` present in the registry, as deletions for one commit.
fn device_files(reg: &registry::Registry, name: &str) -> R<Vec<(String, Option<Vec<u8>>)>> {
    let index = sync::fetch(reg)?.ok_or("the registry has no mesh.json")?;
    Ok([".json", ".pub", ".host.pub"]
        .iter()
        .map(|x| format!("devices/{name}{x}"))
        .filter(|p| index.contains_key(p))
        .map(|p| (p, None))
        .collect())
}

/// Remove another device: its files go in one commit; peers drop it on their next sync.
pub(crate) fn rm(name: &str, yes: bool, ask: &dyn Fn(&str) -> bool, step: &dyn Fn(Step)) -> R<i32> {
    let say = |status, n: &str, detail| {
        step(Step {
            status,
            name: n.into(),
            detail,
        })
    };
    let state = store::read_state()?;
    if name == "self" || name == state.device {
        return Err(Fail(2, "rm: that's this device; use `mesh leave`".into()));
    }
    if !valid_name(name) {
        return Err(Fail(2, format!("rm: {name:?} isn't a device name")));
    }
    exec::targets(name, None)?;
    let reg = registry::open(&state.registry).map_err(|e| Fail(3, e))?;
    let files = device_files(&reg, name)?;
    if files.is_empty() {
        return Err(Fail(1, format!("{name} isn't in the mesh")));
    }
    if !yes && !ask(&format!("Remove {name} from the mesh? [y/n]")) {
        return Ok(1);
    }
    reg.commit(&files, &format!("mesh rm {name}"))?;
    say(
        "ok",
        "rm",
        format!(
            "devices/{name}.* removed ({} files, one commit)",
            files.len()
        ),
    );
    let rep = sync::run(false, false)?;
    say("ok", "sync", rep.changes.join(" · "));
    Ok(0)
}

/// Remove this device and undo everything mesh set up here. Keeps ~/.config/opencode/AGENTS.md,
/// an installed SSH server and the mesh binaries.
pub(crate) fn leave(
    yes: bool,
    ask: &dyn Fn(&str) -> bool,
    step: &dyn Fn(Step),
    progress: &dyn Fn(&str, std::time::Duration),
) -> R<i32> {
    let say: Say = &|status, n, detail| {
        step(Step {
            status,
            name: n.into(),
            detail,
        })
    };
    let state = store::read_state()?;
    let me = state.device.clone();
    let q = format!("Remove {me} from the mesh and undo mesh's setup on this device? [y/n]");
    if !yes && !ask(&q) {
        return Ok(1);
    }
    let mut failed = false;
    let mut report = |name: &str, r: Result<bool, String>, done: &str, absent: &str| match r {
        Ok(true) => say("ok", name, done.into()),
        Ok(false) => say("info", name, absent.into()),
        Err(e) => {
            failed = true;
            say("fail", name, e);
        }
    };

    // 1. the repo, explicitly
    let reg = registry::open(&state.registry).map_err(|e| Fail(3, e))?;
    let files = device_files(&reg, &me)?;
    let pushed = if files.is_empty() {
        Ok(false)
    } else {
        reg.commit(&files, &format!("mesh leave: {me}"))
            .map(|_| true)
    };
    report(
        "device",
        pushed,
        &format!("devices/{me}.* removed"),
        "not in the mesh",
    );

    // 2. opencode: MCP registration and the mesh-owned skills folder
    let mesh = install::dir()
        .join(install::names()[0])
        .display()
        .to_string();
    report(
        "mcp",
        opencode::mcp_unregister("mesh", &[mesh, "mcp".into()]),
        "\"mesh\" unregistered",
        "not registered",
    );
    let skills = paths::config_dir().join("skills").join("opencode-mesh");
    let removed = if skills.exists() {
        std::fs::remove_dir_all(&skills)
            .map(|_| true)
            .map_err(|e| format!("{}: {e}", skills.display()))
    } else {
        Ok(false)
    };
    report(
        "skills",
        removed,
        "skills/opencode-mesh removed",
        "no mesh skills here",
    );
    let _ = opencode::reload();

    // 3. scheduled sync, keys block (+ the elevated keys task: one UAC on admin Windows)
    report(
        "schedule",
        schedule::remove_sync(),
        "sync job removed",
        "no sync job",
    );
    #[cfg(windows)]
    if ssh::keys_need_admin() {
        let plan = super::up::Plan::leave();
        let meshw = install::dir().join(install::names().last().copied().unwrap_or("meshw.exe"));
        say(
            "warn",
            "keys",
            "remove the keys block and the keys task: approve the UAC prompt".into(),
        );
        let r = super::up::elevate(&plan, &meshw, "mesh leave", &|t| progress("keys", t));
        report(
            "keys",
            r.map(|_| true).map_err(|Fail(_, e)| e),
            "keys block and keys task removed",
            "",
        );
    } else {
        report(
            "keys",
            ssh::unauthorize(),
            "keys block removed",
            "no keys block",
        );
    }
    #[cfg(unix)]
    {
        let _ = progress;
        report(
            "keys",
            ssh::unauthorize(),
            "keys block removed",
            "no keys block",
        );
    }
    report(
        "ssh",
        ssh::remove_include(),
        "`Include` line removed from ~/.ssh/config",
        "no `Include` line",
    );
    #[cfg(unix)]
    report(
        "PATH",
        install::remove_profile_line(),
        "mesh's PATH line removed from the shell profile",
        "no mesh PATH line in shell profiles",
    );

    // 4. last: this build's mux, then the state dir (key, cache, logs)
    exec::stop_mux();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let dir = paths::state_dir();
    let r = std::fs::remove_dir_all(&dir).map(|_| true).map_err(|e| {
        format!(
            "{} ({e}): something still has a file open, delete it later",
            dir.display()
        )
    });
    report("state", r, &format!("{} removed", dir.display()), "");
    say(
        "info",
        "kept",
        "~/.config/opencode/AGENTS.md, the SSH server, the mesh binaries".into(),
    );
    Ok(if failed { 1 } else { 0 })
}
