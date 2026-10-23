//! The remote side of exec: `mesh exec self --envelope [<hex>]` (one-shot, SPEC §6.2) and
//! `mesh exec self --serve` (persistent link, SPEC §6.5).

use super::{VERSION, store};
use crate::host::process;
use crate::host::{opencode, paths};
use crate::model::{Envelope, ExitInfo, Shell};
use crate::transport::frame::{self, CANCEL, EXIT, HELLO, PING, START, STDERR, STDIN, STDOUT};
use crate::transport::mux;
use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};

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

/// `path`: the PATH the command gets (the relayed session's), when not this process's.
fn resolve(prog: &str, path: Option<&String>) -> Result<PathBuf, String> {
    match prog {
        "opencode" => opencode::bin().ok_or_else(|| "opencode not found on this device".into()),
        // the console build next to this one (the daemon is meshw on Windows)
        "mesh" => std::env::current_exe()
            .map(|p| p.with_file_name(crate::host::install::names()[0]))
            .map_err(|e| e.to_string()),
        // Windows: find .cmd/.bat as well as .exe on PATH, like a shell would
        p if cfg!(windows) && !p.contains(['/', '\\']) && Path::new(p).extension().is_none() => {
            Ok(paths::on_path_in(p, path.map(Into::into)).unwrap_or_else(|| p.into()))
        }
        p => Ok(p.into()),
    }
}

/// The shell `mesh up` recorded for this device (SPEC §4.2). No detection or fallback here.
fn recorded_shell() -> Result<Shell, String> {
    store::read_state()
        .ok()
        .and_then(|st| st.shell)
        .filter(|sh| Path::new(&sh.path).is_file())
        .ok_or_else(|| {
            "no shell recorded for `exec -c` on this device (or it was uninstalled): run `mesh up` here".into()
        })
}

const PWSH: [&str; 3] = ["-NoLogo", "-NoProfile", "-NonInteractive"];

/// `-c`: the string in the device's shell.
fn shell(s: &str) -> Result<Command, String> {
    let sh = recorded_shell()?;
    let mut c = process::command(&sh.path);
    match sh.name.as_str() {
        "pwsh" | "powershell" => {
            c.args(PWSH).arg("-Command").arg(s);
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

/// A script file mesh wrote; deleted when dropped (after the command ends).
pub(crate) struct ScriptFile(PathBuf);

impl Drop for ScriptFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// `-s`: unix shells read the script from stdin (`sh -s`). PowerShell's `-Command -` reads
/// stdin line by line like a prompt (an `else` on its own line silently ends the script), so
/// PowerShell and cmd run a temp file instead.
fn script(s: &str) -> Result<Built, String> {
    let sh = recorded_shell()?;
    let mut c = process::command(&sh.path);
    let ext = match sh.name.as_str() {
        "pwsh" | "powershell" => "ps1",
        "cmd" => "cmd",
        name => {
            if name != "fish" {
                c.arg("-s"); // fish reads a piped stdin as is, and has no -s
            }
            return Ok(Built {
                cmd: c,
                input: Some(s.as_bytes().to_vec()),
                file: None,
            });
        }
    };
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = paths::state_dir().join("run");
    let path = dir.join(format!(
        "script-{}-{}.{ext}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let body = if ext == "cmd" {
        // cmd echoes every line unless told not to, and wants CRLF
        format!("@echo off\n{s}")
            .replace("\r\n", "\n")
            .replace('\n', "\r\n")
    } else {
        format!("\u{feff}{s}") // BOM: Windows PowerShell reads BOM-less files as ANSI
    };
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(&path, body).map_err(|e| format!("{}: {e}", path.display()))?;
    let file = ScriptFile(path);
    if ext == "cmd" {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.raw_arg(format!("/d /c \"{}\"", file.0.display()));
        }
    } else {
        c.args(PWSH)
            .args(["-ExecutionPolicy", "Bypass", "-File"])
            .arg(&file.0);
    }
    Ok(Built {
        cmd: c,
        input: None,
        file: Some(file),
    })
}

/// What to run for an envelope.
pub(crate) struct Built {
    pub cmd: Command,
    /// The child's stdin instead of the client's (the script for `sh -s`).
    pub input: Option<Vec<u8>>,
    /// Keep until the command ends.
    pub file: Option<ScriptFile>,
}

/// Files a login shell reads (stat only): a change re-captures the login environment.
#[cfg(unix)]
const PROFILES: [&str; 12] = [
    "~/.zshenv",
    "~/.zprofile",
    "~/.zshrc",
    "~/.zlogin",
    "~/.bash_profile",
    "~/.bash_login",
    "~/.bashrc",
    "~/.profile",
    "~/.config/fish/config.fish",
    "/etc/zprofile",
    "/etc/profile",
    "/etc/paths",
];

type EnvMap = std::collections::BTreeMap<String, String>;

/// The user's login-shell environment (SPEC §5.5), captured once per process and again only
/// when a profile file changes. None where it can't be had (it then isn't retried until one
/// does).
#[cfg(unix)]
fn login_env() -> Option<EnvMap> {
    type Sig = Vec<Option<std::time::SystemTime>>;
    static CACHE: Mutex<Option<(Sig, Option<EnvMap>)>> = Mutex::new(None);
    let home = paths::home();
    let sig: Sig = PROFILES
        .iter()
        .map(|p| match p.strip_prefix("~/") {
            Some(rel) => home.join(rel),
            None => PathBuf::from(p),
        })
        .map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .collect();
    let mut c = lock(&CACHE);
    if let Some((s, e)) = &*c
        && *s == sig
    {
        return e.clone();
    }
    let shell = recorded_shell()
        .map(|s| PathBuf::from(s.path))
        .ok()
        .or_else(|| std::env::var_os("SHELL").map(PathBuf::from))?;
    let t0 = Instant::now();
    let e = process::login_env(&shell);
    if IN_DAEMON.load(Ordering::Relaxed) {
        eprintln!(
            "login environment from {}: {} ({} ms)",
            shell.display(),
            if e.is_some() { "ok" } else { "failed" },
            t0.elapsed().as_millis()
        );
    }
    *c = Some((sig, e.clone()));
    e
}

/// What a command's environment starts from. Unix: the user's login shell's (the PATH of a new
/// terminal). Windows: the relaying SSH session's (built fresh from the registry), when relayed.
fn base_env(env: &Envelope) -> Option<EnvMap> {
    #[cfg(unix)]
    if let Some(mut e) = login_env() {
        if env.tty {
            // the terminal's own type and colours, for an interactive command
            for k in ["TERM", "COLORTERM"] {
                if let Ok(v) = std::env::var(k) {
                    e.insert(k.into(), v);
                }
            }
        }
        return Some(e);
    }
    env.relay_env.clone()
}

pub(crate) fn build(env: &Envelope) -> Result<Built, String> {
    let base = base_env(env);
    let mut b = match (&env.argv, &env.command, &env.script) {
        (Some(argv), _, _) => {
            let (prog, args) = argv.split_first().ok_or("empty argv")?;
            let path = base.as_ref().and_then(|e| {
                e.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("PATH"))
                    .map(|(_, v)| v)
            });
            let mut c = process::command(resolve(prog, path)?);
            c.args(args);
            Built {
                cmd: c,
                input: None,
                file: None,
            }
        }
        (None, Some(s), _) => Built {
            cmd: shell(s)?,
            input: None,
            file: None,
        },
        (None, None, Some(s)) => script(s)?,
        (None, None, None) => return Err("envelope has no argv, command or script".into()),
    };
    let c = &mut b.cmd;
    if let Some(base) = &base {
        c.env_clear().envs(base);
    }
    if !env.tty {
        // plain text for programs (and PowerShell 7.2+, whose errors are coloured otherwise)
        c.env("NO_COLOR", "1").env("TERM", "dumb");
    }
    if let Some(dir) = &env.cwd {
        let dir = match dir.strip_prefix('~') {
            Some(rest) => paths::home().join(rest.trim_start_matches(['/', '\\'])),
            None => PathBuf::from(dir),
        };
        c.current_dir(dir);
    }
    c.envs(&env.env).env("MESH_FROM", &env.from);
    Ok(b)
}

/// No output and an idle process tree this long ends a command (SPEC §5.5).
pub(crate) const STALL: Duration = Duration::from_secs(20);
const TICK: Duration = Duration::from_secs(5);
pub(crate) const STALLED: &str = "stalled";

/// Stall detection for one command, sampled every `TICK`: a tick with no new output and the
/// tree under 1% CPU with no I/O is idle; `STALL` of idle ticks is a stall.
pub(crate) struct Watch {
    /// Output bytes so far (pumps add to it).
    out: Arc<AtomicU64>,
    seen: u64,
    usage: Option<(u64, u64)>,
    idle_since: Instant,
    next: Instant,
}

impl Watch {
    pub(crate) fn new(out: Arc<AtomicU64>) -> Watch {
        let now = Instant::now();
        Watch {
            out,
            seen: 0,
            usage: None,
            idle_since: now,
            next: now + TICK,
        }
    }

    /// Call often; samples once per `TICK`. True once the command has stalled.
    pub(crate) fn stalled(&mut self, k: &process::Killer) -> bool {
        let now = Instant::now();
        if now < self.next {
            return false;
        }
        self.next = now + TICK;
        let out = self.out.load(Ordering::Relaxed);
        if out != self.seen {
            self.seen = out;
            self.usage = None;
            self.idle_since = now;
            return false;
        }
        let Some(u) = k.usage() else {
            self.idle_since = now; // unknown here: never a stall
            return false;
        };
        if let Some((cpu, io)) = self.usage.replace(u)
            && (u.0.abs_diff(cpu) > TICK.as_millis() as u64 / 100 || u.1 != io)
        {
            self.idle_since = now;
        }
        now - self.idle_since >= STALL
    }
}

/// How a command ended: still running, killed at the timeout or as stalled, or by itself.
const RAN: u8 = 0;
const TIMEOUT: u8 = 1;
const STALL_KILL: u8 = 2;
const DONE: u8 = 3;

fn what(env: &Envelope) -> String {
    env.argv
        .as_ref()
        .and_then(|a| a.first().cloned())
        .unwrap_or_else(|| "shell".into())
}

pub(crate) fn code(st: io::Result<ExitStatus>) -> i32 {
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
    // A relay (SPEC §6.6): the daemon runs it in the user's session, unless it needs this
    // session's terminal or the daemon can't be reached.
    if let Ok(e) = &env
        && !e.tty
        && let Some(d) = super::daemon::connect(true)
    {
        return oneshot_via(d, e.clone());
    }
    let spawned = env.and_then(|e| {
        let mut b = build(&e)?;
        // the child's stdin: the script, the SSH channel past the envelope line, or nothing
        let feed = b.input.take();
        if feed.is_some() {
            b.cmd.stdin(Stdio::piped());
        } else if !e.stdin {
            b.cmd.stdin(Stdio::null());
        }
        let (mut child, killer) =
            process::spawn_tree(&mut b.cmd).map_err(|err| format!("{}: {err}", what(&e)))?;
        if let (Some(s), Some(mut w)) = (feed, child.stdin.take()) {
            std::thread::spawn(move || w.write_all(&s));
        }
        Ok((e, b, child, killer))
    });
    let (env, _b, mut child, killer) = match spawned {
        Ok(x) => x,
        Err(e) => {
            eprintln!("mesh: {e}");
            return 127;
        }
    };
    let killer = Arc::new(killer);
    let ended = Arc::new(AtomicU8::new(RAN));
    let (done_tx, done_rx) = mpsc::channel::<()>();
    if let Some(t) = env.timeout {
        timer(t, killer.clone(), ended.clone(), done_rx);
    }
    if env.stall {
        // Output goes straight to SSH's channel, so only the tree's I/O shows it here.
        let (k, ended) = (killer.clone(), ended.clone());
        std::thread::spawn(move || {
            let mut w = Watch::new(Arc::default());
            while ended.load(Ordering::Relaxed) == RAN {
                std::thread::sleep(TICK);
                if w.stalled(&k) {
                    stall_kill(&k, &ended);
                }
            }
        });
    }
    let c = code(child.wait());
    drop(done_tx);
    match ended.swap(DONE, Ordering::Relaxed) {
        TIMEOUT => TIMED_OUT,
        STALL_KILL => {
            eprintln!("mesh: {STALLED}: {STALL_HINT}");
            TIMED_OUT
        }
        // A normal exit leaves what the command started running; only timeout, stall, cancel
        // and a lost client kill the tree (this process dying closes the kill-on-close job).
        _ => {
            killer.release();
            c
        }
    }
}

/// One-shot through the daemon: stdin, stdout, stderr and the exit code relayed.
fn oneshot_via(d: mux::Client, mut env: Envelope) -> i32 {
    env.relay_env = Some(relay_env());
    env.device = None;
    let stream_in = env.stdin;
    let d = Arc::new(d);
    if d.send(START, 1, &serde_json::to_vec(&env).unwrap_or_default())
        .is_err()
    {
        eprintln!("mesh: the daemon went away");
        return 255;
    }
    if stream_in {
        let d = d.clone();
        std::thread::spawn(move || {
            let mut stdin = io::stdin().lock();
            let mut buf = vec![0u8; frame::CHUNK];
            while let Ok(n @ 1..) = stdin.read(&mut buf) {
                if d.send(STDIN, 1, &buf[..n]).is_err() {
                    return;
                }
            }
            let _ = d.send(STDIN, 1, &[]);
        });
    }
    let mut next = d.frames();
    while let Some(f) = next() {
        match f.kind {
            STDOUT => {
                let mut o = io::stdout().lock();
                let _ = o.write_all(&f.data);
                let _ = o.flush();
            }
            STDERR => {
                let _ = io::stderr().write_all(&f.data);
            }
            EXIT => {
                let info: ExitInfo = serde_json::from_slice(&f.data).unwrap_or_default();
                match info.error.as_deref() {
                    Some(STALLED) => eprintln!("mesh: {STALLED}: {STALL_HINT}"),
                    Some(e) => eprintln!("mesh: {e}"),
                    None => {}
                }
                return info.code.unwrap_or(255);
            }
            _ => {}
        }
    }
    eprintln!("mesh: the daemon went away");
    255
}

pub(crate) const STALL_HINT: &str = "no output and idle for 20 s; is it waiting for input?";

/// How long output may still arrive after the command's process exits.
const DRAIN: Duration = Duration::from_millis(300);
const HELD: &str = "mesh: a background process still holds stdout/stderr; its later output isn't shown (redirect it to a file)\n";

fn stall_kill(k: &process::Killer, ended: &AtomicU8) {
    if ended
        .compare_exchange(RAN, STALL_KILL, Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        k.kill();
    }
}

/// Kill the tree `t` seconds from now unless `done` drops first.
fn timer(t: f64, k: Arc<process::Killer>, ended: Arc<AtomicU8>, done: mpsc::Receiver<()>) {
    std::thread::spawn(move || {
        if done.recv_timeout(Duration::from_secs_f64(t)) == Err(mpsc::RecvTimeoutError::Timeout)
            && ended
                .compare_exchange(RAN, TIMEOUT, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
        {
            k.kill();
        }
    });
}

use crate::transport::frame::Out;

pub(crate) fn send(out: &Out, kind: u8, id: u32, data: &[u8]) {
    let _ = frame::write(&mut *lock(out), kind, id, data);
}

pub(crate) fn exit_frame(code: i32, error: Option<String>, via: &str) -> Vec<u8> {
    serde_json::to_vec(&ExitInfo {
        code: Some(code),
        error,
        via: Some(via.into()),
    })
    .unwrap_or_default()
}

struct Running {
    stdin: Option<mpsc::Sender<Vec<u8>>>,
    /// Cancel / lost link: kill the tree.
    stop: Arc<dyn Fn() + Send + Sync>,
    /// Stall detection, when asked for.
    watch: Option<(Watch, Arc<process::Killer>, Arc<AtomicU8>)>,
}

type Table = Arc<Mutex<HashMap<u32, Running>>>;

/// Commands running in this process, on every link (the daemon drains to zero to quit).
static RUNNING: AtomicU32 = AtomicU32::new(0);
/// Daemon: asked to quit; exit once nothing runs.
pub(crate) static DRAINING: AtomicBool = AtomicBool::new(false);
/// This process is the daemon (its stderr is its log).
pub(crate) static IN_DAEMON: AtomicBool = AtomicBool::new(false);

pub(crate) fn exit_if_drained() {
    if DRAINING.load(Ordering::Relaxed) && RUNNING.load(Ordering::Relaxed) == 0 {
        eprintln!("drained, exiting");
        std::process::exit(0);
    }
}

/// The commands of one link (an SSH channel, or one relay's connection to the daemon), run by
/// this process. `via` goes into their exit frames.
pub(crate) struct Link {
    out: Out,
    table: Table,
    via: &'static str,
}

impl Link {
    pub(crate) fn new(out: Out, via: &'static str) -> Link {
        let table: Table = Arc::new(Mutex::new(HashMap::new()));
        watch_stalls(Arc::downgrade(&table));
        Link { out, table, via }
    }

    /// START, STDIN, CANCEL for this link's commands.
    pub(crate) fn handle(&self, f: frame::Frame) {
        match f.kind {
            START => {
                if let Err((code, e)) = start(f.id, &f.data, &self.out, &self.table, self.via) {
                    send(&self.out, EXIT, f.id, &exit_frame(code, Some(e), self.via));
                }
            }
            STDIN => {
                if let Some(r) = lock(&self.table).get_mut(&f.id) {
                    if f.data.is_empty() {
                        r.stdin = None; // EOF
                    } else if let Some(tx) = &r.stdin {
                        let _ = tx.send(f.data);
                    }
                }
            }
            CANCEL => {
                if let Some(r) = lock(&self.table).get(&f.id) {
                    (r.stop)();
                }
            }
            _ => {}
        }
    }

    /// The link is gone: nothing it started keeps running without it.
    pub(crate) fn stop_all(&self) {
        for r in lock(&self.table).values() {
            (r.stop)();
        }
    }
}

/// One timer per link: stall checks while no stdin is still streaming in (a command waiting
/// for its caller's input isn't stalled). Ends with the link.
fn watch_stalls(table: std::sync::Weak<Mutex<HashMap<u32, Running>>>) {
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(TICK);
            let Some(table) = table.upgrade() else { return };
            for r in lock(&table).values_mut() {
                if let (None, Some((w, k, ended))) = (&r.stdin, &mut r.watch)
                    && w.stalled(k)
                {
                    stall_kill(k, ended);
                }
            }
        }
    });
}

/// The SSH session's environment, minus what only describes that session: the daemon starts
/// relayed commands with it, as they'd have had in the session (fresh PATH and all).
pub(crate) fn relay_env() -> std::collections::BTreeMap<String, String> {
    std::env::vars()
        .filter(|(k, _)| {
            let k = k.to_ascii_uppercase();
            !k.starts_with("SSH_") && !k.starts_with("XDG_SESSION_")
        })
        .collect()
}

/// The relay's connection to the daemon: commands started there, by id.
struct Relayed {
    daemon: mux::Client,
    ids: Mutex<HashSet<u32>>,
    alive: AtomicBool,
}

impl Relayed {
    /// Pump the daemon's frames to `out` until it goes away; then fail what was still running.
    fn start(daemon: mux::Client, out: Out) -> Arc<Relayed> {
        let r = Arc::new(Relayed {
            daemon,
            ids: Mutex::new(HashSet::new()),
            alive: AtomicBool::new(true),
        });
        let (me, mut next) = (r.clone(), r.daemon.frames());
        std::thread::spawn(move || {
            while let Some(f) = next() {
                if f.kind == EXIT {
                    lock(&me.ids).remove(&f.id);
                }
                if matches!(f.kind, STDOUT | STDERR | EXIT) {
                    send(&out, f.kind, f.id, &f.data);
                }
            }
            me.alive.store(false, Ordering::Relaxed);
            for id in lock(&me.ids).drain() {
                let e = Some("the daemon went away".into());
                send(&out, EXIT, id, &exit_frame(255, e, "daemon"));
            }
        });
        r
    }

    /// Hand a START to the daemon with the session's environment; false if it can't take it.
    fn start_command(
        &self,
        id: u32,
        data: &[u8],
        renv: &std::collections::BTreeMap<String, String>,
    ) -> bool {
        let Ok(mut env) = parse(data) else {
            return false;
        };
        if env.tty {
            return false;
        }
        env.relay_env = Some(renv.clone());
        env.device = None; // run there, not routed on through its mux
        let data = serde_json::to_vec(&env).unwrap_or_default();
        lock(&self.ids).insert(id);
        if self.daemon.send(START, id, &data).is_err() {
            lock(&self.ids).remove(&id);
            return false;
        }
        true
    }
}

/// Persistent link (SPEC §6.5): many concurrent commands, framed on stdin/stdout. A relay
/// (§6.6): each command goes to this user's daemon, or runs here in the SSH session when the
/// daemon can't be reached.
pub(crate) fn serve() -> i32 {
    process::set_background();
    let out: Out = Arc::new(Mutex::new(Box::new(io::stdout())));
    send(
        &out,
        HELLO,
        0,
        format!(r#"{{"v":1,"mesh":"{VERSION}"}}"#).as_bytes(),
    );
    let local = Link::new(out.clone(), "ssh");
    let renv = relay_env();
    // when starting the daemon last failed (logged out, disabled): retried after a minute
    let (mut daemon, mut failed) = (None::<Arc<Relayed>>, None::<Instant>);
    let mut input = io::BufReader::with_capacity(frame::CHUNK, io::stdin().lock());
    while let Ok(Some(f)) = frame::read(&mut input) {
        match f.kind {
            START => {
                if daemon
                    .as_ref()
                    .is_none_or(|d| !d.alive.load(Ordering::Relaxed))
                {
                    // down: start it (registering its autostart if needed)
                    let start = failed.is_none_or(|t| t.elapsed() > Duration::from_secs(60));
                    daemon = super::daemon::connect(start).map(|c| Relayed::start(c, out.clone()));
                    if start && daemon.is_none() {
                        failed = Some(Instant::now());
                    }
                }
                let sent = daemon
                    .as_ref()
                    .is_some_and(|d| d.start_command(f.id, &f.data, &renv));
                if !sent {
                    local.handle(f);
                }
            }
            STDIN | CANCEL => match daemon.as_ref().filter(|d| lock(&d.ids).contains(&f.id)) {
                Some(d) => {
                    let _ = d.daemon.send(f.kind, f.id, &f.data);
                }
                None => local.handle(f),
            },
            PING => send(&out, PING, f.id, &[]),
            _ => {}
        }
    }
    // The link is gone: nothing keeps running without it (closing the daemon connection makes
    // the daemon stop that connection's commands).
    local.stop_all();
    0
}

fn start(
    id: u32,
    data: &[u8],
    out: &Out,
    table: &Table,
    via: &'static str,
) -> Result<(), (i32, String)> {
    let env = parse(data).map_err(|e| (255, e))?;
    let Built {
        mut cmd,
        input: feed,
        file,
    } = build(&env).map_err(|e| (127, e))?;
    let spawned = process::pipes(&mut cmd, feed.is_some() || env.stdin).and_then(|p| {
        let (child, killer) = process::spawn_tree(&mut cmd)?;
        Ok((p, child, killer))
    });
    drop(cmd); // our copies of the child's pipe ends
    let ((stdin, stdout, stderr), mut child, killer) =
        spawned.map_err(|e| (127, format!("{}: {e}", what(&env))))?;
    let killer = Arc::new(killer);
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let tx = match (stdin, feed) {
        (Some(mut w), Some(s)) => {
            std::thread::spawn(move || w.write_all(&s));
            None
        }
        (Some(mut w), None) => {
            std::thread::spawn(move || {
                for chunk in rx {
                    if w.write_all(&chunk).is_err() {
                        break;
                    }
                }
            });
            Some(tx)
        }
        _ => None,
    };
    let written = Arc::new(AtomicU64::new(0));
    // Set once the exit frame is out: output after it (a background child still holding the
    // pipe) is read and dropped, so the child never blocks on a full pipe.
    let closed = Arc::new(Mutex::new(false));
    let (eof_tx, eof_rx) = mpsc::channel::<()>();
    for (mut r, kind) in [(stdout, STDOUT), (stderr, STDERR)] {
        let (out, written, closed, eof) =
            (out.clone(), written.clone(), closed.clone(), eof_tx.clone());
        std::thread::spawn(move || {
            let mut buf = vec![0u8; frame::CHUNK];
            while let Ok(n @ 1..) = r.read(&mut buf) {
                let closed = lock(&closed);
                if !*closed {
                    written.fetch_add(n as u64, Ordering::Relaxed);
                    send(&out, kind, id, &buf[..n]);
                }
            }
            let _ = eof.send(());
        });
    }
    drop(eof_tx);
    let ended = Arc::new(AtomicU8::new(RAN));
    let k = killer.clone();
    RUNNING.fetch_add(1, Ordering::Relaxed);
    lock(table).insert(
        id,
        Running {
            stdin: tx,
            stop: Arc::new(move || k.kill()),
            watch: env
                .stall
                .then(|| (Watch::new(written), killer.clone(), ended.clone())),
        },
    );
    let (done_tx, done_rx) = mpsc::channel::<()>();
    if let Some(t) = env.timeout {
        timer(t, killer.clone(), ended.clone(), done_rx);
    }
    let (out, table, timeout) = (out.clone(), table.clone(), env.timeout);
    std::thread::spawn(move || {
        let st = child.wait();
        drop(done_tx);
        let how = ended.swap(DONE, Ordering::Relaxed);
        if how == RAN {
            killer.release();
        }
        // The command ends with its process (SPEC §5.5): drain what it wrote, but don't wait
        // for a background child that inherited stdout/stderr to close them.
        let deadline = Instant::now() + DRAIN;
        let held = (0..2).any(|_| {
            eof_rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                == Err(mpsc::RecvTimeoutError::Timeout)
        });
        let mut closed = lock(&closed);
        if held {
            send(&out, STDERR, id, HELD.as_bytes());
        }
        drop(file);
        lock(&table).remove(&id);
        let exit = match how {
            TIMEOUT => exit_frame(
                TIMED_OUT,
                Some(format!("timeout after {}s", timeout.unwrap_or_default())),
                via,
            ),
            STALL_KILL => exit_frame(TIMED_OUT, Some(STALLED.into()), via),
            _ => exit_frame(code(st), None, via),
        };
        send(&out, EXIT, id, &exit);
        *closed = true;
        drop(closed);
        RUNNING.fetch_sub(1, Ordering::Relaxed);
        exit_if_drained();
    });
    Ok(())
}
