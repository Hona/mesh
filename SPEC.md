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
   is mesh-owned (see §7.4), with backups.
6. **Opt-in targeting.** Commands reach only devices the user names (§5.1).
7. **Raw, unopinionated tools.** Two MCP tools (`status`, `exec`). No task-specific tools.
   Everything else is composed from `mesh exec <device> opencode …` or plain commands.
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
  "shell": "powershell",         // the SSH server's actual default shell (used by `exec -c`)
  "user": "lukem",               // SSH login user
  "mesh": "C:\\Users\\lukem\\AppData\\Local\\Programs\\mesh\\mesh.exe"   // absolute path
}
```

No opencode path is stored: it changes on every opencode update (§6.4).
No Tailscale IPs or tailnet domain are stored (public-safe).

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
                  #   "device", "synced": "<commit sha>" }
id_ed25519(.pub)  # this device's mesh key (permissions locked to the user)
ssh_config        # generated; used via `ssh -F` for every mesh connection
known_hosts       # generated from devices/*.host.pub
cache/            # last pulled copy of the mesh repo + per-file sha index
backup/<time>/    # backups of anything mesh replaced (AGENTS.md, conflicting skills)
log/              # background run logs
```

Installed binary: `~/.local/bin/mesh` (Linux/macOS);
`%LOCALAPPDATA%\Programs\mesh\{mesh,meshw}.exe` (Windows, added to user PATH).
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

Create a mesh or join this device to one. Idempotent; re-running is also the repair command.

```
mesh up [--repo <owner/name>] [--path <dir>] [--name <device>] [--public]
        [--yes] [--prefer mesh|local] [--check] [--wait] [--skip <step,...>]
```

| Flag | |
|---|---|
| `--repo` | mesh repo; default `<gh-user>/opencode-mesh`; created if missing |
| `--path` | subfolder within the repo |
| `--name` | device name; default Tailscale short name |
| `--public` | create the repo public (default private) |
| `--yes` | non-interactive (agents/scripts); conflicts default to `--prefer mesh` |
| `--prefer` | conflict default: `mesh` (keep mesh, back up local) or `local` (upload local) |
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
   MCP entry exists, ask.
9. **Schedule** — §7.2.
10. **Self-test** — via the mesh SSH config to itself: `mesh exec self opencode --version`
    over SSH. Then `exec` a no-op on each online peer; peers that haven't synced yet are
    reported as "pending (≤ interval)".
11. **Opt-in extra** — offer to add one `Include <state>/ssh_config` line to `~/.ssh/config`
    so plain `ssh <device>` works. Skipped if any mesh device name clashes with an existing
    `Host` entry.

Example:

```
$ mesh up
mesh up · Hona/opencode-mesh · adais
  ✓ gh         logged in as Hona
  ✓ opencode   2.0.32  (service pid 14968)
  ✓ tailscale  adais (online)
  ✓ repo       Hona/opencode-mesh
  ✓ key        created
  ! sshd       not installed — approve the UAC prompt
  ✓ sshd       running · shell: cmd · keys: administrators_authorized_keys (managed task)
  ? AGENTS.md  differs from the mesh  [k]eep mesh / [u]pload this / [e]dit
  ✓ device     devices/adais.{json,pub,host.pub} pushed
  ✓ sync       3 devices · 2 skills
  ✓ mcp        registered "mesh"
  ✓ schedule   sync every 10m
  ✓ self-test  adais → opencode 2.0.32
  … peers      blade, air accept adais after their next sync (≤10m)
```

### 5.4 `mesh status`

```
mesh status [<target>] [--fast] [--json]
```

Default target `all`. Without `--fast`, each online device is probed in parallel over the
handoff (§6.2) by running `mesh status self --json` there, which reports local facts:
opencode version + whether the service is up (from `service.json`, §6.4), busy session count
(`opencode api session.active`), mesh version. `--fast` uses only the repo cache + Tailscale.

```
DEVICE   OS       SHELL       ONLINE  OPENCODE  BUSY  MESH
adais *  windows  cmd         yes     2.0.32    0     0.1.0
blade    windows  powershell  yes     2.0.32    1     0.1.0
air      macos    zsh         no      -         -     -
```

Also shows: pending binary updates (`0.1.0 → 0.2.0 (pending)`), a customised mesh skill,
local `AGENTS.md` edits that were backed up, last sync time/commit.

### 5.5 `mesh exec`

Run a command on devices.

```
mesh exec [-t] [-w <dir>] [-e KEY=VAL]... [--timeout <s>] [--json] <target> <cmd> [args…]
mesh exec [-t] [-w <dir>] [-e KEY=VAL]... [--timeout <s>] [--json] <target> -c '<string>'
```

| Rule | Precedent |
|---|---|
| Default: run the exact argv, **no shell**, identical on every OS | `docker exec`, `kubectl exec` |
| `-c '<string>'`: run the string in the device's own default shell | `sh -c`, `ssh host '…'` |
| argv[0] `opencode` or `mesh` resolves to that device's install (§6.4) | — |
| stdin is passed through | `ssh` |
| `-t` allocates a TTY (interactive; e.g. `mesh exec -t blade opencode` = remote TUI) | `ssh -t`, `docker exec -it` |
| `-w` working dir, `-e` env var | `docker exec -w / -e` |
| Everything after `<target>` belongs to the command; `--` is optional | `docker exec`, `ssh` |

- Single device: output streams live; exit code passed through.
- Multiple devices: run in parallel. Text output prefixes each line with `device │ `, then a
  summary line (`✓ blade 0 1.2s · ✗ air 1 0.4s · ⨯ vps offline`). `--json` returns results.
- `--timeout`: enforced **on the remote side** (kills the whole process tree), plus a local
  backstop. CLI default: none.
- The remote process gets `MESH_FROM=<calling device>`.
- For long-lived background commands, use opencode's own shells:
  `mesh exec blade opencode api shell.create …` / `shell.output`.

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
3. If `mesh.json` `pin` differs from this binary's version, self-update to the pin (§5.9).
4. If skills changed, `opencode reload`.

Prints a short diff (`+ device air`, `~ skill review`, …). Runs on schedule and lazily when a
target names an unknown device.

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
opt-in `Include` line, `skills/opencode-mesh/`, the state dir (incl. key). Leaves
`~/.config/opencode/AGENTS.md` and an installed SSH server in place.

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
   their next sync (they see `pin`).
4. On each device, reconnect the MCP server so the new binary is used
   (`opencode api experimental.mcp.disconnect` / `connect` for `mesh`).

Devices never move to "latest" on their own — only to the pinned version.
`--version` also rolls back. `--check` shows what's available.
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
  "argv": ["opencode", "api", "session.active"],   // or "command": "<string>" (-c mode)
  "cwd": "D:\\Hona\\app", "env": { "K": "V" }, "tty": false, "timeout": 120 }
```

- The remote side is just `mesh exec self`: it resolves `opencode`/`mesh`, sets `MESH_FROM`,
  enforces the timeout, and execs the argv directly (no shell) or the string via the device's
  shell.
- stdout/stderr ride SSH's own channels; the exit status is SSH's exit status. No framing.
- Unknown envelope fields are ignored; a higher major `v` fails with an upgrade message.
- `-t` mode: same, with `ssh -t`.

### 6.3 Remote shell quoting

Never build shell strings from user argv. The only string that crosses a remote shell is the
quoted absolute mesh path + fixed flags above.

### 6.4 Finding opencode on a device (at run time, on that device)

1. `opencode` on PATH.
2. `~/.local/state/opencode/service.json` (respect `XDG_STATE_HOME`) → `pid` → that process's
   executable path (the one persistent service; e.g.
   `%APPDATA%\ai.opencode.desktop\cli\2.0.32-x64\opencode-cli.exe serve --service`).
3. Known install locations (e.g. newest version dir under the desktop app's `cli\`, the
   desktop app's bundled `resources\opencode-cli.exe`).

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

## 7. Setup details

### 7.1 SSH server and OS permissions (inside `mesh up`, once)

Adapt to what exists; never reconfigure an existing server.

| OS | If SSH server missing | Always |
|---|---|---|
| Windows | One UAC: install OpenSSH Server capability, start it, set Automatic | Detect default shell (`HKLM\SOFTWARE\OpenSSH\DefaultShell`, else `cmd`); detect effective keys file (parse `sshd_config` incl. `Match Group administrators`) |
| macOS | Open Sharing settings; user turns on **Remote Login** | Also require **"Allow full disk access for remote users"** (otherwise remote commands can't read `~/Documents`, `~/Desktop`); open the exact pane, wait, re-verify via SSH to self |
| Linux | One sudo: install via detected package manager | `loginctl enable-linger $USER` (user timers survive logout); detect login shell |

### 7.2 Scheduling

Runs `mesh sync` every `interval`, as the user, no admin:

| OS | Mechanism |
|---|---|
| Linux | systemd user timer `opencode-mesh-sync.timer` |
| macOS | LaunchAgent `opencode-mesh.sync` |
| Windows | Task Scheduler `\opencode-mesh\sync`, "run only when user is logged on", action `meshw.exe sync` |

`meshw.exe` is the windowless (`windows_subsystem = "windows"`) build of the same code — the
`pythonw.exe` / `javaw.exe` convention. Background runs log to `<state>/log/`.

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
  upload / keep local / delete. Same-ID skills in both: same diff prompt.
- `--yes` defaults to `--prefer mesh`; never uploads unless `--prefer local`.

## 8. MCP server

- Server name `mesh`; stdio; newline-delimited JSON-RPC 2.0; hand-rolled.
- Methods: `initialize`, `notifications/initialized`, `ping`, `tools/list`, `tools/call`.
- Tool results: `content` (readable text for the model) + `structuredContent` (same JSON as
  the CLI's `--json`). `isError` only for mesh-level errors (bad arguments); per-device
  failures live in `results`.
- `instructions` (one line, from the local cache):
  `Opencode mesh: devices adais (windows), blade (windows), air (macos). Load the "mesh" skill before using mesh tools.`

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
      "opencode": { "version": "2.0.32", "busy": 1 }, "mesh": "0.1.0" } ] }
```

### 8.2 `exec`

```jsonc
// input — exactly one of argv | command
{ "target":    { "type": "string" },                                 // required
  "argv":      { "type": "array", "items": { "type": "string" } },   // no shell
  "command":   { "type": "string" },                                 // device's shell (-c)
  "stdin":     { "type": "string" },
  "cwd":       { "type": "string" },
  "env":       { "type": "object", "additionalProperties": { "type": "string" } },
  "timeout":   { "type": "number", "default": 120 },                 // seconds
  "max_bytes": { "type": "number", "default": 50000 } }              // per stream, keeps head + tail
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
- CI (`release.yml`): cross-build all targets and publish. **No test steps.**
- Install, macOS/Linux:
  `gh api repos/Hona/mesh/contents/install.sh -H "Accept: application/vnd.github.raw" | sh`
- Install, Windows:
  `gh api repos/Hona/mesh/contents/install.ps1 -H "Accept: application/vnd.github.raw" | Out-String | iex`
- Both download via `gh release download`, install to the paths in §4.4, add to user PATH, then
  run `mesh up`.
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
- Windows admin keys file + elevated `keys` task end to end.
- macOS Remote Login + full-disk-access flow (needs air online).
- Debian VPS is not on Tailscale yet; it needs Tailscale to join.

## 13. Out of scope for v1

`sources` · pushing local edits automatically · OpenCode Console as registry · TUI plugin ·
loop guard beyond `MESH_FROM` + skill guidance · non-gh fallback · any automated tests.
