//! Argument parsing, help, dispatch. Rendering lives in `render`.

pub(crate) mod render;

use crate::app::exec::{self, Input};
use crate::app::{self, Fail, cp, status, sync, up};
use crate::model::{Envelope, ExecReport, Step};
use render::Out;
use std::cell::RefCell;
use std::io::Write;

const HELP: &str = "\
mesh · join your devices into an opencode mesh

usage: mesh <command> [flags]

Setup      up · sync · rm · leave · update
Use        status · exec · cp
Internal   mcp
Global     --json  -q/--quiet  -v/--verbose  -h/--help  -V/--version

  up       create a mesh or join this device     mesh up [--repo <owner/name>] [--path <dir>] [--name <device>]
                                                   [--public] [--yes] [--prefer mesh|local] [--check] [--wait]
                                                   [--skip sshd,mcp,schedule] [--upload-skill <id,...>]
  sync     pull the mesh and apply it here       mesh sync [--force] [--keys-only]
  rm       remove a device from the mesh         mesh rm <device> [--yes]
  leave    remove this device from the mesh      mesh leave [--yes]
  update   update mesh on every device           mesh update [--version <x.y.z>] [--check]
  status   list devices                          mesh status [<target>] [--fast]
  exec     run a command on devices              mesh exec [-t] [-w <dir>] [-e K=V]... [--timeout <s>] [--durable] <target> <cmd> [args…]
                                                 mesh exec [flags] <target> -c '<string>'
  cp       copy files between devices            mesh cp [-r] <src> <dst>      (each side: path or <device>:<path>)
  mcp      stdio MCP server (started by opencode)

Targets    name · a,b · all · self · os:linux|macos|windows · -x/--exclude <list>
";

/// Run the CLI; returns the process exit code.
pub fn run(args: Vec<String>) -> i32 {
    app::isolate_std_handles();
    let mut out = Out::default();
    let mut help = false;
    let mut cmd: Option<String> = None;
    let mut rest = Vec::new();
    let mut it = args.into_iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--json" => out.json = true,
            "-q" | "--quiet" => out.quiet = true,
            "-v" | "--verbose" => app::set_verbose(true),
            "-h" | "--help" => help = true,
            // after a command, `--version` is that command's (`mesh update --version <v>`)
            "--version" if cmd.is_some() => rest.push(a),
            "-V" | "--version" => {
                println!("mesh {}", app::VERSION);
                return 0;
            }
            _ if cmd.is_none() => {
                // exec's arguments after the target belong to the remote command
                let exec = a == "exec";
                cmd = Some(a);
                if exec {
                    rest.extend(it.by_ref());
                }
            }
            _ => rest.push(a),
        }
    }
    let Some(cmd) = cmd else {
        print!("{HELP}");
        return if help { 0 } else { 2 };
    };
    if help {
        print!("{HELP}");
        return 0;
    }
    let result = match cmd.as_str() {
        "sync" => sync(&rest, &out),
        "status" => status(&rest, &out),
        "up" => up(&rest, &out),
        "exec" => exec(&rest, &mut out),
        "cp" => cp(&rest),
        "mcp" => Ok(crate::mcp::run()),
        "mux" => app::mux::run(),
        "elevated" => up::elevated(rest.first().map_or("", String::as_str)),
        "update" => update(&rest),
        "rm" => rm(&rest),
        "leave" => leave(&rest),
        _ => Err(Fail(
            2,
            format!("unknown command {cmd:?} (see mesh --help)"),
        )),
    };
    result.unwrap_or_else(|Fail(code, msg)| {
        render::error(&msg, &out);
        code
    })
}

/// Entry for the windowless `meshw` build (scheduled tasks, the elevated step).
pub fn run_background(args: Vec<String>) -> i32 {
    let name = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["sync", "--keys-only", ..] => "keys",
        [first, ..] => first,
        [] => "meshw",
    }
    .to_string();
    app::background(&name, &args.join(" "));
    let code = run(args);
    println!("exit {code}");
    code
}

fn usage(cmd: &str, msg: impl std::fmt::Display) -> Fail {
    Fail(2, format!("{cmd}: {msg}"))
}

fn sync(args: &[String], out: &Out) -> Result<i32, Fail> {
    let (mut force, mut keys_only) = (false, false);
    for a in args {
        match a.as_str() {
            "--force" => force = true,
            "--keys-only" => keys_only = true,
            _ => return Err(usage("sync", format!("unexpected argument {a:?}"))),
        }
    }
    render::sync(&sync::run(force, keys_only)?, out);
    Ok(0)
}

fn status(args: &[String], out: &Out) -> Result<i32, Fail> {
    let (mut fast, mut target, mut exclude) = (false, None, None);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--fast" => fast = true,
            "-x" | "--exclude" => {
                exclude = Some(
                    it.next()
                        .ok_or_else(|| usage("status", "--exclude needs a device list"))?
                        .as_str(),
                );
            }
            s if s.starts_with('-') => return Err(usage("status", format!("unknown flag {s}"))),
            s if target.is_none() => target = Some(s),
            s => return Err(usage("status", format!("unexpected argument {s:?}"))),
        }
    }
    render::status(&status::run(target, exclude, fast)?, out);
    Ok(0)
}

fn up(args: &[String], out: &Out) -> Result<i32, Fail> {
    let mut o = up::Opts {
        repo: None,
        path: String::new(),
        name: None,
        public: false,
        yes: false,
        prefer_local: false,
        upload_skills: Vec::new(),
        check: false,
        wait: false,
        skip: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || {
            it.next()
                .cloned()
                .ok_or_else(|| usage("up", format!("{a} needs a value")))
        };
        match a.as_str() {
            "--repo" => o.repo = Some(value()?),
            "--path" => o.path = value()?,
            "--name" => o.name = Some(value()?),
            "--public" => o.public = true,
            "--yes" | "-y" => o.yes = true,
            "--prefer" => {
                o.prefer_local = match value()?.as_str() {
                    "local" => true,
                    "mesh" => false,
                    v => {
                        return Err(usage(
                            "up",
                            format!("--prefer takes mesh or local, not {v:?}"),
                        ));
                    }
                }
            }
            "--check" => o.check = true,
            "--wait" => o.wait = true,
            "--skip" => o.skip = value()?.split(',').map(|s| s.trim().to_string()).collect(),
            "--upload-skill" => o.upload_skills.extend(
                value()?
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty()),
            ),
            _ => return Err(usage("up", format!("unexpected argument {a:?}"))),
        }
    }
    let steps = RefCell::new(Vec::<Step>::new());
    let json = out.json;
    let step = |s: Step| {
        if json {
            steps.borrow_mut().push(s);
        } else {
            render::step(&s);
        }
    };
    let ask = |q: &str, choices: &str| -> char { prompt(q, choices) };
    let progress = |name: &str, t: std::time::Duration| {
        if !json {
            render::progress(name, t);
        }
    };
    let ui = up::Ui {
        step: &step,
        ask: &ask,
        progress: &progress,
    };
    let code = up::run(&o, &ui)?;
    if json {
        render::json(&serde_json::json!({ "steps": steps.into_inner(), "exit": code }));
    }
    Ok(code)
}

fn exec(args: &[String], out: &mut Out) -> Result<i32, Fail> {
    // remote side of the handoff (hidden)
    if args.first().map(String::as_str) == Some("self") {
        match args.get(1).map(String::as_str) {
            Some("--envelope") => return Ok(app::serve::envelope(args.get(2).map(String::as_str))),
            Some("--serve") => return Ok(app::serve::serve()),
            #[cfg(windows)]
            Some("--run-queued") => return Ok(app::durable::run_queued()),
            _ => {}
        }
    }
    let mut env = Envelope::default();
    let (mut tty, mut exclude) = (false, None);
    let mut i = 0;
    while let Some(a) = args.get(i) {
        let mut value = || {
            i += 1;
            args.get(i)
                .cloned()
                .ok_or_else(|| usage("exec", format!("{a} needs a value")))
        };
        match a.as_str() {
            "-t" => tty = true,
            "--durable" => env.durable = true,
            "-w" => env.cwd = Some(value()?),
            "-e" => {
                let kv = value()?;
                let (k, v) = kv
                    .split_once('=')
                    .ok_or_else(|| usage("exec", "-e takes KEY=VAL"))?;
                env.env.insert(k.into(), v.into());
            }
            "--timeout" => {
                env.timeout = Some(
                    value()?
                        .parse()
                        .map_err(|_| usage("exec", "--timeout takes seconds"))?,
                )
            }
            "-x" | "--exclude" => exclude = Some(value()?),
            "--json" => out.json = true,
            "-q" | "--quiet" => out.quiet = true,
            "-v" | "--verbose" => app::set_verbose(true),
            "-h" | "--help" => {
                print!("{HELP}");
                return Ok(0);
            }
            s if s.starts_with('-') => {
                return Err(usage(
                    "exec",
                    format!("unknown flag {s} (flags go before <target>)"),
                ));
            }
            _ => break,
        }
        i += 1;
    }
    let target = args
        .get(i)
        .ok_or_else(|| usage("exec", "missing <target>"))?;
    let mut cmd: Vec<String> = args[i + 1..].to_vec();
    if cmd.first().map(String::as_str) == Some("--") {
        cmd.remove(0);
    }
    match cmd.first().map(String::as_str) {
        Some("-c") if cmd.len() == 2 => env.command = cmd.pop(),
        Some("-c") => return Err(usage("exec", "-c takes exactly one string")),
        None => return Err(usage("exec", "missing <cmd>")),
        _ => env.argv = Some(cmd),
    }
    let (me, devices) = exec::targets(target, exclude.as_deref())?;
    if devices.is_empty() {
        return Err(Fail(1, format!("no devices match {target}")));
    }
    if tty {
        if env.durable {
            return Err(usage("exec", "--durable can't take a terminal (-t)"));
        }
        let [d] = devices.as_slice() else {
            return Err(usage("exec", "-t needs exactly one device"));
        };
        return exec::interactive(&me, d, &env);
    }
    if out.json {
        let results = exec::capture(&me, &devices, &env, Input::Inherit, usize::MAX);
        let code = match results.as_slice() {
            [r] => r.exit.unwrap_or(255),
            rs if rs.iter().all(|r| r.exit == Some(0)) => 0,
            _ => 1,
        };
        render::json(&ExecReport { results });
        return Ok(code);
    }
    Ok(render::exec_live(&me, &devices, &env))
}

fn cp(args: &[String]) -> Result<i32, Fail> {
    let mut recursive = false;
    let mut paths = Vec::new();
    for a in args {
        match a.as_str() {
            "-r" => recursive = true,
            _ => paths.push(a.as_str()),
        }
    }
    let [src, dst] = paths.as_slice() else {
        return Err(usage("cp", "usage: mesh cp [-r] <src> <dst>"));
    };
    cp::run(recursive, src, dst)
}

/// Ask until one of `choices` is typed; EOF picks the first choice.
fn prompt(q: &str, choices: &str) -> char {
    loop {
        print!("  ? {q} ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
            return choices.chars().next().unwrap_or('n');
        }
        let c = line.trim().chars().next().map(|c| c.to_ascii_lowercase());
        if let Some(c) = c.filter(|c| choices.contains(*c)) {
            return c;
        }
    }
}

/// Yes/no confirmation; without a terminal it says no (pass --yes).
fn confirm(q: &str) -> bool {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() {
        eprintln!("mesh: not a terminal; pass --yes to confirm");
        return false;
    }
    prompt(q, "ny") == 'y'
}

fn update(args: &[String]) -> Result<i32, Fail> {
    let mut o = app::update::Opts {
        version: None,
        check: false,
        resume: false,
    };
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--version" => {
                o.version = Some(
                    it.next()
                        .cloned()
                        .ok_or_else(|| usage("update", "--version needs x.y.z"))?,
                )
            }
            "--check" => o.check = true,
            "--resume" => o.resume = true,
            _ => return Err(usage("update", format!("unexpected argument {a:?}"))),
        }
    }
    app::update::run(&o, &|s| render::step(&s))
}

fn rm(args: &[String]) -> Result<i32, Fail> {
    let yes = args.iter().any(|a| a == "--yes" || a == "-y");
    let names: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let [name] = names.as_slice() else {
        return Err(usage("rm", "usage: mesh rm <device> [--yes]"));
    };
    app::rm::rm(name, yes, &confirm, &|s| render::step(&s))
}

fn leave(args: &[String]) -> Result<i32, Fail> {
    let yes = match args {
        [] => false,
        [a] if a == "--yes" || a == "-y" => true,
        _ => return Err(usage("leave", "usage: mesh leave [--yes]")),
    };
    app::rm::leave(yes, &confirm, &|s| render::step(&s), &render::progress)
}
