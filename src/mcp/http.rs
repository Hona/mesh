//! Streamable HTTP for the daemon's MCP server (SPEC §8): `POST /mcp` with one JSON-RPC message
//! → `application/json` (no SSE; GET → 405). The local-server rules: bound to 127.0.0.1 only,
//! no foreign `Origin`, `Host` must be this endpoint, JSON only, bearer token compared in
//! constant time.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

/// Headers + body limits.
const MAX_HEAD: usize = 64 << 10;
const MAX_BODY: usize = 16 << 20;

/// Serve `listener` (bound to 127.0.0.1) until it fails, or the daemon is quitting (then the port
/// is free for its successor at once); one thread per connection.
pub(crate) fn serve(listener: TcpListener, token: String) {
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
    let token = Arc::new(token);
    for s in listener.incoming().flatten() {
        if crate::app::serve::DRAINING.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        let token = token.clone();
        std::thread::spawn(move || connection(s, &token, port));
    }
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

fn connection(s: TcpStream, token: &str, port: u16) {
    let _ = s.set_nodelay(true);
    let _ = s.set_read_timeout(Some(Duration::from_secs(300)));
    let Ok(w) = s.try_clone() else { return };
    let mut r = BufReader::new(s);
    let mut w = w;
    while let Some(req) = read(&mut r) {
        let close = req
            .header("connection")
            .is_some_and(|c| c.eq_ignore_ascii_case("close"));
        let (status, body) = respond(&req, token, port);
        let reason = match status {
            200 => "OK",
            202 => "Accepted",
            400 => "Bad Request",
            401 => "Unauthorized",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            _ => "Unsupported Media Type",
        };
        let mut head = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\n",
            body.len()
        );
        if !body.is_empty() {
            head += "Content-Type: application/json\r\n";
        }
        if status == 405 {
            head += "Allow: POST\r\n";
        }
        head += if close {
            "Connection: close\r\n\r\n"
        } else {
            "\r\n"
        };
        if w.write_all(head.as_bytes()).is_err() || w.write_all(&body).is_err() || close {
            return;
        }
    }
}

/// One request; None when the connection ends or sends garbage.
fn read(r: &mut impl BufRead) -> Option<Request> {
    let mut head = 0;
    let mut line = String::new();
    let mut next = |line: &mut String| -> Option<()> {
        line.clear();
        let n = r.by_ref().take(8 << 10).read_line(line).ok()?;
        head += n;
        (n > 0 && head <= MAX_HEAD).then_some(())
    };
    next(&mut line)?;
    let mut parts = line.split_whitespace();
    let (method, path) = (parts.next()?.to_string(), parts.next()?.to_string());
    let mut headers = Vec::new();
    loop {
        next(&mut line)?;
        let l = line.trim_end();
        if l.is_empty() {
            break;
        }
        let (k, v) = l.split_once(':')?;
        headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
    }
    let len = headers
        .iter()
        .find(|(k, _)| k == "content-length")
        .map_or(Some(0), |(_, v)| v.parse::<usize>().ok())?;
    if len > MAX_BODY {
        return None;
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).ok()?;
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}

/// Equal without an early exit (the token's length is fixed and public).
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn error(status: u16, msg: &str) -> (u16, Vec<u8>) {
    (status, json!({ "error": msg }).to_string().into_bytes())
}

fn respond(q: &Request, token: &str, port: u16) -> (u16, Vec<u8>) {
    // DNS rebinding and browsers: only this exact endpoint, never a web page.
    let host_ok = q.header("host").is_some_and(|h| {
        h.eq_ignore_ascii_case(&format!("127.0.0.1:{port}"))
            || h.eq_ignore_ascii_case(&format!("localhost:{port}"))
    });
    if !host_ok {
        return error(403, "bad Host");
    }
    if q.header("origin").is_some_and(|o| o != "null") {
        return error(403, "cross-origin requests are not allowed");
    }
    if q.path.split('?').next() != Some("/mcp") {
        return error(404, "not found (POST /mcp)");
    }
    if q.method != "POST" {
        return error(405, "POST only (no SSE stream)");
    }
    let auth = q.header("authorization").unwrap_or("");
    if !same(auth.as_bytes(), format!("Bearer {token}").as_bytes()) {
        return error(401, "bad or missing bearer token");
    }
    let json_type = q
        .header("content-type")
        .is_some_and(|t| t.to_ascii_lowercase().starts_with("application/json"));
    if !json_type {
        return error(415, "Content-Type must be application/json");
    }
    let Ok(msg) = serde_json::from_slice::<Value>(&q.body) else {
        let e = json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "parse error" } });
        return (400, e.to_string().into_bytes());
    };
    match super::handle(&msg) {
        Some(reply) => (200, reply.to_string().into_bytes()),
        None => (202, Vec::new()),
    }
}
