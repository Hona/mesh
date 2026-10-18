//! The local mux (SPEC §6.5): one persistent `exec self --serve` link per device, shared by
//! every local mesh process over the local socket. Frames from clients are routed to the
//! device link with the ids rewritten; replies are routed back.

use super::frame::{self, CANCEL, EXIT, HELLO, QUIT, START, STDERR, STDIN, STDOUT};
use crate::host::ipc::{self, Conn};
use crate::model::{Envelope, ExitInfo};
use std::collections::HashMap;
use std::io::{self, BufReader, Read};
use std::process::{Child, ChildStdin};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Opens the SSH link to a device (stdin/stdout/stderr piped), or says why it can't.
pub(crate) type Dial = Box<dyn Fn(&str) -> Result<Child, String> + Send + Sync>;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn exit_frame(code: i32, error: Option<String>) -> Vec<u8> {
    serde_json::to_vec(&ExitInfo {
        code: Some(code),
        error,
    })
    .unwrap_or_default()
}

/// A local process connected to the mux.
struct Peer {
    conn: Arc<Conn>,
    w: Mutex<()>,
}

impl Peer {
    fn send(&self, kind: u8, id: u32, data: &[u8]) -> io::Result<()> {
        let _g = lock(&self.w);
        frame::write(&mut &*self.conn, kind, id, data)
    }
}

/// One device's persistent link.
struct Link {
    input: Mutex<ChildStdin>,
    routes: Mutex<HashMap<u32, (Arc<Peer>, u32)>>,
    next: AtomicU32,
    alive: AtomicBool,
}

impl Link {
    fn send(&self, kind: u8, id: u32, data: &[u8]) -> io::Result<()> {
        frame::write(&mut *lock(&self.input), kind, id, data)
    }
}

struct Mux {
    dial: Dial,
    links: Mutex<HashMap<String, Arc<Link>>>,
    dialing: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    clients: AtomicUsize,
    last: Mutex<Instant>,
    hello: Vec<u8>,
}

impl Mux {
    fn touch(&self) {
        *lock(&self.last) = Instant::now();
    }

    fn live(&self, dev: &str) -> Option<Arc<Link>> {
        lock(&self.links)
            .get(dev)
            .filter(|l| l.alive.load(Ordering::Relaxed))
            .cloned()
    }

    /// The device's link, dialing it if needed (one dial at a time per device).
    fn link(self: &Arc<Self>, dev: &str) -> Result<Arc<Link>, String> {
        if let Some(l) = self.live(dev) {
            return Ok(l);
        }
        let gate = lock(&self.dialing)
            .entry(dev.to_string())
            .or_default()
            .clone();
        let _g = lock(&gate);
        if let Some(l) = self.live(dev) {
            return Ok(l);
        }
        let started = Instant::now();
        let mut child = (self.dial)(dev)?;
        let (Some(input), Some(output), Some(mut errs)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Err("ssh: no pipes".into());
        };
        let tail = Arc::new(Mutex::new(Vec::new()));
        let t = tail.clone();
        let drain = std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = errs.read(&mut buf) {
                let mut v = lock(&t);
                v.extend_from_slice(&buf[..n]);
                let over = v.len().saturating_sub(4096);
                v.drain(..over);
            }
        });
        let mut reader = BufReader::with_capacity(frame::CHUNK, output);
        match frame::read(&mut reader) {
            Ok(Some(f)) if f.kind == HELLO => {}
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = drain.join();
                let msg = String::from_utf8_lossy(&lock(&tail)).trim().to_string();
                let msg = msg
                    .lines()
                    .last()
                    .unwrap_or("no response from remote mesh")
                    .to_string();
                return Err(format!("{dev}: {msg}"));
            }
        }
        eprintln!("link {dev} up in {} ms", started.elapsed().as_millis());
        let link = Arc::new(Link {
            input: Mutex::new(input),
            routes: Mutex::new(HashMap::new()),
            next: AtomicU32::new(1),
            alive: AtomicBool::new(true),
        });
        lock(&self.links).insert(dev.to_string(), link.clone());
        let (mux, l, name) = (self.clone(), link.clone(), dev.to_string());
        std::thread::spawn(move || mux.pump(&name, l, reader, child, tail));
        Ok(link)
    }

    /// Device -> clients, until the link drops.
    fn pump(
        &self,
        dev: &str,
        link: Arc<Link>,
        mut r: impl Read,
        mut child: Child,
        tail: Arc<Mutex<Vec<u8>>>,
    ) {
        while let Ok(Some(f)) = frame::read(&mut r) {
            let route = match f.kind {
                STDOUT | STDERR => lock(&link.routes).get(&f.id).cloned(),
                EXIT => lock(&link.routes).remove(&f.id),
                _ => None,
            };
            if let Some((peer, id)) = route {
                let _ = peer.send(f.kind, id, &f.data);
            }
            self.touch();
        }
        link.alive.store(false, Ordering::Relaxed);
        {
            let mut links = lock(&self.links);
            if links.get(dev).is_some_and(|l| Arc::ptr_eq(l, &link)) {
                links.remove(dev);
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        let why = String::from_utf8_lossy(&lock(&tail))
            .trim()
            .lines()
            .last()
            .unwrap_or("")
            .to_string();
        eprintln!("link {dev} down {why}");
        for (_, (peer, id)) in lock(&link.routes).drain() {
            let _ = peer.send(
                EXIT,
                id,
                &exit_frame(
                    255,
                    Some(format!("connection to {dev} lost {why}").trim().into()),
                ),
            );
        }
    }

    /// Client -> devices, until the client disconnects; then cancel what it left running.
    fn client(self: &Arc<Self>, conn: Arc<Conn>) {
        let peer = Arc::new(Peer {
            conn: conn.clone(),
            w: Mutex::new(()),
        });
        let _ = peer.send(HELLO, 0, &self.hello);
        let mut mine: HashMap<u32, (Arc<Link>, u32)> = HashMap::new();
        let mut r = BufReader::with_capacity(frame::CHUNK, &*conn);
        while let Ok(Some(f)) = frame::read(&mut r) {
            self.touch();
            match f.kind {
                START => {
                    let dev = serde_json::from_slice::<Envelope>(&f.data)
                        .ok()
                        .and_then(|e| e.device);
                    let link = dev
                        .ok_or_else(|| "start without a device".to_string())
                        .and_then(|d| self.link(&d));
                    match link {
                        Ok(link) => {
                            let rid = link.next.fetch_add(1, Ordering::Relaxed);
                            lock(&link.routes).insert(rid, (peer.clone(), f.id));
                            if link.send(START, rid, &f.data).is_err() {
                                lock(&link.routes).remove(&rid);
                                let _ = peer.send(
                                    EXIT,
                                    f.id,
                                    &exit_frame(255, Some("connection lost".into())),
                                );
                            } else {
                                mine.insert(f.id, (link, rid));
                            }
                        }
                        Err(e) => {
                            let _ = peer.send(EXIT, f.id, &exit_frame(255, Some(e)));
                        }
                    }
                }
                STDIN | CANCEL => {
                    if let Some((link, rid)) = mine.get(&f.id) {
                        let _ = link.send(f.kind, *rid, &f.data);
                    }
                }
                QUIT => std::process::exit(0),
                _ => {}
            }
        }
        for (link, rid) in mine.into_values() {
            if lock(&link.routes).remove(&rid).is_some() {
                let _ = link.send(CANCEL, rid, &[]);
            }
        }
        self.clients.fetch_sub(1, Ordering::Relaxed);
        self.touch();
    }
}

/// Run the mux until it has been idle (no clients) for `idle`.
pub(crate) fn serve(listener: ipc::Listener, dial: Dial, idle: Duration, hello: Vec<u8>) {
    let mux = Arc::new(Mux {
        dial,
        links: Mutex::new(HashMap::new()),
        dialing: Mutex::new(HashMap::new()),
        clients: AtomicUsize::new(0),
        last: Mutex::new(Instant::now()),
        hello,
    });
    let m = mux.clone();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(30));
            if m.clients.load(Ordering::Relaxed) == 0 && lock(&m.last).elapsed() >= idle {
                std::process::exit(0);
            }
        }
    });
    while let Ok(conn) = listener.accept() {
        mux.clients.fetch_add(1, Ordering::Relaxed);
        mux.touch();
        let m = mux.clone();
        std::thread::spawn(move || m.client(Arc::new(conn)));
    }
}

/// A local process's connection to the mux.
pub(crate) struct Client {
    conn: Arc<Conn>,
    w: Mutex<()>,
    /// The mux's hello payload.
    pub hello: Vec<u8>,
}

impl Client {
    /// Connect to a running mux; None if there is none.
    pub(crate) fn connect() -> Option<Client> {
        let conn = Arc::new(ipc::connect().ok()??);
        let f = frame::read(&mut &*conn).ok()??;
        (f.kind == HELLO).then(|| Client {
            conn,
            w: Mutex::new(()),
            hello: f.data,
        })
    }

    pub(crate) fn send(&self, kind: u8, id: u32, data: &[u8]) -> io::Result<()> {
        let _g = lock(&self.w);
        frame::write(&mut &*self.conn, kind, id, data)
    }

    /// Ask this mux to exit (it's from another build).
    pub(crate) fn quit(&self) {
        let _ = self.send(QUIT, 0, &[]);
    }

    /// Frames from the mux; read from one thread only.
    pub(crate) fn frames(&self) -> impl FnMut() -> Option<frame::Frame> + Send + use<> {
        let conn = self.conn.clone();
        let mut r = BufReader::with_capacity(frame::CHUNK, ConnReader(conn));
        move || frame::read(&mut r).ok().flatten()
    }
}

struct ConnReader(Arc<Conn>);

impl Read for ConnReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self.0).read(buf)
    }
}
