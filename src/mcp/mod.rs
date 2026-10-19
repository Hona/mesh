//! MCP server (SPEC §8), hand-rolled JSON-RPC 2.0: served by the daemon over Streamable HTTP
//! (`http`), and as `mesh mcp` on stdio (hidden; fallback and debugging).

pub(crate) mod http;

use crate::app::exec::{self, Input};
use crate::app::{self, Fail, VERSION, status};
use crate::cli::render;
use crate::model::{Envelope, ExecReport};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

/// `mesh mcp`: newline-delimited JSON-RPC on stdio; each request on its own thread.
pub(crate) fn run() -> i32 {
    app::background("mcp", "mcp");
    let out = Arc::new(Mutex::new(std::io::stdout()));
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let out = out.clone();
        std::thread::spawn(move || {
            if let Some(msg) = handle(&req) {
                let mut o = out.lock().unwrap_or_else(|e| e.into_inner());
                let _ = writeln!(o, "{msg}");
                let _ = o.flush();
            }
        });
    }
    0
}

/// One JSON-RPC message: the response, or None for a notification (or a response).
pub(crate) fn handle(req: &Value) -> Option<Value> {
    let id = req.get("id").cloned()?;
    req.get("method")?;
    let params = &req["params"];
    let r = match req["method"].as_str().unwrap_or("") {
        "initialize" => Ok(initialize(params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools() })),
        "tools/call" => Ok(call(params)),
        m => Err((-32601, format!("method not found: {m}"))),
    };
    Some(match r {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    })
}

fn initialize(params: &Value) -> Value {
    let devices: Vec<String> = app::devices()
        .iter()
        .map(|d| format!("{} ({})", d.name, d.os))
        .collect();
    // The absolute CLI path: right after a first install, opencode's shells don't have the new
    // PATH yet. The console build: the daemon itself is the windowless one on Windows.
    let cli = std::env::current_exe()
        .map(|p| {
            p.with_file_name(crate::host::install::names()[0])
                .display()
                .to_string()
        })
        .unwrap_or_else(|_| "mesh".into());
    json!({
        "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "mesh", "version": VERSION },
        "instructions": format!(
            "Opencode mesh: devices {}. Load the \"mesh\" skill before using mesh tools. CLI: {cli}",
            devices.join(", ")
        ),
    })
}

fn tools() -> Value {
    json!([
        {
            "name": "status",
            "description": "List the mesh's devices: os, shell, online, opencode version, busy sessions, mesh version.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target": { "type": "string", "default": "all", "description": "name, a,b, all, self, os:linux|macos|windows" },
                    "fast": { "type": "boolean", "default": false, "description": "repo cache + network only, no probing" }
                }
            }
        },
        {
            "name": "exec",
            "description": "Run on mesh devices, in the user's own session there: give argv or script. What it starts keeps running after it returns.",
            "inputSchema": {
                "type": "object",
                "required": ["target"],
                "properties": {
                    "target": { "type": "string", "description": "name, a,b, all, self, os:linux|macos|windows" },
                    "argv": { "type": "array", "items": { "type": "string" }, "description": "one program and its arguments, passed exactly as given (no shell, no quoting); fastest. argv[0] \"opencode\" or \"mesh\" is that device's install" },
                    "script": { "type": "string", "description": "code for the device's own shell (status shows which: pwsh, bash, …): pipes, variables, several lines, any length; no escaping needed" },
                    "stdin": { "type": "string", "description": "fed to the command; without it, stdin is empty" },
                    "stall": { "type": "boolean", "default": true, "description": "end the command when it has no output and an idle process tree for 20 s; false for commands that wait quietly" },
                    "cwd": { "type": "string" },
                    "env": { "type": "object", "additionalProperties": { "type": "string" } },
                    "timeout": { "type": "number", "default": 120, "description": "seconds; kills the whole process tree" },
                    "max_bytes": { "type": "number", "default": 50000, "description": "per stream; keeps head + tail" }
                }
            }
        }
    ])
}

fn call(p: &Value) -> Value {
    let args = &p["arguments"];
    let r = match p["name"].as_str() {
        Some("status") => status_tool(args),
        Some("exec") => exec_tool(args, p["_meta"]["ai.opencode/sessionID"].as_str()),
        other => Err(format!("unknown tool {other:?}")),
    };
    match r {
        Ok((text, structured)) => {
            json!({ "content": [{ "type": "text", "text": text }], "structuredContent": structured, "isError": false })
        }
        Err(e) => json!({ "content": [{ "type": "text", "text": e }], "isError": true }),
    }
}

fn status_tool(a: &Value) -> Result<(String, Value), String> {
    let target = a["target"].as_str().unwrap_or("all");
    let s = status::run(Some(target), None, a["fast"].as_bool().unwrap_or(false))
        .map_err(|Fail(_, e)| e)?;
    Ok((
        render::status_text(&s),
        serde_json::to_value(&s).map_err(|e| e.to_string())?,
    ))
}

/// `session`: the calling opencode session, passed on as `MESH_SESSION` (SPEC §8.2).
fn exec_tool(a: &Value, session: Option<&str>) -> Result<(String, Value), String> {
    let target = a["target"].as_str().ok_or("target is required")?;
    let argv: Option<Vec<String>> = a["argv"].as_array().map(|v| {
        v.iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect()
    });
    let script = a["script"].as_str().map(String::from);
    if argv.is_some() == script.is_some() || argv.as_ref().is_some_and(Vec::is_empty) {
        return Err("give exactly one of argv (non-empty) or script".into());
    }
    if script.is_some() && a["stdin"].is_string() {
        return Err(
            "script can't be combined with stdin: the script is fed to the shell through stdin"
                .into(),
        );
    }
    let mut env = Envelope {
        argv,
        script,
        stall: a["stall"].as_bool().unwrap_or(true),
        cwd: a["cwd"].as_str().map(String::from),
        env: a["env"]
            .as_object()
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default(),
        timeout: Some(a["timeout"].as_f64().unwrap_or(120.0)),
        ..Default::default()
    };
    if let Some(s) = session {
        env.env
            .entry("MESH_SESSION".into())
            .or_insert_with(|| s.into());
    }
    let input = a["stdin"]
        .as_str()
        .map_or(Input::Empty, |s| Input::Bytes(s.as_bytes().to_vec()));
    let max = a["max_bytes"].as_u64().unwrap_or(50_000) as usize;
    let (me, devices) = exec::targets(target, None).map_err(|Fail(_, e)| e)?;
    if devices.is_empty() {
        return Err(format!("no devices match {target}"));
    }
    let results = exec::capture(&me, &devices, &env, input, max);
    let text = render::exec_text(&results);
    Ok((
        text,
        serde_json::to_value(ExecReport { results }).map_err(|e| e.to_string())?,
    ))
}
