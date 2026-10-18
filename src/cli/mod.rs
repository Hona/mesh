//! Argument parsing, help, dispatch. Rendering lives in `render`.

mod render;

use crate::app::{self, Fail};
use render::Out;

const HELP: &str = "\
mesh · join your devices into an opencode mesh

usage: mesh <command> [flags]

Setup      up · sync · rm · leave · update
Use        status · exec · cp
Internal   mcp
Global     --json  -q/--quiet  -v/--verbose  -h/--help  -V/--version

  up       create a mesh or join this device     mesh up [--repo <owner/name>] [--name <device>] …
  sync     pull the mesh and apply it here       mesh sync [--force] [--keys-only]
  rm       remove a device from the mesh         mesh rm <device> [--yes]
  leave    remove this device from the mesh      mesh leave [--yes]
  update   update mesh on every device           mesh update [--version <x.y.z>] [--check]
  status   list devices                          mesh status [<target>] [--fast]
  exec     run a command on devices              mesh exec <target> <cmd> [args…]
  cp       copy files between devices            mesh cp [-r] <src> <dst>
  mcp      stdio MCP server (started by opencode)

Targets    name · a,b · all · self · os:linux|macos|windows · -x/--exclude <list>
";

/// Run the CLI; returns the process exit code.
pub fn run(args: Vec<String>) -> i32 {
    let mut out = Out {
        json: false,
        quiet: false,
    };
    let mut help = false;
    let mut rest = Vec::new();
    for a in args {
        match a.as_str() {
            "--json" => out.json = true,
            "-q" | "--quiet" => out.quiet = true,
            "-v" | "--verbose" => app::set_verbose(true),
            "-h" | "--help" => help = true,
            "-V" | "--version" => {
                println!("mesh {}", app::VERSION);
                return 0;
            }
            _ => rest.push(a),
        }
    }
    let Some((cmd, args)) = rest.split_first() else {
        print!("{HELP}");
        return if help { 0 } else { 2 };
    };
    if help {
        print!("{HELP}");
        return 0;
    }
    let result = match cmd.as_str() {
        "sync" => sync(args, &out),
        "status" => status(args, &out),
        "up" | "rm" | "leave" | "update" | "exec" | "cp" | "mcp" => {
            Err(Fail(1, format!("{cmd}: not implemented yet")))
        }
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

fn sync(args: &[String], out: &Out) -> Result<i32, Fail> {
    let (mut force, mut keys_only) = (false, false);
    for a in args {
        match a.as_str() {
            "--force" => force = true,
            "--keys-only" => keys_only = true,
            _ => return Err(Fail(2, format!("sync: unexpected argument {a:?}"))),
        }
    }
    render::sync(&app::sync::run(force, keys_only)?, out);
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
                        .ok_or_else(|| Fail(2, "--exclude needs a device list".into()))?
                        .as_str(),
                );
            }
            s if s.starts_with('-') => return Err(Fail(2, format!("status: unknown flag {s}"))),
            s if target.is_none() => target = Some(s),
            s => return Err(Fail(2, format!("status: unexpected argument {s:?}"))),
        }
    }
    render::status(&app::status::run(target, exclude, fast)?, out);
    Ok(0)
}
