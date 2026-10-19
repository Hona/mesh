---
name: Mesh
description: The user has a mesh of devices (Linux, macOS, Windows), all running opencode. Use when they mention another device or want something done on one.
---

# Mesh

The user has a bunch of devices. If they ask, you can do anything on any of them.

Remote exec of the opencode CLI always works: every device runs opencode, and `opencode` as the command resolves to that device's install. For opencode usage (CLI, `opencode run`, `opencode api`, sessions), use the `opencode` skill.

## Reference

    mesh status [target] [--fast]       devices: os, shell, online, opencode version, busy sessions, daemon
    mesh exec <target> <cmd> [args…]    one program, arguments passed exactly (no shell)
    mesh exec <target> -c '<line>'      one line of that device's shell (pwsh, bash, … see status)
    mesh exec <target> -s < file        a script of any length for that device's shell, read from stdin
        -w <dir>  -e KEY=VAL  -t (terminal)  --timeout <s>  --no-stall  --json
    mesh cp [-r] <src> <dst>            files; each side a local path or device:path

MCP: `mesh_status`, and `mesh_exec` with `argv` (one program) or `script` (shell code), plus `stdin`, `cwd`, `env`, `timeout`, `stall`.

Targets: `name` · `a,b` · `all` · `self` · `os:linux|macos|windows` · `--exclude <list>`

Commands run in the user's own session on the device: what they start keeps running, and logins such as `gh` work. Without stdin, a command's stdin is empty.

Exit codes: the command's own · `124` timeout, or stalled (no output and an idle process for 20 s; `--no-stall` / `stall: false` for commands that wait quietly) · `255` device unreachable.

## Examples

    mesh exec win-pc -c 'Get-ChildItem $env:USERPROFILE\Downloads | Sort-Object LastWriteTime | Select-Object -Last 5'
    mesh exec win-pc winget upgrade --all --silent
    mesh exec server -c 'df -h / && docker ps --format "{{.Names}}"'
    mesh exec server opencode api session.active
    mesh_exec { "target": "laptop", "argv": ["git", "-C", "~/src/app", "status", "--short"] }
    mesh_exec { "target": "win-pc", "script": "$p = Get-Process | Sort-Object WS -Descending\n$p | Select-Object -First 5 Name, WS\n" }
    mesh_exec { "target": "server", "script": "cd ~/app\ngit pull\nnpm ci && npm test\n", "timeout": 600 }

## Recipes

Recovery. `mesh status` OPENCODE: `(service down)` not running · `(service stale)` crashed · `(service hung)` not answering for 4 s (a busy service can miss once: check twice). `mesh` doesn't need opencode: fix a device from another one.

    mesh exec pc opencode service start                 # down / stale
    mesh exec --timeout 20 pc opencode service stop     # hung; if that times out, kill the pid, then start
    mesh exec pc -c 'Stop-Process -Force -Id (Get-Content ~/.local/state/opencode/service.json | ConvertFrom-Json).pid'
    mesh exec pc -c 'Get-Content ~/.local/share/opencode/log/opencode.log -Tail 50'
    mesh exec pc opencode upgrade [<version>]           # latest, or roll back

A machine outside the mesh, through a device that can reach it: `mesh_exec { "target": "server", "argv": ["ssh", "-T", "root@10.0.0.5", "bash", "-s"], "stdin": "apt-get update\napt-get install -y nginx\n" }`.

Handing a task to another device's opencode: `mesh exec server opencode run "pull main and run the tests"` (with `-w <dir>` for the project).

## Notes

- Offline devices are skipped and reported as offline.
- `MESH_FROM` (and `MESH_SESSION` from MCP) is set on remote commands: don't send work back to the device that sent it to you.
- If `mesh` isn't on PATH, use the CLI path from the mesh MCP instructions.
