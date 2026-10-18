//! `mesh exec` (SPEC §5.5): through the local mux (§6.5) when possible, else the one-shot
//! SSH handoff (§6.2). Callers get a stream of events and render them.

use super::{Fail, R, VERSION, serve, store, sync};
use crate::host::{install, ipc, process};
use crate::model::{Device, Envelope, ExecResult, ExitInfo, Target};
use crate::network;
use crate::transport::frame::{CANCEL, EXIT, START, STDERR, STDIN, STDOUT};
use crate::transport::{mux, ssh};
use std::io::{IsTerminal, Read, Write};
use std::process::{ChildStdin, Stdio};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub(crate) const BUILD: &str = env!("MESH_BUILD");

pub(crate) enum Input {
    /// This process's stdin when it's piped (a terminal sends nothing).
    Inherit,
    Bytes(Vec<u8>),
    Empty,
}

pub(crate) enum Event {
    Out(usize, Vec<u8>),
    Err(usize, Vec<u8>),
    /// device index, how it ended, elapsed ms
    Exit(usize, ExitInfo, u64),
}

/// Devices for a target (SPEC §5.1); a name missing from the cache triggers one sync first.
pub(crate) fn targets(target: &str, exclude: Option<&str>) -> R<(String, Vec<Device>)> {
    let t = Target::parse(target, exclude).map_err(|e| Fail(2, e))?;
    let state = store::read_state()?;
    let mut devices = store::all_devices();
    let names = match t.resolve(&devices, &state.device) {
        Ok(n) => n,
        Err(e) if e.starts_with("unknown device") => {
            let _ = sync::run(false, false);
            devices = store::all_devices();
            t.resolve(&devices, &state.device).map_err(|e| Fail(2, e))?
        }
        Err(e) => return Err(Fail(2, e)),
    };
    devices.retain(|d| names.contains(&d.name));
    Ok((state.device, devices))
}

pub(crate) fn hello() -> Vec<u8> {
    format!(r#"{{"v":1,"mesh":"{VERSION}","build":"{BUILD}"}}"#).into_bytes()
}

/// This build's mux: each build has its own endpoint, so builds never fight over one.
fn mux_client() -> Option<mux::Client> {
    let connect = || mux::Client::connect(BUILD).filter(|c| c.hello == hello());
    if let Some(c) = connect() {
        return Some(c);
    }
    quit_old_muxes();
    let exe = install::installed_copy().or_else(|| std::env::current_exe().ok())?;
    let log = store::log_file("mux", &format!("start {}", exe.display()))?;
    process::spawn_detached(&exe, &["mux"], log).ok()?;
    for _ in 0..300 {
        std::thread::sleep(Duration::from_millis(5));
        if let Some(c) = connect() {
            return Some(c);
        }
    }
    None
}

/// Ask this build's mux to exit (mesh leave deletes the state dir it logs into).
pub(crate) fn stop_mux() {
    if let Some(c) = mux::Client::connect(BUILD) {
        c.quit();
    }
}

/// Ask muxes of older builds to exit: new clients can't reach them, so they'd only hold links,
/// remote serve processes and their (renamed-aside) exe. Newer builds' muxes are left alone, so
/// a stale old client can never knock out a newer mux. Ok(how many were asked).
pub(crate) fn quit_old_muxes() -> usize {
    let build = |hello: &[u8]| {
        let v: serde_json::Value = serde_json::from_slice(hello).unwrap_or_default();
        u128::from_str_radix(v["build"].as_str().unwrap_or("0"), 16).unwrap_or(0)
    };
    let mine = u128::from_str_radix(BUILD, 16).unwrap_or(0);
    let mut asked = 0;
    for tag in ipc::endpoints().into_iter().filter(|t| t != BUILD) {
        if let Some(c) = mux::Client::connect(&tag).filter(|c| build(&c.hello) < mine) {
            c.quit();
            asked += 1;
        }
    }
    asked
}

/// Feed `input` to `sink` on its own thread; an empty chunk means EOF.
fn feed(input: Input, sink: impl Fn(&[u8]) + Send + 'static) {
    std::thread::spawn(move || {
        match input {
            Input::Bytes(b) => b.chunks(64 << 10).for_each(&sink),
            Input::Inherit if !std::io::stdin().is_terminal() => {
                let mut stdin = std::io::stdin().lock();
                let mut buf = vec![0u8; 64 << 10];
                while let Ok(n @ 1..) = stdin.read(&mut buf) {
                    sink(&buf[..n]);
                }
            }
            _ => {}
        }
        sink(&[]);
    });
}

fn via_mux(
    c: mux::Client,
    me: &str,
    devices: &[Device],
    env: &Envelope,
    input: Input,
    tx: Sender<Event>,
) -> Arc<mux::Client> {
    let c = Arc::new(c);
    let n = devices.len();
    for (i, d) in devices.iter().enumerate() {
        let e = Envelope {
            v: 1,
            from: me.to_string(),
            device: Some(d.name.clone()),
            ..env.clone()
        };
        if c.send(
            START,
            i as u32 + 1,
            &serde_json::to_vec(&e).unwrap_or_default(),
        )
        .is_err()
        {
            let _ = tx.send(Event::Exit(
                i,
                ExitInfo {
                    code: Some(255),
                    error: Some("mux connection lost".into()),
                },
                0,
            ));
        }
    }
    let sink = c.clone();
    feed(input, move |chunk| {
        for id in 1..=n as u32 {
            let _ = sink.send(STDIN, id, chunk);
        }
    });
    let mut next = c.frames();
    std::thread::spawn(move || {
        let mut left = n;
        while left > 0 {
            let Some(f) = next() else { break };
            let i = (f.id as usize).wrapping_sub(1);
            let ev = match f.kind {
                STDOUT => Event::Out(i, f.data),
                STDERR => Event::Err(i, f.data),
                EXIT => {
                    left -= 1;
                    Event::Exit(i, serde_json::from_slice(&f.data).unwrap_or_default(), 0)
                }
                _ => continue,
            };
            if tx.send(ev).is_err() {
                break;
            }
        }
    });
    c
}

/// Inside an SSH session on Windows (a nested `mesh exec`): ssh.exe there only relays output
/// while its stdin pipe has data, so no mux; one-shot with the envelope in argv, stdin null.
fn nested() -> bool {
    cfg!(windows)
        && (std::env::var_os("SSH_CONNECTION").is_some()
            || std::env::var_os("SSH_CLIENT").is_some())
}

/// Fallback without a mux: one `ssh … exec self --envelope` per device.
fn via_ssh(me: &str, devices: &[Device], env: &Envelope, input: Input, tx: Sender<Event>) {
    let nested = nested();
    let hosts: Vec<&str> = devices.iter().map(|d| d.host.as_str()).collect();
    let peers = network::peers(&hosts).ok();
    let sinks: Arc<Mutex<Vec<Option<ChildStdin>>>> = Arc::new(Mutex::new(Vec::new()));
    for (i, d) in devices.iter().enumerate() {
        let fail = |e: String| {
            Event::Exit(
                i,
                ExitInfo {
                    code: Some(255),
                    error: Some(e),
                },
                0,
            )
        };
        if peers
            .as_ref()
            .is_some_and(|p| !p.get(&d.host).is_some_and(|x| x.online))
        {
            let _ = tx.send(fail("offline".into()));
            continue;
        }
        let e = Envelope {
            v: 1,
            from: me.to_string(),
            ..env.clone()
        };
        let mut line = serde_json::to_vec(&e).unwrap_or_default();
        let hex = nested.then(|| serve::hex(&line));
        let spawned = ssh::oneshot(d, hex.as_deref()).and_then(|mut c| {
            c.stdin(if nested {
                Stdio::null()
            } else {
                Stdio::piped()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
            c.spawn().map_err(|e| format!("ssh: {e}"))
        });
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => {
                let _ = tx.send(fail(e));
                continue;
            }
        };
        line.push(b'\n');
        let mut stdin = child.stdin.take();
        if let Some(s) = stdin.as_mut() {
            let _ = s.write_all(&line);
        }
        sinks.lock().unwrap_or_else(|e| e.into_inner()).push(stdin);
        let mut pumps = Vec::new();
        let streams: [(Option<Box<dyn Read + Send>>, bool); 2] = [
            (
                child
                    .stdout
                    .take()
                    .map(|s| Box::new(s) as Box<dyn Read + Send>),
                true,
            ),
            (
                child
                    .stderr
                    .take()
                    .map(|s| Box::new(s) as Box<dyn Read + Send>),
                false,
            ),
        ];
        for (stream, is_out) in streams {
            let (Some(mut r), tx) = (stream, tx.clone()) else {
                continue;
            };
            pumps.push(std::thread::spawn(move || {
                let mut buf = vec![0u8; 64 << 10];
                while let Ok(n @ 1..) = r.read(&mut buf) {
                    let b = buf[..n].to_vec();
                    let _ = tx.send(if is_out {
                        Event::Out(i, b)
                    } else {
                        Event::Err(i, b)
                    });
                }
            }));
        }
        let tx = tx.clone();
        std::thread::spawn(move || {
            let st = child.wait();
            for p in pumps {
                let _ = p.join();
            }
            let code = st.ok().and_then(|s| s.code()).unwrap_or(255);
            let _ = tx.send(Event::Exit(
                i,
                ExitInfo {
                    code: Some(code),
                    error: None,
                },
                0,
            ));
        });
    }
    feed(input, move |chunk| {
        for s in sinks.lock().unwrap_or_else(|e| e.into_inner()).iter_mut() {
            if chunk.is_empty() {
                s.take();
            } else if let Some(w) = s.as_mut() {
                let _ = w.write_all(chunk);
            }
        }
    });
}

/// Run `env` on `devices` in parallel, calling `on` for every event (on this thread).
/// With a timeout, a local backstop gives up `timeout + 15s` after the start.
pub(crate) fn run(
    me: &str,
    devices: &[Device],
    env: &Envelope,
    input: Input,
    on: &mut dyn FnMut(Event),
) {
    let n = devices.len();
    let start = Instant::now();
    let (tx, rx) = mpsc::channel();
    let client = match (!nested()).then(mux_client).flatten() {
        Some(c) => Some(via_mux(c, me, devices, env, input, tx)),
        None => {
            via_ssh(me, devices, env, input, tx);
            None
        }
    };
    let backstop = env
        .timeout
        .map(|t| Duration::from_secs_f64(t) + Duration::from_secs(15));
    let mut done = vec![false; n];
    let mut left = n;
    while left > 0 {
        let ev = match backstop {
            Some(b) => rx.recv_timeout(b.saturating_sub(start.elapsed())),
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
        };
        let ms = start.elapsed().as_millis() as u64;
        match ev {
            Ok(Event::Exit(i, info, _)) if i < n && !done[i] => {
                done[i] = true;
                left -= 1;
                on(Event::Exit(i, info, ms));
            }
            Ok(Event::Exit(..)) => {}
            Ok(ev) => on(ev),
            Err(why) => {
                let msg = match why {
                    RecvTimeoutError::Timeout => "no answer within the timeout (local backstop)",
                    RecvTimeoutError::Disconnected => "connection lost",
                };
                for (i, d) in done.iter().enumerate().filter(|(_, d)| !**d) {
                    let _ = d;
                    if let Some(c) = &client {
                        let _ = c.send(CANCEL, i as u32 + 1, &[]);
                    }
                    on(Event::Exit(
                        i,
                        ExitInfo {
                            code: Some(255),
                            error: Some(msg.into()),
                        },
                        ms,
                    ));
                }
                break;
            }
        }
    }
}

/// `exec -t`: plain `ssh -t`, the terminal passed straight through.
pub(crate) fn interactive(me: &str, d: &Device, env: &Envelope) -> R<i32> {
    let e = Envelope {
        v: 1,
        from: me.to_string(),
        tty: true,
        ..env.clone()
    };
    let hex = serve::hex(&serde_json::to_vec(&e).unwrap_or_default());
    let status = ssh::interactive(d, &hex)?
        .status()
        .map_err(|e| format!("ssh: {e}"))?;
    Ok(status.code().unwrap_or(255))
}

/// Keeps the first and last `max/2` bytes of a stream.
struct Cap {
    half: usize,
    head: Vec<u8>,
    tail: Vec<u8>,
    total: usize,
}

impl Cap {
    fn new(max: usize) -> Cap {
        Cap {
            half: max / 2,
            head: Vec::new(),
            tail: Vec::new(),
            total: 0,
        }
    }

    fn push(&mut self, mut b: &[u8]) {
        self.total += b.len();
        let room = self.half.saturating_sub(self.head.len()).min(b.len());
        self.head.extend_from_slice(&b[..room]);
        b = &b[room..];
        self.tail.extend_from_slice(b);
        if self.tail.len() > self.half.saturating_mul(2) {
            let cut = self.tail.len() - self.half;
            self.tail.drain(..cut);
        }
    }

    fn finish(mut self) -> (String, bool) {
        let kept = self.head.len() + self.tail.len();
        if self.total <= self.half.saturating_mul(2) && kept == self.total {
            self.head.extend_from_slice(&self.tail);
            return (String::from_utf8_lossy(&self.head).into_owned(), false);
        }
        let cut = self.tail.len().saturating_sub(self.half);
        let tail = &self.tail[cut..];
        let dropped = self.total - self.head.len() - tail.len();
        let text = format!(
            "{}\n…[{dropped} bytes truncated]…\n{}",
            String::from_utf8_lossy(&self.head),
            String::from_utf8_lossy(tail)
        );
        (text, true)
    }
}

/// Run and collect results (CLI `--json`, MCP `exec`, status probes).
pub(crate) fn capture(
    me: &str,
    devices: &[Device],
    env: &Envelope,
    input: Input,
    max: usize,
) -> Vec<ExecResult> {
    let mut caps: Vec<(Cap, Cap)> = devices
        .iter()
        .map(|_| (Cap::new(max), Cap::new(max)))
        .collect();
    let mut results: Vec<ExecResult> = devices
        .iter()
        .map(|d| ExecResult {
            device: d.name.clone(),
            ..Default::default()
        })
        .collect();
    run(me, devices, env, input, &mut |ev| match ev {
        Event::Out(i, b) => caps[i].0.push(&b),
        Event::Err(i, b) => caps[i].1.push(&b),
        Event::Exit(i, info, ms) => {
            let r = &mut results[i];
            r.exit = info.code;
            r.ms = Some(ms);
            r.error = info.error;
        }
    });
    for (r, (out, err)) in results.iter_mut().zip(caps) {
        if r.error.as_deref() == Some("offline") {
            r.exit = None;
            r.ms = None;
            continue;
        }
        let ((so, t1), (se, t2)) = (out.finish(), err.finish());
        r.stdout = Some(so);
        r.stderr = Some(se);
        r.truncated = Some(t1 || t2);
    }
    results
}
