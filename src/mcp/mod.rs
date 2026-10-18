//! `mesh mcp` (SPEC §8): stdio MCP server, newline-delimited JSON-RPC 2.0, hand-rolled.
//! Idle it is one thread blocked on stdin; each tool call runs on its own thread.

use crate::app::exec::{self, Input};
use crate::app::{self, Fail, VERSION, status};
use crate::cli::render;
use crate::model::{Envelope, ExecReport};
use serde_json::{Value, json};
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

type Out = Arc<Mutex<std::io::Stdout>>;

pub(crate) fn run() -> i32 {
    app::background("mcp", "mcp");
    let out: Out = Arc::new(Mutex::new(std::io::stdout()));
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let Ok(req) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let Some(id) = req.get("id").cloned() else {
            continue;
        }; // notifications need no answer
        let params = req["params"].clone();
        match req["method"].as_str().unwrap_or("") {
            "initialize" => reply(&out, id, Ok(initialize(&params))),
            "ping" => reply(&out, id, Ok(json!({}))),
            "tools/list" => reply(&out, id, Ok(json!({ "tools": tools() }))),
            "tools/call" => {
                let out = out.clone();
                std::thread::spawn(move || reply(&out, id, Ok(call(&params))));
            }
            m => reply(&out, id, Err((-32601, format!("method not found: {m}")))),
        }
    }
    0
}

fn reply(out: &Out, id: Value, r: Result<Value, (i64, String)>) {
    let msg = match r {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    };
    let mut o = out.lock().unwrap_or_else(|e| e.into_inner());
    let _ = writeln!(o, "{msg}");
    let _ = o.flush();
}

fn initialize(params: &Value) -> Value {
    let devices: Vec<String> = app::devices()
        .iter()
        .map(|d| format!("{} ({})", d.name, d.os))
        .collect();
    // The absolute CLI path: right after a first install, opencode's shells don't have the new
    // PATH yet, but this server was just (re)started by `mesh up`.
    let cli = std::env::current_exe()
        .map(|p| p.display().to_string())
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
            "description": "Run a command on mesh devices. argv runs the exact program with no shell; command runs a string in the device's own shell. argv[0] \"opencode\" or \"mesh\" resolves to that device's install.",
            "inputSchema": {
                "type": "object",
                "required": ["target"],
                "properties": {
                    "target": { "type": "string", "description": "name, a,b, all, self, os:linux|macos|windows" },
                    "argv": { "type": "array", "items": { "type": "string" } },
                    "command": { "type": "string" },
                    "stdin": { "type": "string" },
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
        Some("exec") => exec_tool(args),
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

fn exec_tool(a: &Value) -> Result<(String, Value), String> {
    let target = a["target"].as_str().ok_or("target is required")?;
    let argv: Option<Vec<String>> = a["argv"].as_array().map(|v| {
        v.iter()
            .filter_map(|x| x.as_str().map(String::from))
            .collect()
    });
    let command = a["command"].as_str().map(String::from);
    if argv.is_some() == command.is_some() || argv.as_ref().is_some_and(Vec::is_empty) {
        return Err("give exactly one of argv (non-empty) or command".into());
    }
    let env = Envelope {
        argv,
        command,
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
