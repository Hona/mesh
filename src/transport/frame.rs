//! Binary frames for persistent connections (SPEC §6.5):
//! `[kind u8][id u32 LE][len u32 LE][len bytes]`.

use std::io::{self, Read, Write};

pub(crate) const HELLO: u8 = 0;
pub(crate) const START: u8 = 1;
pub(crate) const STDIN: u8 = 2;
pub(crate) const STDOUT: u8 = 3;
pub(crate) const STDERR: u8 = 4;
pub(crate) const EXIT: u8 = 5;
pub(crate) const CANCEL: u8 = 6;
/// Client → daemon (or an older build's mux): quit, once nothing runs (`leave`, a newer build).
pub(crate) const QUIT: u8 = 7;
pub(crate) const PING: u8 = 8;
/// Client → daemon: close the SSH links (after `update`: they run the devices' old mesh).
pub(crate) const RESET: u8 = 9;

/// A shared frame writer: one lock, so frames from several threads never interleave.
pub(crate) type Out = std::sync::Arc<std::sync::Mutex<Box<dyn Write + Send>>>;

pub(crate) fn send(out: &Out, kind: u8, id: u32, data: &[u8]) -> io::Result<()> {
    write(
        &mut *out.lock().unwrap_or_else(|e| e.into_inner()),
        kind,
        id,
        data,
    )
}

/// Largest payload accepted; senders chunk stream data well below this.
const MAX: usize = 16 << 20;
/// Stream chunk size used by senders.
pub(crate) const CHUNK: usize = 64 << 10;

pub(crate) struct Frame {
    pub kind: u8,
    pub id: u32,
    pub data: Vec<u8>,
}

pub(crate) fn write(w: &mut impl Write, kind: u8, id: u32, data: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(9 + data.len());
    buf.push(kind);
    buf.extend_from_slice(&id.to_le_bytes());
    buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
    buf.extend_from_slice(data);
    w.write_all(&buf)?;
    w.flush()
}

/// Next frame; None at a clean end of stream.
pub(crate) fn read(r: &mut impl Read) -> io::Result<Option<Frame>> {
    let mut head = [0u8; 9];
    match r.read(&mut head[..1])? {
        0 => return Ok(None),
        _ => r.read_exact(&mut head[1..])?,
    }
    let id = u32::from_le_bytes([head[1], head[2], head[3], head[4]]);
    let len = u32::from_le_bytes([head[5], head[6], head[7], head[8]]) as usize;
    if len > MAX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut data = vec![0u8; len];
    r.read_exact(&mut data)?;
    Ok(Some(Frame {
        kind: head[0],
        id,
        data,
    }))
}
