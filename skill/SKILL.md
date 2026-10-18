---
name: Mesh
description: The user has a mesh of devices (Linux, macOS, Windows), all running opencode. Use when they mention another device or want something done on one.
---

# Mesh

The user has a bunch of devices. If they ask, you can do anything on any of them.

Remote exec of the opencode CLI always works: every device runs opencode, and
`opencode` as the command resolves to that device's install. For opencode usage
(CLI, `opencode run`, `opencode api`, sessions), use the `opencode` skill.

## Reference

    mesh status [target] [--fast]           list devices: os, shell, online, opencode version, busy
    mesh exec <target> <cmd> [args…]        run an exact argv, no shell
    mesh exec <target> -c '<string>'        run a string in that device's shell (see `shell` in status)
        -w <dir>  -e KEY=VAL  -t  --timeout <s>  --json
    mesh cp [-r] <src> <dst>                copy files; paths are local or device:path (device→device works)

Targets: `name` · `a,b` · `all` (includes this device) · `self` · `os:linux|macos|windows` · `--exclude <list>`

MCP tools `mesh_status` and `mesh_exec` are the same commands with JSON output.

If `mesh` isn't on PATH, use the CLI path from the mesh MCP instructions.

Exit codes: the remote command's own code · 255 = device unreachable. Offline devices are skipped.

On Windows and macOS devices, tools that keep their login in the OS credential store (like `gh`) may not be logged in inside `mesh exec`.

Don't send work back to the device that sent it to you (`MESH_FROM` is set on remote commands).

## Examples

    mesh exec server opencode run "pull main and run the tests"
    mesh exec all opencode api session.active
    mesh exec laptop -c 'docker ps | grep web'
    mesh cp -r ./dist server:~/deploy/
    mesh exec -t server opencode            # give the user the remote TUI
