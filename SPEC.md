# mesh — spec

`mesh` is a tiny native binary that joins a user's devices (Linux, macOS, Windows) into an
**opencode mesh**: from any opencode session, on any device, the agent can run anything —
including the `opencode` CLI — on any other device.

This file is the source of truth. If an implementation idea conflicts with it, stop and ask.
When a design change is agreed, update this file in the same change.

---

## 1. Rules (non-negotiable)

1. **Performance first.** Low CPU and RAM; low latency and high throughput between devices.
   Never spawn a helper process where an OS API or a file read works. Minimize process spawns
   and network round trips. Parallelize independent work. Stream large output, don't buffer it.
2. **No automated tests. Ever.** No unit, integration, e2e, snapshot, or any other automated
   tests. No test files, no test dependencies, no test steps in CI. Verification is manual
   (run the command on real devices and look). CI only builds release binaries.
3. **Early iteration speed beats everything else.** Smallest change that works. No speculative
   abstraction.
4. **Automatic actions only pull.** Background/scheduled work never writes to the mesh repo.
   Repo writes happen only from explicit commands (`up`, `update`, `rm`, `leave`) or the user
   pushing by hand.
5. **Never break user space.** Mesh only adds marked managed blocks or mesh-owned files, all
   removable by `mesh leave`. It never edits `sshd_config`, never changes a default shell, never
   rewrites lines it didn't write. The one deliberate exception: `~/.config/opencode/AGENTS.md`
   is mesh-owned (see §7.4), with backups. Mesh never reloads or restarts the user's opencode
   while sessions may be running (`opencode reload` cancels every running tool call), except the
   explicit, guarded cases in §5.3. OpenCode picks up skill and `AGENTS.md` changes by itself.
6. **Opt-in targeting.** Commands reach only devices the user names (§5.1).
7. **Raw, unopinionated tools.** Two MCP tools (`status`, `exec`). No task-specific tools.
   Everything else is composed from `mesh exec <device> opencode …` or plain commands.
8. **Remote exec depends only on the OS SSH server and the mesh binary, never on opencode**
   (service, MCP or desktop app), so a broken opencode can always be repaired remotely.
8. **No master.** Every device is equal. Any device can join, leave, go offline, or come back
   at any time.
9. **Tiny.** Rust stable. Dependencies: `serde` + `serde_json` only (ask before adding any
   crate). No async runtime, no TLS, no git library, no CLI framework. Shell out to `gh`,
   `ssh`, `scp`, `ssh-keygen`, `tailscale`, `opencode`.
10. **One-time setup does every privileged / permission step.** `mesh up` walks the user through
    all of it while they're at the device. After that nothing prompts; `sync`/`status` only
    report problems.
11. **One concept, one name.** Each MCP tool is exactly a CLI command: same name, same
    arguments (MCP `script` = CLI `-s`), same JSON as `--json`.
12. **No backwards compatibility while pre-1.0.** Breaking changes are fine; clean up old
    installs by hand or re-run `up`. No migration code, no handling of older builds' formats.

## 2. Invariants (assumed on every device)

- `gh` installed and authenticated (with access to the mesh repo and the tool repo).
- `opencode` installed — CLI, desktop app, or both.
- Tailscale installed and logged in.
- OpenSSH client (`ssh`, `scp`, `ssh-keygen`) — ships with Windows 10+, macOS, Linux.

These tools may be installed but **not on PATH** (e.g. desktop-app shells). Mesh must look in
known install locations, not just PATH.

## 3. Glossary

| Term | Meaning |
|---|---|
| mesh | A set of devices sharing one mesh repo |
| device | A machine that has run `mesh up` for the mesh. Being on Tailscale alone is not membership |
| mesh repo | The GitHub repo holding the mesh (default `<gh-user>/opencode-mesh`) |
| tool repo | This repo, `Hona/mesh` — source and releases of the `mesh` binary |
| target | Which devices a command runs on (§5.1) |

## 4. Repos and files

### 4.1 Tool repo — `Hona/mesh` (private for now)

```
Cargo.toml
src/            # one package: lib + bins mesh, meshw (Windows-only windowless build); §4.5
skill/SKILL.md  # the built-in mesh skill, embedded into the binary (include_str!)
schema/v1.json  # JSON schema for mesh.json
install.sh      # macOS/Linux installer (fetched via gh)
install.ps1     # Windows installer (fetched via gh)
.github/workflows/release.yml   # build + release only, no tests
SPEC.md  AGENTS.md  README.md
```

### 4.2 Mesh repo — `<gh-user>/opencode-mesh` (private by default)

Created by the first `mesh up`. Its root mirrors `~/.config/opencode/` for shared content.

```
mesh.json                   # marks the repo as a mesh; shared settings
devices/
  <name>.json               # device descriptor (written by that device's `mesh up`)
  <name>.pub                # device's mesh public key  → everyone's authorized keys
  <name>.host.pub           # device's SSH host key     → everyone's mesh known_hosts
AGENTS.md                   # shared global instructions
skills/<id>/SKILL.md        # shared skills
skills/mesh/SKILL.md        # built-in mesh skill, managed by the mesh CLI (§9)
```

`mesh up --repo <owner/name> --path <dir>` may point at a subfolder of any repo (e.g. an
existing dotfiles repo); everything above is then relative to `<dir>`.

#### `mesh.json`

```jsonc
{
  "$schema": "https://raw.githubusercontent.com/Hona/mesh/main/schema/v1.json",
  "pin": "0.2.0",      // mesh version every device converges to (set by `mesh update`)
  "interval": "10m"    // how often devices pull (default 10m)
}
```

All fields optional. The `$schema` URL carries the file-format version (`v1`).
Not in v1: `sources` (pulling skills from other people's repos).

#### `devices/<name>.json`

```jsonc
{
  "name": "blade",               // mesh device name
  "host": "lukeblade",           // network host name (Tailscale name); address resolved locally at sync time
  "os": "windows",               // linux | macos | windows
  "arch": "x64",                 // x64 | arm64
  "shell": "pwsh",               // shell mesh uses for `exec -c` (see below)
  "user": "lukem",               // SSH login user
  "mesh": "C:\\Users\\lukem\\AppData\\Local\\Programs\\mesh\\mesh.exe"   // absolute path
}
```

No opencode path is stored: it changes on every opencode update (§6.4).
No Tailscale IPs or tailnet domain are stored (public-safe).

`shell` is the shell mesh itself runs `exec -c` strings in, chosen by `mesh up` and then
static: Windows `pwsh` → `powershell` → `cmd`, the first installed (file checks only; pwsh via
PATH, then `%ProgramFiles%\PowerShell\7\pwsh.exe`); Linux/macOS the login shell. `up` records the
name here and the absolute path in local `state.json`; re-running `up` re-detects. It is not
sshd's DefaultShell, which mesh never reads or changes (it only wraps mesh's own handoff).

### 4.3 Device names

- Default: the Tailscale short hostname. Override: `mesh up --name <name>`. Mesh names are
  independent of Tailscale names (renaming in Tailscale is not required).
- Pattern: `^[a-z0-9][a-z0-9-]{1,62}$` (lowercase, 2+ chars).
- Reserved: `all`, `self`, anything containing `:` or `,`, single letters (Windows drive
  letters, so `C:\x` is never a device path).

### 4.4 Local state (per device, never in the repo)

State dir: `~/.local/share/opencode-mesh/` (respect `XDG_DATA_HOME`) on Linux/macOS;
`%LOCALAPPDATA%\opencode-mesh\` on Windows.

```
state.json        # { "registry": { "kind": "github", "repo", "path", "branch" },
                  #   "device", "shell": { "name": "pwsh", "path": "<abs path>" },
                  #   "synced": "<commit sha>", "mcp_port": 47474 }
sync.lock         # held during a sync (pid inside; taken over if that process is gone)
run/              # temp script files for `exec -s` on PowerShell / cmd (§5.5)
daemon-<build>.sock  # Linux/macOS: the daemon's local socket, one per build (dir is 0700)
mcp-token         # the MCP bearer token (§8), readable by this user only
id_ed25519(.pub)  # this device's mesh key (permissions locked to the user)
ssh_config        # generated; used via `ssh -F` for every mesh connection
known_hosts       # generated from devices/*.host.pub
cache/            # last pulled copy of the mesh repo + per-file sha index
backup/<time>/    # backups of anything mesh replaced (AGENTS.md, conflicting skills)
log/              # daemon.log, sync.log (rotated at 1 MB)
```

Installed binary: `~/.local/bin/mesh` (Linux/macOS; if that isn't on PATH, one marked line in the
`$SHELL` profile adds it, §5.3); `%LOCALAPPDATA%\Programs\mesh\{mesh,meshw}.exe` (Windows, added
to user PATH).
Everything internal (remote handoff, the daemon's autostart) uses absolute paths.

### 4.5 Architecture (`src/`)

One package `mesh`: a library plus thin binaries.

```
lib.rs          module declarations only
main.rs         bin `mesh`: parse args → app → exit code
bin/meshw.rs    bin `meshw`, Windows windowless build (starts the daemon, the elevated step)
cli/            argument parsing, help, text + JSON rendering
app/            commands (sync, status, up, exec, …): orchestration only
                → daemon.rs (§6.6), serve.rs (running commands; the relay)
model/          plain data: Mesh, Device, Target, State, reports
registry/       where the mesh lives            → github.rs (the only place that runs `gh`)
network/        how devices are found / online  → tailscale.rs (the only place that runs `tailscale`)
transport/      how commands reach devices      → ssh.rs; mux.rs, frame.rs (§6.5)
host/           this machine's OS integration   → paths, processes, local sockets, autostart, opencode
mcp/            MCP server (§8): JSON-RPC, Streamable HTTP for the daemon, stdio (hidden)
```

- `cli`, `mcp` → `app` → `registry`, `network`, `transport`, `host`, `model`. The daemon gets
  its MCP server passed in by `cli` (`app` never calls `mcp`).
- Backends (`registry`, `network`, `transport`, `host`) depend only on `model` (and `host` for
  paths), never on `app`/`cli` or on each other's internals. They return `model` types, never raw
  `gh`/`tailscale` JSON.
- One backend each today. No traits, backend enums, or plugin layers; keep each backend's public
  surface small so a second one (e.g. plain SSH without Tailscale, a web registry) is a local
  change. Selection happens in one place (`registry::open`).
- Idiomatic Rust: `pub(crate)` by default, `cargo fmt`, `cargo clippy` clean.

## 5. CLI

```
mesh <command> [flags]

Setup      up · sync · rm · leave · update
Use        status · exec · cp
Internal   daemon · mcp (hidden)
Global     --json  -q/--quiet  -v/--verbose  -h/--help  -V/--version
```

### 5.1 Targets

| Syntax | Meaning |
|---|---|
| `blade` | one device |
| `blade,air` | a list |
| `all` | every device in the mesh, including this one |
| `self` | this device |
| `os:linux` / `os:macos` / `os:windows` | every device with that OS |
| `-x <list>` / `--exclude <list>` | remove devices from the selection |

No `*`, no `!`, no `others`. Offline devices (per Tailscale) are reported as `offline`
immediately, never waited on. If a target names a device not in the local cache, run a sync
first, then fail if still unknown.

### 5.2 Exit codes

| Code | Meaning |
|---|---|
| 0 | success (all devices, for multi-target) |
| N | single-device `exec`: the remote command's exit code |
| 1 | general error, or any device failed (multi-target) |
| 2 | usage error |
| 3 | needs a human (setup step); exact command/instructions printed |
| 255 | mesh/transport failure (unreachable, SSH error) — same convention as `ssh` |

`--json` output carries an `error` field that disambiguates a remote 255 from a transport 255.

### 5.3 `mesh up`

Create a mesh or join this device to one. Idempotent; re-running is also the repair command, so
a joined device keeps its mesh and name unless `--repo` / `--name` change them.

```
mesh up [--repo <owner/name>] [--path <dir>] [--name <device>] [--public]
        [--yes] [--prefer mesh|local] [--check] [--wait] [--skip <step,...>]
```

| Flag | |
|---|---|
| `--repo` | mesh repo; default: the mesh this device is in, else `<gh-user>/opencode-mesh`; created if missing |
| `--path` | subfolder within the repo; default: the joined mesh's when `--repo` isn't given |
| `--name` | device name; default: this device's mesh name, else the Tailscale short name |
| `--public` | create the repo public (default private) |
| `--yes` | non-interactive (agents/scripts); `AGENTS.md` defaults to `--prefer mesh`, local skills stay local |
| `--prefer` | `AGENTS.md` only: `mesh` (keep mesh, back up local) or `local` (upload local). Never uploads skills |
| `--upload-skill` | `<id>[,<id>…]`, repeatable: upload these local skills to the mesh (the only non-interactive way) |
| `--check` | report what would change; change nothing |
| `--wait` | after joining, poll until peers have synced and accept this device |
| `--skip` | skip steps: `sshd`, `mcp` |

Steps (each checks first, prints ✓ / ! / ✗):

1. **Preflight** — find `gh` (+ auth), `opencode` (§6.4), `tailscale` (+ online, own hostname
   and IP), `ssh`/`scp`/`ssh-keygen`.
2. **Repo** — resolve; if missing, create it (private unless `--public`) with `mesh.json`,
   `skills/mesh/SKILL.md`, and (after confirming) this device's `AGENTS.md`.
3. **Key** — create `<state>/id_ed25519` if missing; lock permissions.
4. **SSH server + OS permissions** — §7.1. One UAC / one sudo / one guided macOS settings
   pass, only if needed. Detect the server's real default shell and the user's effective
   authorized-keys file.
5. **Conflicts** (joining an existing mesh) — §7.4: `AGENTS.md` and local skills.
6. **Device** — write `devices/<name>.{json,pub,host.pub}` in one commit (explicit push).
7. **Sync** — §5.6. Before it, the MCP endpoint is fixed: `state.json` `mcp_port` (47474, or
   the next free port up to +99; kept once chosen) and `<state>/mcp-token` (32 random bytes,
   hex, user-only file).
8. **Daemon** — §6.6: autostart registered (§7.2), this build's daemon running (it retires
   other builds' daemons once they're idle).
9. **MCP** — the global opencode entry `mesh` = `{"type": "remote", "url":
   "http://127.0.0.1:<port>/mcp", "oauth": false, "headers": {"Authorization": "Bearer
   {file:<state>/mcp-token>}"}}`: `opencode mcp add mesh --global --url … --header …`, then
   `"oauth": false` written into that entry (no flag for it; the file is the one `mcp add`
   names: a running service may not show the entry yet). opencode reads the token from the
   file, so the config holds no secret (no service environment to set). Once the daemon
   answers on the port (≤ 5 s), the same entry, token inline, goes live in every loaded
   location with `opencode api experimental.mcp.add` (`debug.location.list`): no reload. This
   registration is the only per-location step; updates need none. A different `mesh` entry:
   ask before replacing it.
10. **Self-test** — via the mesh SSH config to itself: `mesh exec self opencode --version`
    over SSH. Then `exec` a no-op on each online peer; peers that haven't synced yet are
    reported as "pending (≤ interval)".
11. **Opt-in extra** — offer to add one `Include <state>/ssh_config` line to `~/.ssh/config`
    so plain `ssh <device>` works. Skipped if any mesh device name clashes with an existing
    `Host` entry.
12. **PATH live** — only if step 1 added the install dir to PATH (Windows: user PATH;
    Linux/macOS: when `~/.local/bin` isn't on PATH, one line ending `# opencode-mesh: PATH` in
    the `$SHELL` profile: `~/.zshrc`, `~/.bashrc` (macOS `~/.bash_profile`),
    `~/.config/fish/conf.d/opencode-mesh.fish`, else `~/.profile`). The running opencode
    service and its agents' shells keep the old PATH, so `up` runs
    `opencode service restart` (the service's own binary, with the install dir added to PATH)
    when that cuts nothing off and can work: `up` isn't running inside opencode (`OPENCODE` /
    `OPENCODE_SESSION_ID` unset; opencode sets them for everything it starts), not in an SSH
    session on Windows (the new service would die with it), `session.active` is empty, and no
    live opencode client owns the service (its parent process is the desktop app or a TUI:
    that client starts the replacement itself, from its own old environment). After the
    restart, a replacement started by a live client also counts as not done. The restart
    closes opencode's own terminals, like any service restart. Otherwise:
    `! PATH  restart opencode (or the desktop app) so agents can run plain \`mesh\``. Until
    then agents use the CLI path in the MCP `instructions` (§8).

Example:

```
$ mesh up
mesh up · Hona/opencode-mesh · adais
  ✓ gh         logged in as Hona
  ✓ opencode   2.0.32  (service pid 14968)
  ✓ tailscale  adais (online)
  ✓ repo       Hona/opencode-mesh
  ✓ key        created
  ✓ shell      pwsh for exec -c
  ! sshd       installing OpenSSH Server via Windows Update: this can take several minutes
  ! sshd       install OpenSSH Server, …: approve the UAC prompt
  … sshd       still working · 2m40s
  ✓ sshd       running · keys: administrators_authorized_keys (managed task)
  ? AGENTS.md  differs from the mesh  [k]eep mesh / [u]pload this / [e]dit
  ✓ device     devices/adais.{json,pub,host.pub} pushed
  ✓ sync       3 devices · 2 skills
  ✓ daemon     running · starts at login, kept running
  ✓ mcp        registered "mesh" · http://127.0.0.1:47474/mcp · live in 6 opencode locations
  ✓ self-test  adais → opencode 2.0.32
  … peers      blade, air accept adais after their next sync (≤10m)
```

Any step that can block for more than a few seconds (the elevated step, the macOS settings
wait, `--wait`) prints an elapsed-time heartbeat (same line on a terminal, else every ~30 s).
`--check` refreshes the local cache but changes nothing else.

### 5.4 `mesh status`

```
mesh status [<target>] [--fast] [--json]
```

Default target `all`. Without `--fast`, each online device is probed in parallel over the
handoff (§6.2) by running `mesh status self --json` there, which reports local facts:
opencode version + the service state (below), busy session count (`opencode api
session.active`, only asked of a service that answers), daemon `up` / `down` (a connect to its
socket, no spawn), mesh version. `--fast` uses only the repo cache + Tailscale.

```
DEVICE   OS       SHELL       ONLINE  OPENCODE  BUSY  DAEMON  MESH
adais *  windows  pwsh        yes     2.0.32    0     up      0.1.0
blade    windows  powershell  yes     2.0.32    1     up      0.1.0
air      macos    zsh         no      -         -     -       -
```

Also shows: pending binary updates (`0.1.0 → 0.2.0 (pending)`), a customised mesh skill,
local `AGENTS.md` edits that were backed up, last sync time/commit.

OPENCODE is the version of the opencode `mesh exec <device> opencode` would run (§6.4). If the
running service is a different version it is appended, `2.0.28 (service 0.0.0-beta-21153)`
(JSON: `opencode.service_version`, present only then).

The service state (JSON `opencode.service`) is shown when it isn't `ok`, e.g.
`2.0.28 (service hung)`:

| State | Meaning | Check |
|---|---|---|
| `ok` | answers | `GET /api/info` on `service.json`'s url gets any HTTP status (401 too; the password is never read) within 4 s |
| `down` | not running | no `service.json` (stopped cleanly) |
| `stale` | crashed / killed | `service.json` names a dead pid |
| `hung` | alive, not answering | pid alive, no HTTP answer within 4 s (connect 1 s; a busy service answered in 1.8 s right after MCP restarts, so 2 s was too tight) |

A raw `TcpStream` with timeouts, no `opencode` spawn. `status` never hangs on a broken opencode:
every opencode spawn mesh makes (version, busy, config, MCP restarts) has a hard limit
(3 s for status probes, 20 s otherwise) and is skipped when the service doesn't answer.

A running desktop app owns its service and starts it again by itself (verified on Blade): within
~1 s after `opencode service stop`, a little later after a kill; `opencode service start` then
adopts whichever registers first. `hung` can show once while a busy service catches up (seen
for a few seconds right after MCP restarts).

### 5.5 `mesh exec`

Run a command on devices.

```
mesh exec [-t] [-w <dir>] [-e KEY=VAL]... [--timeout <s>] [--no-stall] [--json] <target> <cmd> [args…]
mesh exec [flags] <target> -c '<string>'
mesh exec [flags] <target> -s            # script on stdin
```

| Rule | Precedent |
|---|---|
| Default: run the exact argv, **no shell**, identical on every OS | `docker exec`, `kubectl exec` |
| `-c '<string>'`: run the string in the device's shell (§4.2; PowerShell gets `-NoLogo -NoProfile -NonInteractive -Command`, cmd `/d /s /c`) | `sh -c`, `ssh host '…'` |
| `-s`: read a script from stdin and run it in the device's shell, no quoting: unix shells get it on stdin (`-s`; fish none); PowerShell a temp `.ps1` (UTF-8 BOM) with `-NoLogo -NoProfile -NonInteractive -ExecutionPolicy Bypass -File`, cmd a temp `.cmd` (`@echo off`, CRLF) with `/d /c`, both in `<state>/run/`, deleted when it ends. (`-Command -` reads stdin line by line like a prompt: an `else` on its own line silently ends the script.) | `bash -s`, `ssh host 'bash -s' < x` |
| argv[0] `opencode` or `mesh` resolves to that device's install (§6.4) | — |
| stdin is passed through when it's a pipe/file; otherwise (terminal, null device, closed) the command's stdin is the null device, so it sees EOF at once (use `-t` for a terminal) | `ssh -n`, `docker exec` |
| `-t` allocates a TTY (interactive; e.g. `mesh exec -t blade opencode` = remote TUI) | `ssh -t`, `docker exec -it` |
| `-w` working dir, `-e` env var | `docker exec -w / -e` |
| Everything after `<target>` belongs to the command; `--` is optional | `docker exec`, `ssh` |

- Single device: output streams live; exit code passed through.
- Multiple devices: run in parallel. Text output prefixes each line with `device │ `, then a
  summary line (`✓ blade 0 1.2s · ✗ air 1 0.4s · ⨯ vps offline`). `--json` returns results.
- `--timeout`: enforced **on the remote side** (kills the whole process tree: a job object on
  Windows, a process group on unix; exit code 124, `error: "timeout after Ns"`), plus a local
  backstop at timeout + 15 s. CLI default: none.
- The tree is killed **only** on timeout, cancel (Ctrl-C, MCP cancel) or the client going away
  mid-command. A command that exits by itself leaves what it started running (Windows: the job
  stops being kill-on-close; it allows breakaway).
- Commands run in the device's daemon (§6.6), in the user's own login session: what they start
  outlives the SSH session, and the OS credential store (`gh`'s login) works. Their base
  environment: Linux/macOS, the user's login shell's, as in a new terminal (`<shell> -l -i -c
  'env -0'`, stdin null, ≤ 5 s, minus `PWD`/`OLDPWD`/`SHLVL`/`_`; captured once per process and
  again only when a profile file's mtime changes: `~/.zshenv`, `.zprofile`, `.zshrc`, `.zlogin`,
  `.bash_profile`, `.bash_login`, `.bashrc`, `.profile`, fish `config.fish`, `/etc/zprofile`,
  `/etc/profile`, `/etc/paths`: stat only, no spawn per command); Windows, the SSH session's
  (built fresh from the registry), minus `SSH_*` / `XDG_SESSION_*`. On top: `NO_COLOR`/`TERM`,
  `-e`, `MESH_FROM`, `MESH_SESSION` (with `-t`, the terminal's `TERM`/`COLORTERM`). Daemon
  unreachable (nobody logged on, crashed and not restartable): they run in the SSH session as a
  fallback. Each result says where: `"via": "daemon" | "ssh"` (text: `· via ssh (its daemon is
  down)`).
- **Stall**: a command with no output whose process tree has used < 1% CPU and done no I/O for
  20 s (sampled every 5 s: job accounting on Windows, the process group's `/proc` stat + io on
  Linux; macOS: not detected) is killed: exit 124, `error: "stalled"`, `hint: "no output and
  idle for 20 s; is it waiting for input?"`, the output so far; the CLI prints `mesh: <device>:
  stalled: <hint>`. Not while stdin is still streaming in, not with `-t`. One timer per link (a
  thread per one-shot command). Opt out: `--no-stall` (MCP `stall: false`).
  Internal execs (`status`, `update`, `cp`, `up`) don't ask for it.
- **Plain output**: non-`-t` commands get `NO_COLOR=1` and `TERM=dumb` (before `-e`; PowerShell
  7.2+ then renders `PlainText`). `--json` / MCP results drop ANSI escape sequences and turn
  CRLF into LF; the live CLI stream is raw.
- Windows: a command's stdio are named pipes opened for overlapped I/O (like sshd's own), not
  std's synchronous anonymous pipes: with those, Win32-OpenSSH's `ssh.exe` inside an SSH
  session prints nothing while its stdin is open and never exits with a stdout pipe.
- A target naming a device missing from the cache runs one sync first (§5.1).
- The remote process gets `MESH_FROM=<calling device>` (and `MESH_SESSION`, §8.2).
- Without a daemon to reach, inside an SSH session on Windows (the fallback above): one-shot SSH
  per device with the envelope hex-encoded in argv, stdin null, and stdout/stderr to files in
  `<state>/run/` that are tailed live (ssh.exe's pipe trouble, above). No stdin.
- Long-lived background commands: the command's own way to background (`Start-Process`,
  `nohup … &`), or opencode's shells (`opencode api shell.create …`).

```sh
mesh exec blade opencode run "fix the failing test"
mesh exec blade -w D:\Hona\app opencode run --format json "fix the failing test"
mesh exec all opencode api session.active
mesh exec -t blade opencode
mesh exec air docker ps
mesh exec air -c 'docker ps | grep web'
mesh exec self opencode --version
```

JSON result (also the MCP `exec` result):

```jsonc
{
  "results": [
    { "device": "blade", "exit": 0, "ms": 1210, "stdout": "…", "stderr": "", "truncated": false,
      "via": "daemon" },
    { "device": "vps", "exit": 124, "ms": 20210, "stdout": "", "stderr": "", "truncated": false,
      "error": "stalled", "hint": "no output and idle for 20 s; is it waiting for input?",
      "via": "daemon" },
    { "device": "air", "error": "offline" }
  ]
}
```

### 5.6 `mesh sync`

```
mesh sync [--force]
```

Pull-only. Never writes to the repo.

1. Fetch the branch head commit (one call). If it equals `synced` and there's no `--force`,
   skip the listing, downloads and network lookup (`ssh_config`/`known_hosts` stay as they
   are); only the local, spawn-free checks of step 2 run. Otherwise list the repo tree in one
   call (`gh api repos/{repo}/git/trees/{commit}?recursive=1`), compare per-file shas with
   `cache/`, download only changed blobs, in parallel.
2. Apply locally:
   - authorized keys: rewrite the managed block (§7.3) from `devices/*.pub`;
   - `<state>/known_hosts` from `devices/*.host.pub`;
   - `<state>/ssh_config` from `devices/*.json` + current Tailscale IPs (`tailscale status --json`);
   - skills → `~/.config/opencode/skills/opencode-mesh/` (mesh-owned folder, replaced wholesale);
   - `AGENTS.md` → `~/.config/opencode/AGENTS.md` (§7.4).
3. If `mesh.json` `pin` differs from this binary's version, self-update to the pin (§5.9 step
   1; either direction, no repo writes), then run `<installed mesh> daemon --start`: the new
   build's daemon starts and the old one quits once its commands are done (§6.6). Leftover
   `*.old` binaries are deleted.

Before anything else (not for `--keys-only`), a sync makes sure this build's daemon answers
(else registers its autostart and starts it, §6.6).

Prints a short diff (`+ device air`, `~ skill review`, …). The daemon runs it every `interval`
(§6.6); also run lazily when a target names an unknown device. A remote `mesh exec <device>
mesh sync` runs in that device's daemon like any command, so `gh` is logged in there.

### 5.7 `mesh cp`

```
mesh cp [-r] <src> <dst>        # each side: local path or <device>:<path>
```

Local↔device uses `scp -F <state>/ssh_config`. Device→device runs on the source device:
`mesh exec <src> mesh cp [-r] <path> <dst>:<path>`.

```sh
mesh cp -r ./dist air:~/deploy/
mesh cp blade:C:/logs/out.log .
mesh cp blade:C:/build.zip air:~/
```

### 5.8 `mesh rm` / `mesh leave`

```
mesh rm <device> [--yes]     # any device; deletes devices/<device>.* in one commit; not self
mesh leave [--yes]           # this device
```

`leave`: delete own `devices/*` (commit), then undo all local setup — MCP registration
(`opencode api experimental.mcp.remove`), the daemon's autostart, the managed keys block, the
opt-in `Include` line, the marked PATH line in shell profiles (Linux/macOS),
`skills/opencode-mesh/`, the state dir (incl. key and token). Leaves
`~/.config/opencode/AGENTS.md`, an installed SSH server and the mesh binaries in place.

- `experimental.mcp.remove` only drops the running server, and the config API can't delete
  entries, so `leave` cuts its own `mesh` entry (only if its URL is this daemon's) out of the
  global config text, leaving every other byte as is, then removes it in every loaded location.
- Admin Windows: the keys block and the `\opencode-mesh\keys` task need one UAC prompt (the
  same elevated step as `up`). The daemons are told to quit (frame `7`) before the state dir is
  deleted.
- `rm` refuses this device (use `leave`); without a terminal both need `--yes`.

### 5.9 `mesh update`

```
mesh update [--version <x.y.z>] [--check]
```

One command updates the whole mesh:

1. Download the release asset for this OS/arch with `gh release download` from `Hona/mesh`,
   verify against the release's `SHA256SUMS`, replace self (Windows: rename the running
   `mesh.exe`/`meshw.exe` to `*.old`, write new, delete `*.old` on a later run).
2. Set `pin` in `mesh.json` and push (explicit). Push `skills/mesh/SKILL.md` if newer (§9).
3. `mesh exec all mesh update --version <v>` for online devices; offline ones converge on
   their next sync (they see `pin`). Then the local daemon closes its SSH links (frame `9`):
   they still run the devices' pre-update mesh, so the next command redials.
4. On each device the new build's daemon takes over (§5.6 step 3); MCP clients keep working:
   each MCP request is a separate HTTP POST (§8), so nothing in opencode needs reconnecting.

Devices never move to "latest" on their own — only to the pinned version.
`--version` also rolls back. `--check` shows what's available. Without `--version`, "latest"
is the highest release, counting prereleases only when this build is one.

- Step 1 downloads `mesh-<os>-<arch>[.exe]` (+ `meshw-…` on Windows) and `SHA256SUMS` into
  `<state>/update/`, then swaps them in at the §4.4 install path. Steps 2–4 run in the new
  binary (`mesh update --version <v> --resume`), so the skill it pushes is the new one.
- Run on a device by another device's step 3 (`MESH_FROM` is set), `update` runs a sync there:
  the caller already pushed the pin, so it converges to it (§5.6 step 3), and reports the new
  version. No repo writes, no further fan-out.
- Pin and skill go in one commit; nothing is committed when both are already current.
- No compatibility across breaking releases (§1.12): re-run `up` on each device instead.

### 5.10 `mesh daemon` / `mesh mcp` (hidden)

`mesh daemon`: §6.6 (`--start`: make sure this build's daemon runs, after a binary swap).
`mesh mcp`: the same MCP server on stdio (§8), for debugging; not registered anywhere.

## 6. Transport

### 6.1 SSH

Every mesh connection is `ssh -F <state>/ssh_config <device> …`. The user's `~/.ssh/config`
and `known_hosts` are never read or modified by mesh connections. Generated config per device:

```
Host blade
  HostName 100.87.251.43          # current Tailscale IP, resolved locally at sync (network backend)
  User lukem
  IdentityFile <state>/id_ed25519
  IdentitiesOnly yes
  UserKnownHostsFile <state>/known_hosts
  StrictHostKeyChecking yes
  BatchMode yes
  ConnectTimeout 5
  ServerAliveInterval 15
```

### 6.2 Handoff (how remote commands run without remote shell parsing)

Local `mesh exec` connects with SSH and runs the remote mesh with a hidden flag:

```
ssh -F <cfg> <device> "<device.mesh path>" exec self --envelope
```

The first stdin line is a JSON envelope; everything after it is passed to the child's stdin:

```jsonc
{ "v": 1, "from": "adais",
  "argv": ["opencode", "api", "session.active"],   // or "command": "<string>" (-c), "script" (-s)
  "cwd": "D:\\Hona\\app", "env": { "K": "V" }, "tty": false, "timeout": 120,
  "stdin": true,      // the client streams stdin (absent: the command's stdin is the null device)
  "stall": true }     // stall detection (absent = off)
```

- The remote side is just `mesh exec self`, a relay (§6.6): it hands the envelope to the
  daemon (or runs it itself, the fallback), which resolves `opencode`/`mesh`, sets
  `MESH_FROM`, enforces the timeout, and execs the argv directly (no shell) or the string via
  the device's shell.
- stdout/stderr go to SSH's own channels; the exit status is SSH's exit status.
- `-t` mode: `ssh -t … exec self --envelope <hex>`: the envelope travels hex-encoded as an
  argument (only `[0-9a-f]`, safe in every shell) because stdin is the terminal.

### 6.3 Remote shell quoting

Never build shell strings from user argv. The only string that crosses a remote shell is the
absolute mesh path + fixed flags above (+ the hex envelope for `-t`). Windows: the path is
unquoted when it has no spaces, so it runs under any sshd DefaultShell (cmd, powershell, pwsh);
with spaces it's double-quoted (cmd, the default). Unix: single-quoted.

### 6.4 Finding opencode on a device (at run time, on that device)

1. `~/.local/state/opencode/service.json` (respect `XDG_STATE_HOME`) → `pid` → that process's
   executable path (the one persistent service; e.g.
   `%APPDATA%\ai.opencode.desktop\cli\2.0.32-x64\opencode-cli.exe serve --service`). Every
   opencode CLI is a client of that service, and another version can replace it with an
   incompatible one (seen on Blade: PATH's 2.0.28 `service restart` replaced the desktop's
   beta service with one answering 500), so the service's own build comes first.
2. The last service executable seen by step 1 (`<state>/opencode-service`), so a down or stale
   service is started again with the same build.
3. `opencode` on PATH.
4. Known install locations: the desktop apps' CLIs, stable and beta (`%APPDATA%\ai.opencode.desktop\cli\<v>\`,
   `%APPDATA%\ai.opencode.desktop.beta\cli\<v>\`, the bundled
   `%LOCALAPPDATA%\Programs\@opencodedesktop\resources\opencode-cli.exe`; the same app ids
   under `~/Library/Application Support/` or `~/.local/share/`, and `/Applications/OpenCode
   [Beta].app` on macOS), taking the most recently installed one (beta versions
   `0.0.0-beta-N` sort below stable ones), then `~/.opencode/bin/opencode` and the usual bin
   dirs.

Only `version`, `url`, `pid` are read from `service.json` — never `password`.

### 6.5 Persistent connections (the mux, inside the daemon)

- The daemon (§6.6) keeps one SSH connection per device ("link"), dialed lazily, closed after
  10 min without commands or output (link pings don't count).
- Each link runs the remote mesh's relay, `mesh exec self --serve`, which handles many
  concurrent commands over the one SSH channel.
- CLI invocations talk to the local daemon over its socket; if it doesn't answer they start it
  (§6.6) and wait up to 3 s, else fall back to a direct one-shot `ssh` (the §6.2 handoff). The
  daemon's own MCP calls go through its socket too.
- Goal: after the first command, each exec costs one network round trip + the command's own
  startup. No SSH handshake and no remote shell startup per command (Windows SSH runs every
  command through the default shell; PowerShell costs 0.3–0.8 s).
- `-t` interactive sessions keep using plain `ssh -t` (and run in the SSH session).

Details:

- Links: offline devices fail at once; no hello within 15 s = failed.
- Frame: `[kind u8][id u32 LE][len u32 LE][len bytes]`. Kinds: `0 hello` (JSON, first frame
  each way the server speaks), `1 start` (envelope JSON; one with `device` goes to that
  device's link, one without runs on this device), `2 stdin` (empty = EOF), `3 stdout`,
  `4 stderr`, `5 exit` (JSON `{code, error?, via?}`), `6 cancel` (kill the tree), `7 quit`
  (client → daemon: quit once idle), `8 ping` (echoed), `9 reset` (client → daemon: close the
  links). Stream chunks ≤ 64 KiB. The mux rewrites ids between clients and links; a client
  that disconnects cancels its commands.
- Dead links: a link quiet for 20 s is pinged before reuse (one round trip; no answer in 3 s →
  redial transparently). The mux also pings quiet links and kills one that doesn't answer
  within 10 s; its in-flight commands end with exit 255 `connection to <device> lost`.

### 6.6 The daemon

One long-lived mesh process per user and device, `mesh daemon` (hidden), in the user's own
login session. It is the only mesh process on a device (plus commands' transient children):

```
caller ──ssh──▶ sshd ──▶ mesh exec self --serve   (relay, inside the SSH session)
                                 │ local socket, this user only
                                 ▼
                           mesh daemon  (user's login session)
                                 ├─▶ starts the commands
                                 ├─ mux: SSH links to other devices (§6.5)
                                 ├─ MCP server, http://127.0.0.1:<port>/mcp (§8)
                                 └─ sync every `interval` (a `mesh sync` child; first after 1 min)
```

- **Socket**: per build, Windows pipe `\\.\pipe\opencode-mesh-<user SID>-daemon-<build id>`
  (DACL = that user only, remote clients rejected), unix `<state>/daemon-<build id>.sock`
  (state dir 0700). Hello `{"v":1,"mesh":…,"build":…}`.
- **Relay**: `exec self --serve` connects to its build's daemon and hands it each START with the
  SSH session's environment (`relay_env`, the command's base environment) and no `device`;
  stdin, cancel, output and exit pass through. `--envelope` (one-shot) does the same for its one
  command. `-t` runs in the SSH session. If the daemon doesn't answer, the relay starts it
  (registering its autostart if missing, ≤ 3 s; after a failed start, again only after a
  minute) and otherwise runs the command itself in the SSH session (`via: "ssh"`). The daemon
  going away mid-command ends that command with exit 255 `the daemon went away`.
- **Commands**: exactly as §5.5 (overlapped pipes on Windows, job object / process group,
  timeout, stall, `NO_COLOR`/`TERM=dumb`, `MESH_FROM`, scripts, `opencode`/`mesh` resolution;
  `mesh` = the console build next to the daemon, `PATH` lookups use the relayed PATH). The tree
  is killed only on timeout, cancel, stall or the relay's connection closing mid-command.
- **Windows console**: the autostart runs the windowless `meshw.exe daemon`, which starts
  `mesh.exe daemon` with `CREATE_NO_WINDOW` (a console without a window; out of the task's job
  when allowed; output to `log/daemon.log`) and exits. Commands inherit that console instead of
  each getting a new conhost: spawning `hostname` measured 9.8 vs 19.7 ms in the user's
  session.
- **One per user**: a daemon that can't bind its socket (this build already runs) exits. A
  starting daemon asks every other build's daemon to quit (frame `7`); a daemon told to quit
  closes its MCP listener at once (the port is free for its successor) and exits as soon as no
  command runs (it still serves its existing connections until then).
- **MCP listener**: bound when `state.json` has a port and `<state>/mcp-token` exists, else (or
  while the port is taken) retried every 2 s. A request that arrives while no daemon listens
  fails; the next one after it's back works (verified: opencode needs no reconnect).
- **Taking over after a new binary** (`daemon --start`, `update`, `up`, a relay or sync that
  finds no daemon of its build): register the autostart, then Windows: run the task (a new
  instance; it retires the old); Linux/macOS: ask the old daemon to quit and start the unit
  (systemd/launchd starts the new binary once the old one exits).
- **Idle cost**: blocked threads, a 10 s link-watch tick, the sync timer. Measured idle private
  memory: see §12.

## 7. Setup details

### 7.1 SSH server and OS permissions (inside `mesh up`, once)

Adapt to what exists; never reconfigure an existing server.

| OS | If SSH server missing | Always |
|---|---|---|
| Windows | One UAC: install OpenSSH Server capability, start it, set Automatic | Pick the `exec -c` shell (§4.2; sshd's DefaultShell is left untouched); detect effective keys file (parse `sshd_config` incl. `Match Group administrators`; membership counts even when not elevated); the one UAC also creates the admin keys file (Administrators + SYSTEM only, no inheritance) and the `keys` task (§7.3) when needed |
| macOS | Open Sharing settings; user turns on **Remote Login** | Also require **"Allow full disk access for remote users"** (otherwise remote commands can't read `~/Documents`, `~/Desktop`); open the exact pane, wait, re-verify via SSH to self |
| Linux | One sudo: install via detected package manager | `loginctl enable-linger $USER` (the user service survives logout); detect login shell |

The device's `.host.pub` is read from the server's `ssh_host_ed25519_key.pub`; where that needs
admin to read (Windows), `up` asks the running server instead (`ssh-keyscan 127.0.0.1`).
The one elevated step on Windows is `meshw elevated <hex plan>` via a single UAC prompt; if it's
declined or unanswered `up` exits 3 and prints the exact command to run in an admin terminal.

### 7.2 Scheduling: the daemon's autostart

The daemon (§6.6) starts at login and is kept running, as the user, no admin. Its own timer
runs the periodic sync.

| OS | Mechanism |
|---|---|
| Linux | systemd user service `opencode-mesh.service`: `ExecStart=<mesh> daemon`, `Restart=always`, `RestartSec=1`, `KillMode=process` (stopping the daemon never kills what its commands started), `WantedBy=default.target` (linger keeps it running without a login) |
| macOS | LaunchAgent `opencode-mesh.daemon`: `RunAtLoad`, `KeepAlive` |
| Windows | Task Scheduler `\opencode-mesh\daemon`, "run only when user is logged on": at this user's logon and every `interval` (a running daemon makes that instance exit at once), parallel instances, no time limit, priority 5 (normal: commands inherit it), action `meshw.exe daemon` |

`meshw.exe` is the windowless (`windows_subsystem = "windows"`) build of the same code — the
`pythonw.exe` / `javaw.exe` convention. Logs go to `<state>/log/`.

### 7.3 Authorized keys

- Write a marked block into the file the SSH server actually uses for this user (admin
  Windows default: `C:\ProgramData\ssh\administrators_authorized_keys`; otherwise
  `~/.ssh/authorized_keys`; or a custom `AuthorizedKeysFile`). Keep every other line and the
  file's permissions/ACL exactly.

```
# >>> opencode-mesh (managed, do not edit) >>>
ssh-ed25519 AAAA… opencode-mesh:blade
ssh-ed25519 AAAA… opencode-mesh:air
# <<< opencode-mesh <<<
```

- If that file needs admin to write (Windows admin case), the one UAC in `up` also creates
  `\opencode-mesh\keys`: a scheduled task with highest privileges that runs
  `meshw.exe sync --keys-only` (rewrites only the block from the local cache) on the same
  interval. Silent afterwards. This is the only elevated part of mesh.

### 7.4 AGENTS.md and skills (no drift)

- `~/.config/opencode/AGENTS.md` is mesh-owned: sync makes it match the mesh's `AGENTS.md`.
  If it was edited locally since the last sync, the local version is first copied to
  `backup/` and `status` warns. (Pushing edits is the user's job.)
- Mesh skills install to `~/.config/opencode/skills/opencode-mesh/<id>/` — mesh-owned,
  replaced wholesale; the user's other skills are untouched.
- **Creating a mesh:** this device's `AGENTS.md` (if any) becomes the mesh's first version,
  after confirmation.
- **Joining:** if local `AGENTS.md` differs from the mesh's, show the diff and ask:
  `[k]eep mesh` (back up local, replace) / `[u]pload this` (replaces it for every device;
  pushed) / `[e]dit` (merge by hand, then keep or upload). Local skills not in the mesh:
  upload / keep local / delete. Same-ID skills in both: `[k]eep mesh` (back up local) /
  `[u]pload this`.
- `--yes` defaults to `--prefer mesh`, which (like `--prefer local`) decides only `AGENTS.md`.
- Skills are never bulk-uploaded: each upload is its own answer, or `--upload-skill <id>`.
  Non-interactive without it, local-only skills stay local and same-ID ones keep the mesh's.
- The first sync on a device names its `AGENTS.md` backup as the previous file, not as an
  edit "since the last sync".

## 8. MCP server

- Server name `mesh`, served by the daemon (§6.6), JSON-RPC 2.0, hand-rolled. One server per
  device for every opencode location (no process per location).
- **Transport**: Streamable HTTP, minimal: `POST http://127.0.0.1:<port>/mcp` with one JSON-RPC
  message → `200 application/json` with the response (`202`, empty, for notifications). No SSE:
  `GET` → `405`. Stateless (no `Mcp-Session-Id`): a daemon restart is invisible to clients
  between requests, so updates need no reconnect.
- **Security** (local server): bound to `127.0.0.1` only; a request with an `Origin` other than
  absent/`null` → `403`; `Host` other than `127.0.0.1:<port>` / `localhost:<port>` → `403`
  (DNS rebinding); `Content-Type` not `application/json` → `415`; `Authorization` other than
  `Bearer <token>` → `401`, compared in constant time. Token: `<state>/mcp-token`, 32 random
  bytes (OS RNG) as hex, readable by the user only (unix 0600; Windows inheritance cut, the
  user only). opencode sends it from the config entry `"Bearer {file:<token path>}"` (§5.3
  step 9). Limits: 64 KiB of headers, 16 MiB body.
- `mesh mcp` (hidden) serves the same on stdio (newline-delimited) for debugging.
- Methods: `initialize`, `notifications/initialized`, `ping`, `tools/list`, `tools/call`.
- Tool results: `content` (readable text for the model) + `structuredContent` (same JSON as
  the CLI's `--json`). `isError` only for mesh-level errors (bad arguments); per-device
  failures live in `results`.
- `instructions` (one line, from the local cache, plus the console build's absolute path, which
  works before a new PATH reaches opencode's shells):
  `Opencode mesh: devices adais (windows), blade (windows), air (macos). Load the "mesh" skill before using mesh tools. CLI: C:\Users\Lukem\AppData\Local\Programs\mesh\mesh.exe`
- The built-in skill says: if `mesh` isn't on PATH, use that CLI path.

### 8.1 `status`

```jsonc
// input
{ "target": { "type": "string", "default": "all" },
  "fast":   { "type": "boolean", "default": false } }
// output
{ "self": "adais",
  "devices": [
    { "name": "blade", "self": false, "os": "windows", "arch": "x64", "shell": "powershell",
      "user": "lukem", "online": true, "reachable": true, "latency_ms": 38,
      "opencode": { "version": "2.0.32", "busy": 1, "service": "ok" }, "daemon": "up",
      "mesh": "0.1.0" } ] }
// "service": "ok" | "down" | "stale" | "hung" (§5.4)
```

### 8.2 `exec`

```jsonc
// input — exactly one of argv | script
{ "target":    { "type": "string" },                                 // required
  "argv":      { "type": "array", "items": { "type": "string" } },   // one program, exact args, no shell; fastest
  "script":    { "type": "string" },                                 // device's shell code, any length (-s); not with stdin
  "stdin":     { "type": "string" },                                 // absent: stdin is the null device
  "stall":     { "type": "boolean", "default": true },               // §5.5
  "cwd":       { "type": "string" },
  "env":       { "type": "object", "additionalProperties": { "type": "string" } },
  "timeout":   { "type": "number", "default": 120 },                 // seconds
  "max_bytes": { "type": "number", "default": 50000 } }              // per stream, keeps head + tail
// output: §5.5 JSON result
```

The calling session, `params._meta["ai.opencode/sessionID"]`, reaches the remote command as
`MESH_SESSION` (unless `env` sets it), next to `MESH_FROM`, to trace work back to a session.
The field descriptions in `tools/list` explain argv vs script on their own.

Text rendering for the model:

```
── blade · exit 0 · 1.2s
…stdout…
── air · offline
```

## 9. Built-in skill

- Source: `skill/SKILL.md` in this repo, embedded into the binary.
- Describes the interface plainly and shows examples; no prescriptive rules. Under ~60 lines:
  the opening (any device, remote opencode always works, the `opencode` skill for opencode
  usage); a reference (`status`, `exec` argv / `-c` / `-s` and flags, `cp`, targets, exit codes
  incl. 124 and 255); examples per OS family, CLI and MCP; recipes (recovery: service states,
  start/stop/kill, log, `opencode upgrade`; setting up a machine outside the mesh through a
  device that reaches it, `argv: ["ssh", …, "bash", "-s"]` + `stdin`; handing a task to another
  device's `opencode run`); notes (offline devices, `MESH_FROM`, the CLI path).
- Lives in the mesh repo at `skills/mesh/SKILL.md` and reaches devices through normal sync.
- Written to the repo only by `mesh up` (when creating a mesh) and `mesh update`.
- When writing, the binary adds frontmatter `metadata.mesh/version` (its version) and
  `metadata.mesh/hash` (hash of the body it wrote).
- Guards: never write if the repo copy's `mesh/version` is newer (no downgrade); never write if
  the repo copy's body no longer matches its `mesh/hash` (user customised it — leave it,
  warn in `status`).

## 10. Distribution

- Release tag `v<semver>`. Assets: `mesh-linux-x64`, `mesh-linux-arm64` (musl, static),
  `mesh-macos-x64`, `mesh-macos-arm64`, `mesh-windows-x64.exe`, `meshw-windows-x64.exe`,
  `mesh-windows-arm64.exe`, `meshw-windows-arm64.exe`, `SHA256SUMS`.
- Release profile: `opt-level = "z"`, `lto = true`, `codegen-units = 1`, `panic = "abort"`,
  `strip = true`. Targets: < ~1 MB binary, ~1–2 MB idle RSS for the daemon.
- CI (`release.yml`): on a `v*` tag, fail unless it matches the `Cargo.toml` version, build
  all targets (Linux musl via cargo-zigbuild, both macOS on one runner, both Windows MSVC),
  write `SHA256SUMS`, publish the release (prerelease when the tag has a `-`; if a release for
  the tag already exists, its assets are replaced with `--clobber`). **No test steps.**
  Releases come only from CI: bump `Cargo.toml`, push the tag, wait for the run
  (`gh run watch`), then `mesh update`. The pin always names a CI-built release.
- Install, macOS/Linux:
  `gh api repos/Hona/mesh/contents/install.sh -H "Accept: application/vnd.github.raw" | sh`
- Install, Windows:
  `gh api repos/Hona/mesh/contents/install.ps1 -H "Accept: application/vnd.github.raw" | Out-String | iex`
- Both download via `gh release download` (latest, or `MESH_VERSION=<x.y.z>`), check
  `SHA256SUMS`, then run `mesh up` from the download folder; `up` installs to the paths in §4.4
  (rename-aside if in use), puts it on PATH (user PATH / shell profile line), starts the daemon,
  registers MCP, and restarts the opencode service when safe (§5.3 step 12). Arguments for `up`:
  `| sh -s -- <args>` / `$env:MESH_UP_ARGS`.
- Copy-paste prompt (any OS):
  > Install opencode mesh on this device: use `gh release download` to get the `mesh` binary
  > for this OS and CPU from the latest `Hona/mesh` release, put it in a user bin folder on
  > PATH, then run `mesh up --yes`. If it needs admin or sudo, give me the exact command.

## 11. Phases

| Phase | Deliverable | Manual check |
|---|---|---|
| 0 | Spike: key + handoff between adais and blade by hand | `mesh exec blade opencode --version` from adais |
| 1 | `sync`, `status` | repo pulled, ssh files generated, status table right |
| 2 | `up` (create + join, setup steps, conflicts, MCP, schedule) | fresh join of adais and blade |
| 3 | `exec` (full, over persistent connections §6.5), `cp`, `mcp`, skill | agent on adais drives blade via `mesh_exec` |
| 4 | `update`, `rm`, `leave`, release CI, installers | mesh-wide update from one command |

## 12. Open items (verify manually during phases)

- Closed by the daemon (§6.6): an opencode service (or anything) started by `mesh exec`
  survives the SSH session; `gh` and other credential-store logins work in remote commands.
- Commands now run with the user's normal (on Windows admins: filtered, non-elevated) token,
  as the user's own programs do; before, an admin's SSH session ran them elevated.
- macOS: the daemon LaunchAgent and the `launchctl` takeover are untested (air offline); no
  stall detection there.
- Can `opencode api --data` / `opencode session import` read stdin? If not, large payloads go
  via `mesh cp` + a path.
- Windows admin keys file + elevated `keys` task end to end: verified on adais (one UAC; the
  task runs from a non-elevated `schtasks /run`). Still to see: a second device's key arriving.
- Windows sshd drops the exit status in pty mode (plain `ssh -t` too), so `exec -t` to a
  Windows device always exits 0. Non-`-t` exec is unaffected.
- **Open decision: the elevated `keys` task.** It runs `meshw.exe` from the user-writable
  install dir (and reads the user-writable cache), so code running as the user could swap the
  binary or the cache and gain admin without a UAC prompt. Options: accept (status quo), or have
  the task run a copy in `%ProgramFiles%\opencode-mesh\` that only elevated steps refresh, at the
  cost of one UAC per `mesh update` on admin devices.
- macOS Remote Login + full-disk-access flow (needs air online).

## 13. Out of scope for v1

`sources` · pushing local edits automatically · OpenCode Console as registry · TUI plugin ·
loop guard beyond `MESH_FROM` + skill guidance · non-gh fallback · any automated tests.
