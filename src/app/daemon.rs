//! `mesh daemon` (hidden, SPEC §6.6): the one long-lived mesh process per user and device, in
//! the user's own login session, started at login and kept running by the OS (§7.2). It is:
//! - the exec host: relays (`exec self --serve`, `--envelope`) hand it commands over the local
//!   socket, so they run where the user's own programs do (processes they start outlive the SSH
//!   session; the OS credential store, and so `gh`, works);
//! - the mux (§6.5): local CLI clients reach other devices through its SSH links;
//! - the MCP server (§8), on `http://127.0.0.1:<port>/mcp`;
//! - the periodic sync.

use super::serve::{self, Link};
use super::{Fail, R, VERSION, exec, store};
use crate::host::ipc::{self, Conn};
use crate::host::{install, paths, process, schedule};
use crate::network;
use crate::transport::frame::{HELLO, Out, PING, QUIT, RESET};
use crate::transport::{mux, ssh};
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// This build's endpoint.
fn tag() -> String {
    format!("daemon-{}", exec::BUILD)
}

pub(crate) fn hello() -> Vec<u8> {
    format!(r#"{{"v":1,"mesh":"{VERSION}","build":"{}"}}"#, exec::BUILD).into_bytes()
}

/// The binary the autostart runs (Windows: the windowless meshw): the installed one when it is
/// this build, else the one next to this binary (a build being tried out).
fn exe() -> Option<PathBuf> {
    let names = install::names();
    let dir = match install::installed_copy() {
        Some(_) => install::dir(),
        None => std::env::current_exe().ok()?.parent()?.to_path_buf(),
    };
    Some(dir.join(names[names.len() - 1]))
}

fn interval() -> u64 {
    schedule::parse_interval(store::mesh().interval.as_deref())
}

/// Is this build's daemon answering? A socket connect, no spawn.
pub(crate) fn up() -> bool {
    matches!(ipc::connect(&tag()), Ok(Some(_)))
}

/// A connection to this build's daemon. `start`: if it doesn't answer, start it (registering
/// its autostart first if needed) and wait up to 3 s.
pub(crate) fn connect(start: bool) -> Option<mux::Client> {
    let c = || mux::Client::connect(&tag());
    if let Some(c) = c() {
        return Some(c);
    }
    if !start || ensure().is_some() {
        return None;
    }
    for _ in 0..60 {
        std::thread::sleep(Duration::from_millis(50));
        if let Some(c) = c() {
            return Some(c);
        }
    }
    None
}

/// Make sure this build's daemon runs: autostart registered, started, other builds retired (they
/// finish their running commands first). Nothing to do when it answers. Some(problem).
pub(crate) fn ensure() -> Option<String> {
    if up() {
        return None;
    }
    if let Err(e) = install(false) {
        return Some(e);
    }
    // Windows: a new task instance retires the others itself. Elsewhere one supervised
    // instance: the old one quits once idle and its supervisor starts this build.
    #[cfg(unix)]
    quit_all(Some(&tag()));
    schedule::start_daemon().err()
}

/// Register the autostart.
pub(crate) fn install(check: bool) -> Result<schedule::Outcome, String> {
    let exe = exe().ok_or("can't find the mesh binary")?;
    schedule::ensure_daemon(&exe, interval(), check)
}

/// Ask daemons (all, or all but `keep`) to quit once idle. How many were asked.
pub(crate) fn quit_all(keep: Option<&str>) -> usize {
    let mut n = 0;
    for t in ipc::endpoints() {
        if t.starts_with("daemon-")
            && Some(t.as_str()) != keep
            && let Some(c) = mux::Client::connect(&t)
        {
            c.quit();
            n += 1;
        }
    }
    n
}

/// Close this daemon's SSH links (after `update`, they run the devices' old mesh).
pub(crate) fn reset_links() {
    if let Some(c) = mux::Client::connect(&tag()) {
        let _ = c.send(RESET, 0, &[]);
    }
}

fn token_file() -> PathBuf {
    paths::state_dir().join("mcp-token")
}

/// The MCP endpoint: (port, bearer token, token file). `create`: pick the port (the default
/// unless taken) and make the token when missing.
pub(crate) fn mcp_endpoint(create: bool) -> Result<(u16, String, PathBuf), String> {
    let file = token_file();
    let token = match std::fs::read_to_string(&file) {
        Ok(t) if t.trim().len() >= 32 => t.trim().to_string(),
        _ if create => {
            let t = crate::host::random_hex(32).ok_or("no OS randomness")?;
            crate::host::write_secret(&file, &t)?;
            t
        }
        _ => return Err("no MCP token yet (run `mesh up`)".into()),
    };
    let mut st = store::read_state().map_err(|Fail(_, e)| e)?;
    let port = match st.mcp_port {
        Some(p) => p,
        None if create => {
            let p = (MCP_PORT..MCP_PORT + 100)
                .find(|p| std::net::TcpListener::bind(("127.0.0.1", *p)).is_ok())
                .ok_or("no free port for the MCP server")?;
            st.mcp_port = Some(p);
            store::write_state(&st)?;
            p
        }
        None => return Err("no MCP port yet (run `mesh up`)".into()),
    };
    Ok((port, token, file))
}

const MCP_PORT: u16 = 47474;

struct ConnIo(Arc<Conn>);

impl Read for ConnIo {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self.0).read(buf)
    }
}

impl Write for ConnIo {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self.0).write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// What serves MCP on the daemon's HTTP listener (the `mcp` frontend; app doesn't depend on it).
pub(crate) type McpServer = fn(std::net::TcpListener, String);

pub(crate) fn run(mcp: McpServer) -> R<i32> {
    // Windows: the autostart runs the windowless meshw, which starts the console build with a
    // hidden console and exits. Commands then share that console instead of each getting a new
    // one (conhost costs ~8 ms per spawn). Elsewhere: no console concerns.
    #[cfg(windows)]
    {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        if exe
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("meshw.exe"))
        {
            return relaunch(&exe.with_file_name("mesh.exe"));
        }
    }
    #[cfg(unix)]
    process::set_background();
    let Ok(listener) = ipc::bind(&tag()) else {
        eprintln!("daemon {VERSION} build {} already running", exec::BUILD);
        return Ok(0);
    };
    eprintln!(
        "daemon {VERSION} build {} pid {}",
        exec::BUILD,
        std::process::id()
    );
    let retired = quit_all(Some(&tag()));
    if retired > 0 {
        eprintln!("asked {retired} other daemon(s) to quit");
    }
    match mcp_endpoint(false) {
        Ok((port, token, _)) => match std::net::TcpListener::bind(("127.0.0.1", port)) {
            Ok(l) => {
                eprintln!("mcp on http://127.0.0.1:{port}/mcp");
                std::thread::spawn(move || mcp(l, token));
            }
            Err(e) => eprintln!("mcp: 127.0.0.1:{port}: {e}"),
        },
        Err(e) => eprintln!("mcp: {e}"),
    }
    std::thread::spawn(sync_timer);
    let mux = mux::Mux::new(Box::new(dial));
    while let Ok(conn) = listener.accept() {
        let mux = mux.clone();
        std::thread::spawn(move || link(Arc::new(conn), &mux));
    }
    Ok(0)
}

/// `mesh.exe daemon` with a console of its own that has no window (CREATE_NO_WINDOW), out of
/// the task's job when allowed, logging to `log/daemon.log`.
#[cfg(windows)]
fn relaunch(mesh: &std::path::Path) -> R<i32> {
    use std::os::windows::process::CommandExt;
    if up() {
        return Ok(0);
    }
    let log = store::log_file("daemon", "start").ok_or("can't open the daemon log")?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let mut c = std::process::Command::new(mesh);
    c.arg("daemon")
        .stdin(std::process::Stdio::null())
        .stdout(log)
        .stderr(err);
    crate::host::win::std_handles_private();
    // CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP (| CREATE_BREAKAWAY_FROM_JOB)
    if c.creation_flags(0x0800_0000 | 0x200 | 0x0100_0000)
        .spawn()
        .is_err()
    {
        c.creation_flags(0x0800_0000 | 0x200)
            .spawn()
            .map_err(|e| format!("{}: {e}", mesh.display()))?;
    }
    Ok(0)
}

/// The SSH link to a device for the mux. Offline devices are reported at once (SPEC §5.1).
fn dial(dev: &str) -> Result<std::process::Child, String> {
    let d = store::all_devices()
        .into_iter()
        .find(|d| d.name == dev)
        .ok_or_else(|| format!("unknown device {dev}"))?;
    if let Ok(p) = network::peers(&[d.host.as_str()])
        && !p.get(&d.host).is_some_and(|p| p.online)
    {
        return Err("offline".into());
    }
    ssh::serve(&d)
}

/// One local connection (the CLI, MCP calls from this process go direct, or a relay): commands
/// for other devices go through the mux, the rest run here. Its commands live as long as it.
fn link(conn: Arc<Conn>, mux: &Arc<mux::Mux>) {
    let out: Out = Arc::new(Mutex::new(Box::new(ConnIo(conn.clone()))));
    serve::send(&out, HELLO, 0, &hello());
    let local = Link::new(out.clone(), "daemon");
    mux.client(ConnIo(conn), out.clone(), &mut |f| match f.kind {
        PING => serve::send(&out, PING, f.id, &[]),
        QUIT => {
            eprintln!("asked to quit: exiting once idle");
            serve::DRAINING.store(true, std::sync::atomic::Ordering::Relaxed);
            serve::exit_if_drained();
        }
        RESET => mux.reset(),
        _ => local.handle(f),
    });
    local.stop_all();
}

/// `mesh sync` every `interval` (mesh.json), first a minute after start. A child process: a
/// sync that installs a new binary runs the new one next time.
fn sync_timer() {
    let mut wait = Duration::from_secs(60);
    loop {
        std::thread::sleep(wait);
        wait = Duration::from_secs(interval());
        let Some(mesh) = std::env::current_exe()
            .ok()
            .map(|p| p.with_file_name(install::names()[0]))
        else {
            continue;
        };
        let Some(log) = store::log_file("sync", "sync") else {
            continue;
        };
        let Ok(err) = log.try_clone() else { continue };
        let code = process::command(&mesh)
            .arg("sync")
            .stdin(std::process::Stdio::null())
            .stdout(log)
            .stderr(err)
            .status()
            .map_or(255, |s| s.code().unwrap_or(1));
        let p = paths::state_dir().join("log").join("sync.log");
        if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(p) {
            let _ = writeln!(f, "exit {code}");
        }
    }
}
