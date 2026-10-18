//! Durable commands (SPEC §5.5): run outside the SSH session, so what they start keeps running
//! after it ends. Windows: sshd's session job kills everything in it when the channel closes
//! (breakaway only lands in another job it kills too), so the command is queued for the
//! user-context `\opencode-mesh\run` task, like a remote sync (§5.6), and its output relayed.
//! Linux: a user scope, out of the login session logind may reap. macOS: runs as is.

#[cfg(any(windows, target_os = "linux"))]
use crate::model::Envelope;

/// `opencode service start|restart` is always durable: the service must outlive the command.
#[cfg(any(windows, target_os = "linux"))]
pub(crate) fn wanted(env: &Envelope) -> bool {
    !env.tty
        && (env.durable
            || matches!(env.argv.as_deref(),
                Some([p, s, a, ..]) if p == "opencode" && s == "service" && (a == "start" || a == "restart")))
}

/// Linux: `systemd-run --user --scope` around `c` (same stdio, waits, exit code passed
/// through) when the user manager is reachable; else `c` as is.
#[cfg(target_os = "linux")]
pub(crate) fn scope(c: std::process::Command) -> std::process::Command {
    use crate::host::{paths, process};
    let bus = std::env::var_os("XDG_RUNTIME_DIR")
        .map(|d| std::path::Path::new(&d).join("bus").exists())
        .unwrap_or(false);
    let Some(sr) = paths::find("systemd-run", &paths::unix_bin_dirs("systemd-run")) else {
        return c;
    };
    if !bus {
        return c;
    }
    let mut w = process::command(sr);
    w.args(["--user", "--scope", "--collect", "--quiet", "--"])
        .arg(c.get_program())
        .args(c.get_args());
    for (k, v) in c.get_envs() {
        match v {
            Some(v) => w.env(k, v),
            None => w.env_remove(k),
        };
    }
    if let Some(d) = c.get_current_dir() {
        w.current_dir(d);
    }
    w
}

#[cfg(windows)]
pub(crate) use win::*;

#[cfg(windows)]
mod win {
    use super::super::serve::{self, TIMED_OUT};
    use super::super::store;
    use crate::host::{install, paths, process, schedule};
    use crate::model::{Envelope, ExitInfo};
    use serde::{Deserialize, Serialize};
    use std::fs;
    use std::io::{Read, Seek, SeekFrom};
    use std::path::{Path, PathBuf};
    use std::process::Stdio;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::time::{Duration, Instant};

    const TICK: Duration = Duration::from_millis(50);

    #[derive(Serialize, Deserialize)]
    struct Job {
        env: Envelope,
        /// The serving process; the command is killed if it goes away (the client is gone).
        caller: u32,
    }

    fn dir() -> PathBuf {
        paths::state_dir().join("run")
    }

    /// Inside an SSH session, where nothing outlives the channel.
    pub(crate) fn in_ssh() -> bool {
        std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_CLIENT").is_some()
    }

    pub(crate) fn new_id() -> String {
        static N: AtomicU32 = AtomicU32::new(0);
        format!(
            "{}-{}-{}",
            store::now_ms(),
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        )
    }

    /// Ask the run task to kill job `id` (cancel, or the link is going away).
    pub(crate) fn cancel(id: &str) {
        let _ = fs::write(dir().join(format!("{id}.cancel")), b"");
    }

    /// Serving side: queue `env` as job `id`, start the run task, relay the job's output
    /// (stdout and stderr merged) to `emit` until it ends.
    pub(crate) fn run(id: &str, env: &Envelope, emit: &dyn Fn(&[u8])) -> ExitInfo {
        let d = dir();
        let f = |ext: &str| d.join(format!("{id}.{ext}"));
        let r = queue_and_relay(&d, id, env, emit);
        for ext in ["json", "started", "done", "cancel", "log"] {
            let _ = fs::remove_file(f(ext)); // the log stays while leftovers still write to it
        }
        r.unwrap_or_else(|(code, e)| ExitInfo {
            code: Some(code),
            error: Some(e),
        })
    }

    fn queue_and_relay(
        d: &Path,
        id: &str,
        env: &Envelope,
        emit: &dyn Fn(&[u8]),
    ) -> Result<ExitInfo, (i32, String)> {
        let f = |ext: &str| d.join(format!("{id}.{ext}"));
        let io = |e: std::io::Error| (255, format!("durable: {e}"));
        fs::create_dir_all(d).map_err(io)?;
        let job = serde_json::to_vec(&Job {
            env: env.clone(),
            caller: std::process::id(),
        })
        .map_err(|e| (255, e.to_string()))?;
        fs::write(f("tmp"), job).map_err(io)?;
        fs::rename(f("tmp"), f("json")).map_err(io)?;
        let meshw = install::dir().join("meshw.exe");
        let meshw = if meshw.is_file() {
            meshw
        } else {
            std::env::current_exe().map_err(io)?
        };
        schedule::start_run_task(&meshw).map_err(|e| (255, format!("run task: {e}")))?;
        let t0 = Instant::now();
        loop {
            if let Ok(s) = fs::read_to_string(f("started")) {
                if let Some(e) = s.strip_prefix("error: ") {
                    return Err((127, e.to_string()));
                }
                break;
            }
            // Windows runs the task only while the user is logged on.
            if t0.elapsed() > Duration::from_secs(20) {
                let _ = fs::remove_file(f("json"));
                if !f("started").exists() {
                    return Err((
                        255,
                        "not logged in: durable commands run in the user's own session (nothing ran)"
                            .into(),
                    ));
                }
            }
            std::thread::sleep(TICK);
        }
        let mut off = 0;
        loop {
            let done = fs::read(f("done")).ok();
            off = relay(&f("log"), off, emit);
            if let Some(b) = done {
                relay(&f("log"), off, emit);
                return serde_json::from_slice(&b).map_err(|e| (255, format!("durable: {e}")));
            }
            std::thread::sleep(TICK);
        }
    }

    /// Emit what `log` has past `off`; the new offset.
    fn relay(log: &Path, off: u64, emit: &dyn Fn(&[u8])) -> u64 {
        let Ok(mut file) = fs::File::open(log) else {
            return off;
        };
        let mut buf = Vec::new();
        if file.seek(SeekFrom::Start(off)).is_err() || file.read_to_end(&mut buf).is_err() {
            return off;
        }
        if !buf.is_empty() {
            emit(&buf);
        }
        off + buf.len() as u64
    }

    /// Task side (`meshw exec self --run-queued`, in the user's logon): run every queued job.
    pub(crate) fn run_queued() -> i32 {
        process::set_background();
        let d = dir();
        let mut jobs = Vec::new();
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "json") {
                let claimed = p.with_extension("claimed");
                if fs::rename(&p, &claimed).is_ok() {
                    jobs.push(std::thread::spawn(move || run_one(&claimed)));
                }
            }
        }
        prune(&d);
        for j in jobs {
            let _ = j.join();
        }
        0
    }

    fn run_one(claimed: &Path) {
        let f = |ext: &str| claimed.with_extension(ext);
        let started = |s: String| {
            let _ = fs::write(f("started"), s);
        };
        let job: Job = match fs::read(claimed)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            Some(j) => j,
            None => return started("error: unreadable job".into()),
        };
        let spawned = (|| {
            let log = fs::File::create(f("log")).map_err(|e| e.to_string())?;
            let mut cmd = serve::build(&job.env)?;
            cmd.stdin(Stdio::null())
                .stdout(log.try_clone().map_err(|e| e.to_string())?)
                .stderr(log);
            process::spawn_tree(&mut cmd).map_err(|e| e.to_string())
        })();
        let (mut child, killer) = match spawned {
            Ok(x) => x,
            Err(e) => {
                let _ = fs::remove_file(claimed);
                return started(format!("error: {e}"));
            }
        };
        started(child.id().to_string());
        let t0 = Instant::now();
        let timeout = job.env.timeout.map(Duration::from_secs_f64);
        let mut checked = Instant::now();
        let exit = loop {
            if let Ok(Some(st)) = child.try_wait() {
                killer.release(); // what it started keeps running
                break ExitInfo {
                    code: Some(serve::code(Ok(st))),
                    error: None,
                };
            }
            let stop = if f("cancel").exists() {
                Some((130, "cancelled".to_string()))
            } else if timeout.is_some_and(|t| t0.elapsed() >= t) {
                Some((
                    TIMED_OUT,
                    format!("timeout after {}s", job.env.timeout.unwrap_or_default()),
                ))
            } else if checked.elapsed() >= Duration::from_secs(1) {
                checked = Instant::now();
                process::parent(job.caller)
                    .is_none()
                    .then(|| (255, "the client went away".to_string()))
            } else {
                None
            };
            if let Some((code, e)) = stop {
                killer.kill();
                let _ = child.wait();
                break ExitInfo {
                    code: Some(code),
                    error: Some(e),
                };
            }
            std::thread::sleep(TICK);
        };
        let _ = fs::write(f("done"), serde_json::to_vec(&exit).unwrap_or_default());
        let _ = fs::remove_file(claimed);
    }

    /// Leftovers of jobs nobody collected (a day old).
    fn prune(d: &Path) {
        let day = Duration::from_secs(86_400);
        for e in fs::read_dir(d).into_iter().flatten().flatten() {
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t.elapsed().unwrap_or_default() > day);
            if old {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}
