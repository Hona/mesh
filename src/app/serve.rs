//! The remote side of exec: `mesh exec self --envelope [<hex>]` (one-shot, SPEC §6.2) and
//! `mesh exec self --serve` (persistent link, SPEC §6.5).

use super::{VERSION, store};
use crate::host::process::{self, Killer};
use crate::host::{opencode, paths};
use crate::model::{Envelope, ExitInfo};
use crate::transport::frame::{self, CANCEL, EXIT, HELLO, PING, START, STDERR, STDIN, STDOUT};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::Duration;

/// Exit code reported when `timeout` killed the command (as coreutils `timeout`).
pub(crate) const TIMED_OUT: i32 = 124;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub(crate) fn unhex(s: &str) -> Option<Vec<u8>> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok())
        .collect()
}

fn parse(bytes: &[u8]) -> Result<Envelope, String> {
    let env: Envelope = serde_json::from_slice(bytes).map_err(|e| format!("bad envelope: {e}"))?;
    if env.v > 1 {
        return Err(format!(
            "envelope v{} needs a newer mesh than {VERSION} on this device: run `mesh update`",
            env.v
        ));
    }
    Ok(env)
}

fn resolve(prog: &str) -> Result<PathBuf, String> {
    match prog {
        "opencode" => opencode::bin().ok_or_else(|| "opencode not found on this device".into()),
        "mesh" => std::env::current_exe().map_err(|e| e.to_string()),
        // Windows: find .cmd/.bat as well as .exe on PATH, like a shell would
        p if cfg!(windows) && !p.contains(['/', '\\']) && Path::new(p).extension().is_none() => {
            Ok(paths::on_path(p).unwrap_or_else(|| p.into()))
        }
        p => Ok(p.into()),
    }
}

/// `-c`: the string in the shell `mesh up` recorded for this device (SPEC §4.2). No detection
/// or fallback here.
fn shell(s: &str) -> Result<Command, String> {
    let sh = store::read_state()
        .ok()
        .and_then(|st| st.shell)
        .filter(|sh| Path::new(&sh.path).is_file())
        .ok_or("no shell recorded for `exec -c` on this device (or it was uninstalled): run `mesh up` here")?;
    let mut c = process::command(&sh.path);
    match sh.name.as_str() {
        "pwsh" | "powershell" => {
            c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command"])
                .arg(s);
        }
        #[cfg(windows)]
        "cmd" => {
            use std::os::windows::process::CommandExt;
            c.raw_arg(format!("/d /s /c \"{s}\""));
        }
        _ => {
            c.arg("-c").arg(s);
        }
    }
    Ok(c)
}

fn build(env: &Envelope) -> Result<Command, String> {
    let mut c = match (&env.argv, &env.command) {
        (Some(argv), _) => {
            let (prog, args) = argv.split_first().ok_or("empty argv")?;
            let mut c = process::command(resolve(prog)?);
            c.args(args);
            c
        }
        (None, Some(s)) => shell(s)?,
        (None, None) => return Err("envelope has neither argv nor command".into()),
    };
    if let Some(dir) = &env.cwd {
        let dir = match dir.strip_prefix('~') {
            Some(rest) => paths::home().join(rest.trim_start_matches(['/', '\\'])),
            None => PathBuf::from(dir),
        };
        c.current_dir(dir);
    }
    c.envs(&env.env).env("MESH_FROM", &env.from);
    Ok(c)
}

fn what(env: &Envelope) -> String {
    env.argv
        .as_ref()
        .and_then(|a| a.first().cloned())
        .unwrap_or_else(|| "shell".into())
}

fn code(st: io::Result<ExitStatus>) -> i32 {
    let Ok(st) = st else { return 255 };
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = st.signal() {
            return 128 + sig;
        }
    }
    st.code().unwrap_or(1)
}

/// Raw stdin, unbuffered: the envelope line must not swallow the child's input.
fn read_line_raw() -> io::Result<Vec<u8>> {
    #[cfg(windows)]
    let mut f = {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        std::mem::ManuallyDrop::new(unsafe {
            std::fs::File::from_raw_handle(io::stdin().as_raw_handle())
        })
    };
    #[cfg(unix)]
    let mut f = {
        use std::os::unix::io::FromRawFd;
        std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(0) })
    };
    let mut line = Vec::new();
    let mut b = [0u8; 1];
    while f.read(&mut b)? == 1 && b[0] != b'\n' {
        line.push(b[0]);
    }
    Ok(line)
}

/// One-shot: run the envelope with this process's stdio (SSH's channels) and exit with its code.
pub(crate) fn envelope(arg: Option<&str>) -> i32 {
    let env = match arg {
        Some(h) => unhex(h)
            .ok_or_else(|| "bad envelope".to_string())
            .and_then(|b| parse(&b)),
        None => read_line_raw()
            .map_err(|e| e.to_string())
            .and_then(|b| parse(&b)),
    };
    let (mut child, killer) = match env.as_ref().map_err(String::clone).and_then(|e| {
        let mut c = build(e)?;
        process::spawn_tree(&mut c).map_err(|err| format!("{}: {err}", what(e)))
    }) {
        Ok(x) => x,
        Err(e) => {
            eprintln!("mesh: {e}");
            return 127;
        }
    };
    let timed_out = Arc::new(AtomicBool::new(false));
    if let Some(t) = env.ok().and_then(|e| e.timeout) {
        let k = Arc::new(killer);
        let (k2, flag) = (k.clone(), timed_out.clone());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs_f64(t));
            flag.store(true, Ordering::Relaxed);
            k2.kill();
        });
        let c = code(child.wait());
        return if timed_out.load(Ordering::Relaxed) {
            TIMED_OUT
        } else {
            k.release();
            c
        };
    }
    // A normal exit leaves what the command started running; only timeout, cancel and a lost
    // client kill the tree (this process dying closes the still kill-on-close job).
    let c = code(child.wait());
    killer.release();
    c
}

type Out = Arc<Mutex<io::Stdout>>;

fn send(out: &Out, kind: u8, id: u32, data: &[u8]) {
    let _ = frame::write(&mut *lock(out), kind, id, data);
}

fn exit_frame(code: i32, error: Option<String>) -> Vec<u8> {
    serde_json::to_vec(&ExitInfo {
        code: Some(code),
        error,
    })
    .unwrap_or_default()
}

struct Running {
    stdin: Option<mpsc::Sender<Vec<u8>>>,
    killer: Arc<Killer>,
}

type Table = Arc<Mutex<HashMap<u32, Running>>>;

/// Persistent link: many concurrent commands, framed on stdin/stdout.
pub(crate) fn serve() -> i32 {
    process::set_background();
    let out: Out = Arc::new(Mutex::new(io::stdout()));
    send(
        &out,
        HELLO,
        0,
        format!(r#"{{"v":1,"mesh":"{VERSION}"}}"#).as_bytes(),
    );
    let table: Table = Arc::new(Mutex::new(HashMap::new()));
    let mut input = io::BufReader::with_capacity(frame::CHUNK, io::stdin().lock());
    while let Ok(Some(f)) = frame::read(&mut input) {
        match f.kind {
            START => {
                if let Err((code, e)) = start(f.id, &f.data, &out, &table) {
                    send(&out, EXIT, f.id, &exit_frame(code, Some(e)));
                }
            }
            STDIN => {
                if let Some(r) = lock(&table).get_mut(&f.id) {
                    if f.data.is_empty() {
                        r.stdin = None; // EOF
                    } else if let Some(tx) = &r.stdin {
                        let _ = tx.send(f.data);
                    }
                }
            }
            CANCEL => {
                if let Some(r) = lock(&table).get(&f.id) {
                    r.killer.kill();
                }
            }
            PING => send(&out, PING, f.id, &[]),
            _ => {}
        }
    }
    // The link is gone: nothing keeps running without it.
    for r in lock(&table).values() {
        r.killer.kill();
    }
    0
}

fn start(id: u32, data: &[u8], out: &Out, table: &Table) -> Result<(), (i32, String)> {
    let env = parse(data).map_err(|e| (255, e))?;
    let mut cmd = build(&env).map_err(|e| (127, e))?;
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (mut child, killer) =
        process::spawn_tree(&mut cmd).map_err(|e| (127, format!("{}: {e}", what(&env))))?;
    let killer = Arc::new(killer);
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    if let Some(mut w) = child.stdin.take() {
        std::thread::spawn(move || {
            for chunk in rx {
                if w.write_all(&chunk).is_err() {
                    break;
                }
            }
        });
    }
    let mut pumps = Vec::new();
    let streams: [(Option<Box<dyn Read + Send>>, u8); 2] = [
        (
            child
                .stdout
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            STDOUT,
        ),
        (
            child
                .stderr
                .take()
                .map(|s| Box::new(s) as Box<dyn Read + Send>),
            STDERR,
        ),
    ];
    for (stream, kind) in streams {
        let (Some(mut r), out) = (stream, out.clone()) else {
            continue;
        };
        pumps.push(std::thread::spawn(move || {
            let mut buf = vec![0u8; frame::CHUNK];
            while let Ok(n @ 1..) = r.read(&mut buf) {
                send(&out, kind, id, &buf[..n]);
            }
        }));
    }
    lock(table).insert(
        id,
        Running {
            stdin: Some(tx),
            killer: killer.clone(),
        },
    );
    let timed_out = Arc::new(AtomicBool::new(false));
    let (done_tx, done_rx) = mpsc::channel::<()>();
    if let Some(t) = env.timeout {
        let (k, flag) = (killer.clone(), timed_out.clone());
        std::thread::spawn(move || {
            if done_rx.recv_timeout(Duration::from_secs_f64(t))
                == Err(mpsc::RecvTimeoutError::Timeout)
            {
                flag.store(true, Ordering::Relaxed);
                k.kill();
            }
        });
    }
    let (out, table, timeout) = (out.clone(), table.clone(), env.timeout);
    std::thread::spawn(move || {
        let st = child.wait();
        drop(done_tx);
        if !timed_out.load(Ordering::Relaxed) {
            killer.release();
        }
        for p in pumps {
            let _ = p.join();
        }
        let exit = if timed_out.load(Ordering::Relaxed) {
            exit_frame(
                TIMED_OUT,
                Some(format!("timeout after {}s", timeout.unwrap_or_default())),
            )
        } else {
            exit_frame(code(st), None)
        };
        lock(&table).remove(&id);
        send(&out, EXIT, id, &exit);
    });
    Ok(())
}
