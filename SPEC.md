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
    arguments, same JSON as `--json`.

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
                  #   "synced": "<commit sha>" }
sync.lock         # held during a sync (pid inside; taken over if that process is gone)
sync.result       # how the last sync ended (report or error), for a remote `mesh sync` (§5.6)
run/              # Windows: durable commands queued for the run task, their logs (§5.5)
mux-<build>.sock  # Linux/macOS: the mux's local socket, one per build (dir is 0700)
id_ed25519(.pub)  # this device's mesh key (permissions locked to the user)
ssh_config        # generated; used via `ssh -F` for every mesh connection
known_hosts       # generated from devices/*.host.pub
cache/            # last pulled copy of the mesh repo + per-file sha index
backup/<time>/    # backups of anything mesh replaced (AGENTS.md, conflicting skills)
log/              # background run logs
```

Installed binary: `~/.local/bin/mesh` (Linux/macOS; if that isn't on PATH, one marked line in the
`$SHELL` profile adds it, §5.3); `%LOCALAPPDATA%\Programs\mesh\{mesh,meshw}.exe` (Windows, added
to user PATH).
Everything internal (MCP registration, remote handoff, scheduled tasks) uses absolute paths.

### 4.5 Architecture (`src/`)

One package `mesh`: a library plus thin binaries.

```
lib.rs          module declarations only
main.rs         bin `mesh`: parse args → app → exit code
bin/meshw.rs    bin `meshw`, Windows windowless build (Phase 2)
cli/            argument parsing, help, text + JSON rendering
app/            commands (sync, status, up, exec, …): orchestration only
model/          plain data: Mesh, Device, Target, State, reports
registry/       where the mesh lives            → github.rs (the only place that runs `gh`)
network/        how devices are found / online  → tailscale.rs (the only place that runs `tailscale`)
transport/      how commands reach devices      → ssh.rs; mux.rs, frame.rs (§6.5, Phase 3)
host/           this machine's OS integration   → paths, processes, admin check, opencode discovery
mcp/            stdio MCP server (§8, Phase 3)
```

- `cli` → `app` → `registry`, `network`, `transport`, `host`, `model`.
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
Internal   mcp
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
| `--skip` | skip steps: `sshd`, `mcp`, `schedule` |

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
7. **Sync** — §5.6.
8. **MCP** — `opencode mcp add mesh --global -- <abs mesh path> mcp`. If a different `mesh`
   MCP entry exists, ask. If `up` just replaced the installed binary, restart the running `mesh`
   MCP servers so none keeps old code: `opencode api experimental.mcp.disconnect` / `connect`
   (`server=mesh`) in every loaded location (`debug.location.list`; if that fails:
   `! mcp  restart opencode to load the new mesh binary`).
9. **Schedule** — §7.2.
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
    service, its agents' shells and the MCP servers it starts keep the old PATH, so `up` runs
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
  ✓ mcp        registered "mesh"
  ✓ schedule   sync every 10m
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
session.active`, only asked of a service that answers), mesh version. `--fast` uses only the
repo cache + Tailscale.

```
DEVICE   OS       SHELL       ONLINE  OPENCODE  BUSY  MESH
adais *  windows  pwsh        yes     2.0.32    0     0.1.0
blade    windows  powershell  yes     2.0.32    1     0.1.0
air      macos    zsh         no      -         -     -
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
Older devices send `service: true|false` (read as `ok` / `down`).

A running desktop app owns its service and starts it again by itself (verified on Blade): within
~1 s after `opencode service stop`, a little later after a kill; `opencode service start` then
adopts whichever registers first. `hung` can show once while a busy service catches up (seen
for a few seconds right after MCP restarts).

### 5.5 `mesh exec`

Run a command on devices.

```
mesh exec [-t] [-w <dir>] [-e KEY=VAL]... [--timeout <s>] [--durable] [--no-stall] [--json] <target> <cmd> [args…]
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
- `--durable` (MCP `durable: true`): what the command starts must also outlive the SSH
  session (mux idle exit, network drop). Windows sshd kills its session job, and a breakaway
  lands in an outer job it kills too, so the command is queued in `<state>/run/` and run by the
  user-context `\opencode-mesh\run` task (no trigger, parallel instances, created on first use),
  in the logged-on user's session; its output (stdout and stderr merged, no stdin) is relayed
  live and the exit code returned. Same kill rule: timeout, cancel or the serving process gone.
  Nobody logged on: fails with `not logged in …` (nothing ran). Linux: wrapped in
  `systemd-run --user --scope` when the user bus is up (out of the login session logind may
  reap; linger keeps the user manager; verified on the Debian VPS: the process lands in
  `user@0.service/app.slice/run-*.scope`). macOS: runs as is. `opencode service start|restart`
  is always durable.
- **Stall**: a command with no output whose process tree has used < 1% CPU and done no I/O for
  20 s (sampled every 5 s: job accounting on Windows, the process group's `/proc` stat + io on
  Linux; macOS: not detected) is killed: exit 124, `error: "stalled"`, `hint: "no output and
  idle for 20 s; is it waiting for input?"`, the output so far; the CLI prints `mesh: <device>:
  stalled: <hint>`. Not while stdin is still streaming in, not with `-t`. One timer per link (a
  thread per one-shot / durable command). Opt out: `--no-stall` (MCP `stall: false`).
  Internal execs (`status`, `update`, `cp`, `up`) don't ask for it.
- **Plain output**: non-`-t` commands get `NO_COLOR=1` and `TERM=dumb` (before `-e`; PowerShell
  7.2+ then renders `PlainText`). `--json` / MCP results drop ANSI escape sequences and turn
  CRLF into LF; the live CLI stream is raw.
- Windows: the serving side gives a command's stdio named pipes opened for overlapped I/O (like
  sshd's own), not std's synchronous anonymous pipes: with those, Win32-OpenSSH's `ssh.exe`
  inside an SSH session prints nothing while its stdin is open and never exits with a stdout
  pipe (`mesh exec blade ssh vps uname` hung until the timeout; now 0.6 s).
- A target naming a device missing from the cache runs one sync first (§5.1).
- The remote process gets `MESH_FROM=<calling device>`.
- Nested (`mesh exec` run inside a `mesh exec` on a Windows device): no mux, one-shot SSH per
  device with the envelope hex-encoded in argv, stdin null, and stdout/stderr to files in
  `<state>/run/` that are tailed live. ssh.exe inside an SSH session misbehaves with pipes
  (stdin pipe: relays output only while it has data; stdout pipe: never exits), so a mux
  started there stalls. The nested command gets no stdin.
- For long-lived background commands: `--durable` with the command's own way to background
  (`Start-Process`, `nohup … &`), or opencode's shells (`opencode api shell.create …`).

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
    { "device": "blade", "exit": 0, "ms": 1210, "stdout": "…", "stderr": "", "truncated": false },
    { "device": "vps", "exit": 124, "ms": 20210, "stdout": "", "stderr": "", "truncated": false,
      "error": "stalled", "hint": "no output and idle for 20 s; is it waiting for input?" },
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
3. If `mesh.json` `pin` differs from this binary's version, self-update to the pin (§5.9 step 1
   + MCP restart; either direction, no repo writes). Leftover `*.old` binaries are deleted.

Prints a short diff (`+ device air`, `~ skill review`, …). Runs on schedule and lazily when a
target names an unknown device.

Inside `mesh exec` (an SSH session, `MESH_FROM` set) the OS credential store, and so `gh`'s
login, is out of reach on Windows and macOS (Linux: no secret service). There, `sync` starts the
device's own sync job (`\opencode-mesh\sync` task, `launchctl kickstart`, `systemctl --user
start`), waits for its `sync.result` (up to 5 min) and reports that. A job that doesn't start
within 20 s means the user isn't logged on (Windows runs it only then): it fails with
`<device>: not logged in …; it syncs on its next run`. `--force` doesn't carry over.

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
(`opencode api experimental.mcp.remove`), scheduled tasks, the managed keys block, the
opt-in `Include` line, the marked PATH line in shell profiles (Linux/macOS),
`skills/opencode-mesh/`, the state dir (incl. key). Leaves
`~/.config/opencode/AGENTS.md`, an installed SSH server and the mesh binaries in place.

- `experimental.mcp.remove` only drops the running server, and the config API can't delete
  entries, so `leave` cuts its own `mesh` entry (only if its command is mesh's) out of the
  global config text, leaving every other byte as is, then removes it in every loaded location.
- Admin Windows: the keys block and the `\opencode-mesh\keys` task need one UAC prompt (the
  same elevated step as `up`). This build's mux is told to quit (frame `7`) before the state
  dir is deleted.
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
   their next sync (they see `pin`). Then the local mux is told to quit: its links still run
   the devices' pre-update mesh, so the next command redials.
4. On each device, reconnect the MCP server so the new binary is used
   (`opencode api experimental.mcp.disconnect` / `connect` for `mesh`, in every loaded
   location, as in §5.3 step 8).

Devices never move to "latest" on their own — only to the pinned version.
`--version` also rolls back. `--check` shows what's available. Without `--version`, "latest"
is the highest release, counting prereleases only when this build is one.

- Step 1 downloads `mesh-<os>-<arch>[.exe]` (+ `meshw-…` on Windows) and `SHA256SUMS` into
  `<state>/update/`, then swaps them in at the §4.4 install path. Steps 2–4 run in the new
  binary (`mesh update --version <v> --resume`), so the skill it pushes is the new one.
- Run on a device by another device's step 3 (`MESH_FROM` is set), `update` downloads nothing
  itself (`gh` can't log in there, §5.6): the caller already pushed the pin, so it runs the
  device's own sync job, which converges to it (§5.6 step 3, incl. the MCP restart), and
  reports the new version. No repo writes, no further fan-out. Devices on 0.1.0-dev.5 or older
  don't have this and need their own sync job to run once (≤ interval).
- Pin and skill go in one commit; nothing is committed when both are already current.
`mesh update` and the exec handoff stay backward compatible, so a new binary can always tell
an old one to update.

### 5.10 `mesh mcp`

Stdio MCP server (§8). Started by opencode.

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
  "stdin": false,     // nothing streams: the command's stdin is the null device (absent = true)
  "stall": true }     // stall detection (absent = off)
```

- The remote side is just `mesh exec self`: it resolves `opencode`/`mesh`, sets `MESH_FROM`,
  enforces the timeout, and execs the argv directly (no shell) or the string via the device's
  shell.
- stdout/stderr ride SSH's own channels; the exit status is SSH's exit status. No framing.
- Unknown envelope fields are ignored; a higher major `v` fails with an upgrade message.
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

### 6.5 Persistent connections (Phase 3)

- A local background `mesh` process (the "mux") starts on first use, keeps one SSH connection
  per device, and exits after ~10 min idle.
- Each connection runs the remote mesh in a long-lived serve mode that handles many concurrent
  commands over the one SSH channel.
- Binary length-prefixed frames per command id: `start` (envelope), `stdin`, `stdout`,
  `stderr`, `exit`, `cancel`.
- CLI invocations and the MCP server talk to the local mux over a local socket (named pipe on
  Windows, unix socket elsewhere).
- Goal: after the first command, each exec costs one network round trip + the command's own
  startup. No SSH handshake and no remote shell startup per command (Windows SSH runs every
  command through the default shell; PowerShell costs 0.3–0.8 s).
- `-t` interactive sessions keep using plain `ssh -t`. If the mux can't start, fall back to a
  direct one-shot `ssh` (the §6.2 handoff).

Details:

- The mux is the hidden `mesh mux`, spawned detached by the first client (no console, its own
  process group, out of the caller's job when allowed; logs to `log/mux.log`). One mux per
  build: Windows pipe `\\.\pipe\opencode-mesh-<user SID>-<build id>`, DACL = that user only,
  remote clients rejected; unix `<state>/mux-<build id>.sock`. Different builds never share a
  mux. A mux exits after 10 min without clients and without command output (link pings don't
  count); when it exits, its SSH links close and the remote `exec self --serve` processes exit
  on EOF. A starting mux, and `up`/`update` after installing, ask this user's muxes from
  *older* builds to quit (frame `7`; endpoints found by name prefix, incl. the pre-per-build
  name); newer builds' muxes are never touched, so a stale old client can't knock out a new one.
- The remote side is the hidden `mesh exec self --serve`; links are dialed lazily per device
  (offline devices fail at once; no hello within 15 s = failed).
- Frame: `[kind u8][id u32 LE][len u32 LE][len bytes]`. Kinds: `0 hello` (JSON, first frame
  each way the server speaks), `1 start` (envelope JSON; to the mux it also carries `device`),
  `2 stdin` (empty = EOF), `3 stdout`, `4 stderr`, `5 exit` (JSON `{code, error?}`), `6 cancel`
  (kill the tree), `7 quit` (client → mux: `leave`, and newer builds retiring older muxes),
  `8 ping` (echoed). Stream chunks ≤ 64 KiB. The mux
  rewrites ids between clients and links; a client that disconnects cancels its commands.
- Dead links: a link quiet for 20 s is pinged before reuse (one round trip; no answer in 3 s →
  redial transparently). The mux also pings quiet links and kills one that doesn't answer
  within 10 s; its in-flight commands end with exit 255 `connection to <device> lost`.

## 7. Setup details

### 7.1 SSH server and OS permissions (inside `mesh up`, once)

Adapt to what exists; never reconfigure an existing server.

| OS | If SSH server missing | Always |
|---|---|---|
| Windows | One UAC: install OpenSSH Server capability, start it, set Automatic | Pick the `exec -c` shell (§4.2; sshd's DefaultShell is left untouched); detect effective keys file (parse `sshd_config` incl. `Match Group administrators`; membership counts even when not elevated); the one UAC also creates the admin keys file (Administrators + SYSTEM only, no inheritance) and the `keys` task (§7.3) when needed |
| macOS | Open Sharing settings; user turns on **Remote Login** | Also require **"Allow full disk access for remote users"** (otherwise remote commands can't read `~/Documents`, `~/Desktop`); open the exact pane, wait, re-verify via SSH to self |
| Linux | One sudo: install via detected package manager | `loginctl enable-linger $USER` (user timers survive logout); detect login shell |

The device's `.host.pub` is read from the server's `ssh_host_ed25519_key.pub`; where that needs
admin to read (Windows), `up` asks the running server instead (`ssh-keyscan 127.0.0.1`).
The one elevated step on Windows is `meshw elevated <hex plan>` via a single UAC prompt; if it's
declined or unanswered `up` exits 3 and prints the exact command to run in an admin terminal.

### 7.2 Scheduling

Runs `mesh sync` every `interval`, as the user, no admin:

| OS | Mechanism |
|---|---|
| Linux | systemd user timer `opencode-mesh-sync.timer` |
| macOS | LaunchAgent `opencode-mesh.sync` |
| Windows | Task Scheduler `\opencode-mesh\sync`, "run only when user is logged on", action `meshw.exe sync` |

`meshw.exe` is the windowless (`windows_subsystem = "windows"`) build of the same code — the
`pythonw.exe` / `javaw.exe` convention. Background runs log to `<state>/log/`.

Windows also has `\opencode-mesh\run` (same principal, no trigger, parallel instances, no time
limit, action `meshw.exe exec self --run-queued`) for durable commands (§5.5); created on first
use, removed by `leave`.

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

- Server name `mesh`; stdio; newline-delimited JSON-RPC 2.0; hand-rolled.
- Methods: `initialize`, `notifications/initialized`, `ping`, `tools/list`, `tools/call`.
- Tool results: `content` (readable text for the model) + `structuredContent` (same JSON as
  the CLI's `--json`). `isError` only for mesh-level errors (bad arguments); per-device
  failures live in `results`.
- `instructions` (one line, from the local cache, plus the server's own absolute path, which
  works before a new PATH reaches opencode's shells; `up` restarts the server, so it's there at
  once):
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
      "opencode": { "version": "2.0.32", "busy": 1, "service": "ok" }, "mesh": "0.1.0" } ] }
// "service": "ok" | "down" | "stale" | "hung" (§5.4)
```

### 8.2 `exec`

```jsonc
// input — exactly one of argv | command | script
{ "target":    { "type": "string" },                                 // required
  "argv":      { "type": "array", "items": { "type": "string" } },   // no shell
  "command":   { "type": "string" },                                 // device's shell (-c)
  "script":    { "type": "string" },                                 // device's shell (-s); not with stdin
  "stdin":     { "type": "string" },                                 // absent: stdin is the null device
  "stall":     { "type": "boolean", "default": true },               // §5.5
  "cwd":       { "type": "string" },
  "env":       { "type": "object", "additionalProperties": { "type": "string" } },
  "timeout":   { "type": "number", "default": 120 },                 // seconds
  "max_bytes": { "type": "number", "default": 50000 },               // per stream, keeps head + tail
  "durable":   { "type": "boolean", "default": false } }             // §5.5
// output: §5.5 JSON result
```

Text rendering for the model:

```
── blade · exit 0 · 1.2s
…stdout…
── air · offline
```

## 9. Built-in skill

- Source: `skill/SKILL.md` in this repo, embedded into the binary.
- Besides the reference, it carries a short **Recovery** section (≤10 lines): the service
  states (§5.4), then the exact `mesh exec` lines for start/restart (durable automatically),
  a hung service (`opencode service stop` with `--timeout`, else kill the `service.json`
  pid), the log (`~/.local/share/opencode/log/opencode.log`) and `opencode upgrade
  [<version>]` (rollback); run from another device, since `mesh` doesn't need opencode.
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
  `strip = true`. Targets: < ~1 MB binary, ~1–2 MB idle RSS for `mesh mcp`.
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
  (rename-aside if in use), puts it on PATH (user PATH / shell profile line), restarts the MCP
  server, and restarts the opencode service when safe (§5.3 step 12). Arguments for `up`:
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

- Does an opencode service started over an SSH session survive disconnect (Windows sshd job
  object)? Usually moot (desktop/login keeps it running). If it dies, add a login autostart.
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
- Debian VPS is not on Tailscale yet; it needs Tailscale to join.

## 13. Out of scope for v1

`sources` · pushing local edits automatically · OpenCode Console as registry · TUI plugin ·
loop guard beyond `MESH_FROM` + skill guidance · non-gh fallback · any automated tests.
