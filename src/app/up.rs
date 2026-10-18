//! `mesh up` (SPEC §5.3): create a mesh or join this device to one. Idempotent; re-running is
//! also the repair command. Every step checks first and only changes what's missing.

use super::exec::{self, Input};
use super::store::{self, Index};
use super::{Fail, R, serve, skill, sync};
use crate::host::install::{self, Installed};
use crate::host::{opencode, paths, process, schedule, vlog};
use crate::model::{Device, Envelope, RegistryConfig, Shell, State, Step, valid_name};
use crate::transport::ssh;
use crate::{network, registry};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::IsTerminal;
use std::path::Path;
use std::time::Duration;

pub(crate) struct Opts {
    pub repo: Option<String>,
    pub path: String,
    pub name: Option<String>,
    pub public: bool,
    pub yes: bool,
    /// AGENTS.md only.
    pub prefer_local: bool,
    /// Local skills to upload (never implied by --prefer or --yes).
    pub upload_skills: Vec<String>,
    pub check: bool,
    pub wait: bool,
    pub skip: Vec<String>,
}

/// How `up` talks to the user; rendering lives in the CLI.
pub(crate) struct Ui<'a> {
    pub step: &'a dyn Fn(Step),
    /// Ask a question, return one of the `choices` characters. Only called interactively.
    pub ask: &'a dyn Fn(&str, &str) -> char,
    /// Heartbeat while a step blocks: step name, time spent so far.
    pub progress: &'a dyn Fn(&str, Duration),
}

const MESH_JSON: &str = "{\n  \"$schema\": \"https://raw.githubusercontent.com/Hona/mesh/main/schema/v1.json\",\n  \"interval\": \"10m\"\n}\n";

/// The privileged Windows steps, done by one elevated `meshw elevated <hex plan>`.
#[derive(Serialize, Deserialize, Default)]
pub(crate) struct Plan {
    install: bool,
    start: bool,
    keys_file: Option<String>,
    keys_task: Option<(String, u64)>,
    own_key: Option<String>,
    /// mesh leave: remove the keys block, the keys task and the task folder.
    #[serde(default)]
    leave: bool,
}

/// Run `plan` through one UAC prompt and wait for it (SPEC §7.1).
#[cfg(windows)]
pub(crate) fn elevate(
    plan: &Plan,
    meshw: &Path,
    rerun: &str,
    progress: &dyn Fn(Duration),
) -> R<()> {
    use crate::host::win::{self, Elevated};
    let hex = serve::hex(&serde_json::to_vec(plan).unwrap_or_default());
    let log = paths::state_dir().join(r"log\elevated.log");
    let again = format!(
        "re-run `{rerun}` and approve the UAC prompt, or run this in an admin terminal:\n  \"{}\" elevated {hex}",
        meshw.display()
    );
    let wait = (Duration::from_secs(180), Duration::from_secs(1800));
    match win::run_elevated(meshw, &format!("elevated {hex}"), wait.0, wait.1, progress) {
        Elevated::Exit(0) => Ok(()),
        Elevated::Exit(n) => Err(Fail(
            1,
            format!("the elevated step failed (exit {n}); see {}", log.display()),
        )),
        Elevated::Declined => Err(Fail(3, format!("UAC was declined: {again}"))),
        Elevated::NoAnswer => Err(Fail(3, format!("no answer to the UAC prompt: {again}"))),
        Elevated::TimedOut => Err(Fail(
            3,
            format!(
                "the elevated step is still running after 30 min; see {}",
                log.display()
            ),
        )),
        Elevated::Failed(e) => Err(Fail(3, format!("couldn't elevate ({e}): {again}"))),
    }
}

impl Plan {
    /// mesh leave's privileged part.
    #[cfg(windows)]
    pub(crate) fn leave() -> Plan {
        Plan {
            leave: true,
            ..Default::default()
        }
    }

    fn describe(&self) -> String {
        let mut v = Vec::new();
        if self.leave {
            v.push("remove the keys block and the keys task");
        }
        if self.install {
            v.push("install OpenSSH Server");
        } else if self.start {
            v.push("start sshd");
        }
        if self.keys_file.is_some() {
            v.push("create the admin keys file");
        }
        if self.own_key.is_some() {
            v.push("write the keys block");
        }
        if self.keys_task.is_some() {
            v.push("create the keys task");
        }
        v.join(", ")
    }
}

struct Up<'a> {
    o: &'a Opts,
    ui: &'a Ui<'a>,
    interactive: bool,
    failed: bool,
}

impl Up<'_> {
    fn step(&self, status: &'static str, name: &str, detail: impl Into<String>) {
        (self.ui.step)(Step {
            status,
            name: name.into(),
            detail: detail.into(),
        });
    }
    fn ok(&self, name: &str, detail: impl Into<String>) {
        self.step("ok", name, detail);
    }
    fn warn(&self, name: &str, detail: impl Into<String>) {
        self.step("warn", name, detail);
    }
    fn info(&self, name: &str, detail: impl Into<String>) {
        self.step("info", name, detail);
    }
    fn fail(&mut self, name: &str, detail: impl Into<String>) {
        self.failed = true;
        self.step("fail", name, detail);
    }
    fn choose(&self, question: &str, choices: &str, default: char) -> char {
        if self.interactive {
            (self.ui.ask)(question, choices)
        } else {
            default
        }
    }
    fn would(&self, done: &str, would: &str) -> String {
        if self.o.check {
            would.into()
        } else {
            done.into()
        }
    }

    /// A new PATH reaches agents' shells only through a restarted opencode service (SPEC §5.3
    /// step 12), done here only when it can't cut anything off.
    fn path_live(&self) {
        const HINT: &str = "restart opencode (or the desktop app) so agents can run plain `mesh`";
        if self.o.check {
            return self.info(
                "PATH",
                "would restart the opencode service if no session is running",
            );
        }
        // Windows: a service started from an SSH session is killed when the session ends.
        let ssh = cfg!(windows)
            && (std::env::var_os("SSH_CONNECTION").is_some()
                || std::env::var_os("MESH_FROM").is_some());
        if opencode::inside() || ssh {
            vlog!("PATH: no restart: run from inside opencode or an SSH session");
            return self.warn("PATH", HINT);
        }
        match opencode::restart_service(&install::dir()) {
            Ok(true) => self.ok(
                "PATH",
                "restarted the opencode service: agents can run plain `mesh`",
            ),
            Ok(false) => vlog!("PATH: no opencode service running, nothing to restart"),
            Err(e) => {
                vlog!("PATH: no restart: {e}");
                self.warn("PATH", HINT);
            }
        }
    }
}

fn user() -> String {
    std::env::var(if cfg!(windows) { "USERNAME" } else { "USER" }).unwrap_or_default()
}

pub(crate) fn run(o: &Opts, ui: &Ui) -> R<i32> {
    let mut up = Up {
        o,
        ui,
        interactive: !o.yes && std::io::stdin().is_terminal(),
        failed: false,
    };
    let check = o.check;

    // Preflight first: it decides the default repo and device name for the header.
    let inst = install::install(check)?;
    if inst.changed && !check {
        exec::quit_old_muxes();
    }
    let path_added = install::ensure_user_path(&install::dir(), check)?;
    let login = registry::login().map_err(|e| Fail(3, e))?;
    let (oc_version, oc_pid) = opencode::installed().ok_or_else(|| {
        Fail(
            3,
            "opencode not found: install the opencode CLI or desktop app".into(),
        )
    })?;
    let (host, me) = network::me()
        .map_err(|e| Fail(3, format!("{e}: install Tailscale and run `tailscale up`")))?;
    if !me.online {
        return Err(Fail(
            3,
            "tailscale is not online: run `tailscale up`".into(),
        ));
    }
    ssh::tools().map_err(|e| Fail(3, e))?;
    let repo = o
        .repo
        .clone()
        .unwrap_or_else(|| format!("{login}/opencode-mesh"));
    let name = o.name.clone().unwrap_or_else(|| host.to_lowercase());
    if !valid_name(&name) {
        return Err(Fail(
            2,
            format!("device name {name:?} isn't valid: pass --name (a-z, 0-9, -; 2+ chars)"),
        ));
    }

    up.step(
        "head",
        "",
        format!(
            "mesh up · {repo} · {name}{}",
            if check { " (check)" } else { "" }
        ),
    );
    let mut note = if inst.changed {
        up.would(" (updated)", " (would update)")
    } else {
        String::new()
    };
    if let Some(w) = &path_added {
        note += up
            .would(&format!(" · added to {w}"), &format!(" · would add to {w}"))
            .as_str();
    }
    up.ok("install", format!("{}{note}", inst.mesh.display()));
    up.ok("gh", format!("logged in as {login}"));
    let pid = oc_pid
        .map(|p| format!("  (service pid {p})"))
        .unwrap_or_default();
    up.ok("opencode", format!("{oc_version}{pid}"));
    up.ok(
        "tailscale",
        format!("{host} ({})", me.address.as_deref().unwrap_or("online")),
    );
    up.ok("ssh", "ssh · scp · ssh-keygen");

    // Repo
    let mut cfg = RegistryConfig {
        kind: "github".into(),
        repo: repo.clone(),
        path: o.path.trim_matches('/').into(),
        branch: "main".into(),
    };
    let new_repo = match registry::open(&cfg)
        .map_err(|e| Fail(3, e))?
        .default_branch()?
    {
        Some(b) => {
            cfg.branch = b;
            false
        }
        None => {
            if !check {
                registry::open(&cfg)
                    .map_err(|e| Fail(3, e))?
                    .create(o.public)?;
            }
            true
        }
    };
    let reg = registry::open(&cfg).map_err(|e| Fail(3, e))?;
    let prev = store::read_state().ok();
    let same = prev
        .as_ref()
        .is_some_and(|p| p.registry == cfg && p.device == name);
    // The shell `exec -c` uses here is decided now and recorded (SPEC §4.2): static afterwards.
    let (shell_name, shell_path) = paths::detect_shell();
    let state = State {
        registry: cfg,
        device: name.clone(),
        shell: Some(Shell {
            name: shell_name.clone(),
            path: shell_path.display().to_string(),
        }),
        synced: if same {
            prev.and_then(|p| p.synced)
        } else {
            None
        },
    };
    if !check {
        store::write_state(&state)?;
    }
    let where_ = if o.path.is_empty() {
        repo.clone()
    } else {
        format!("{repo}/{}", o.path.trim_matches('/'))
    };
    let index = match if new_repo && check {
        None
    } else {
        sync::fetch(&reg)?
    } {
        Some(i) => {
            up.ok("repo", where_);
            i
        }
        None if check => {
            up.info("repo", format!("would create a mesh in {where_}"));
            Index::new()
        }
        None => {
            let mut files = vec![
                ("mesh.json".to_string(), Some(MESH_JSON.as_bytes().to_vec())),
                (
                    "skills/mesh/SKILL.md".to_string(),
                    Some(skill::builtin().into_bytes()),
                ),
            ];
            if let Ok(agents) = fs::read(paths::config_dir().join("AGENTS.md")) {
                let q = "Use this device's AGENTS.md as the mesh's first version? [y/n]";
                if up.choose(q, "yn", 'y') == 'y' {
                    files.push(("AGENTS.md".to_string(), Some(agents)));
                }
            }
            reg.commit(&files, "mesh up: create mesh")?;
            up.ok("repo", format!("{where_} created"));
            sync::fetch(&reg)?.ok_or("the new mesh files are missing")?
        }
    };

    // Key
    let created = ssh::ensure_key(&name, check)?;
    let k = ssh::key_path().display().to_string();
    up.ok(
        "key",
        if created {
            up.would(&format!("created {k}"), "would create")
        } else {
            k
        },
    );
    up.ok(
        "shell",
        format!("{shell_name} for exec -c ({})", shell_path.display()),
    );

    // SSH server + OS permissions
    let keys_admin = if o.skip.iter().any(|s| s == "sshd") {
        up.info("sshd", "skipped");
        false
    } else {
        up.sshd(&inst)?
    };

    up.conflicts(&reg, &index, &name)?;
    let pushed = up.device(&reg, &name, &host, &inst)?;

    // Sync
    if check {
        up.info("sync", "would sync");
    } else {
        let rep = sync::run(false, false)?;
        let index = store::read_index();
        let skills: std::collections::BTreeSet<&str> = index
            .keys()
            .filter_map(|k| k.strip_prefix("skills/")?.split('/').next())
            .collect();
        up.ok(
            "sync",
            format!(
                "{} devices · {} skills",
                store::all_devices().len(),
                skills.len()
            ),
        );
        for w in &rep.warnings {
            // the elevated keys task owns the admin keys file
            if !(keys_admin && w.starts_with("authorized keys skipped")) {
                up.warn("sync", w);
            }
        }
        #[cfg(windows)]
        if keys_admin && (pushed || !rep.changes.is_empty()) {
            let _ = schedule::run_task(schedule::KEYS_TASK);
        }
        let _ = pushed;
    }

    if o.skip.iter().any(|s| s == "mcp") {
        up.info("mcp", "skipped");
    } else {
        up.mcp(&inst.mesh, inst.changed);
    }
    if o.skip.iter().any(|s| s == "schedule") {
        up.info("schedule", "skipped");
    } else {
        up.schedule(&inst);
    }
    if !check {
        up.self_test(&name);
    }
    up.include();
    if path_added.is_some() {
        up.path_live();
    }
    Ok(if up.failed { 1 } else { 0 })
}

impl Up<'_> {
    /// SSH server + OS permissions (SPEC §7.1). Returns true when the keys file needs admin.
    fn sshd(&mut self, inst: &Installed) -> R<bool> {
        let srv = ssh::server();
        if let Err(e) = &srv.keys {
            self.warn("sshd", e.clone());
            return Ok(false);
        }
        #[cfg(windows)]
        {
            let every = schedule::parse_interval(store::mesh().interval.as_deref());
            let meshw = inst.meshw.clone().unwrap_or_else(|| inst.mesh.clone());
            let keys = srv.keys.clone().unwrap_or_default();
            let mut plan = Plan {
                install: !srv.installed,
                start: !srv.running,
                ..Default::default()
            };
            if srv.keys_admin {
                if !keys.exists() {
                    plan.keys_file = Some(keys.display().to_string());
                }
                let task = schedule::ensure_task(
                    schedule::KEYS_TASK,
                    &meshw,
                    "sync --keys-only",
                    every,
                    true,
                    true,
                )?;
                if task == schedule::Outcome::Installed {
                    plan.keys_task = Some((meshw.display().to_string(), every));
                }
                if plan.install || plan.keys_file.is_some() || plan.keys_task.is_some() {
                    plan.own_key = ssh::public_key();
                }
            }
            let todo = plan.describe();
            if !todo.is_empty() {
                if self.o.check {
                    self.info("sshd", format!("would ask for one UAC prompt to: {todo}"));
                    return Ok(srv.keys_admin);
                }
                if plan.install {
                    self.warn("sshd", "installing OpenSSH Server via Windows Update: this can take several minutes");
                }
                self.warn("sshd", format!("{todo}: approve the UAC prompt"));
                elevate(&plan, &meshw, "mesh up", &|t| (self.ui.progress)("sshd", t))?;
            }
            let srv = ssh::server();
            if !srv.running {
                self.fail("sshd", "sshd isn't running");
                return Ok(srv.keys_admin);
            }
            let file = keys
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let managed = if srv.keys_admin {
                " (managed task)"
            } else {
                ""
            };
            self.ok("sshd", format!("running · keys: {file}{managed}"));
            Ok(srv.keys_admin)
        }
        #[cfg(target_os = "macos")]
        {
            let _ = inst;
            if !srv.installed || !srv.running {
                if self.o.check {
                    self.info("sshd", "would ask you to turn on Remote Login");
                    return Ok(srv.keys_admin);
                }
                self.warn("sshd", "turn on Remote Login and \"Allow full disk access for remote users\" in System Settings");
                let _ = process::command("open")
                    .arg("x-apple.systempreferences:com.apple.Sharing-Settings.extension")
                    .status();
                let start = std::time::Instant::now();
                while !ssh::server().running && start.elapsed() < Duration::from_secs(300) {
                    std::thread::sleep(Duration::from_secs(2));
                    (self.ui.progress)("sshd", start.elapsed());
                }
                if !ssh::server().running {
                    return Err(Fail(3, "Remote Login is off: System Settings → General → Sharing → Remote Login (and allow full disk access for remote users), then re-run `mesh up`".into()));
                }
            }
            self.ok("sshd", "running · keys: authorized_keys");
            Ok(srv.keys_admin)
        }
        #[cfg(target_os = "linux")]
        {
            let _ = inst;
            if !srv.installed || !srv.running {
                if self.o.check {
                    self.info("sshd", "would install/start the OpenSSH server (sudo)");
                    return Ok(false);
                }
                self.warn(
                    "sshd",
                    "installing the OpenSSH server: enter your sudo password",
                );
                ssh::install_server().map_err(|e| Fail(3, e))?;
            } else if process::command("loginctl")
                .arg("enable-linger")
                .status()
                .is_ok_and(|s| !s.success())
            {
                self.warn(
                    "sshd",
                    "`loginctl enable-linger` failed: the sync timer stops when you log out",
                );
            }
            self.ok("sshd", "running · keys: authorized_keys");
            Ok(srv.keys_admin)
        }
    }

    /// SPEC §7.4 conflicts when joining: AGENTS.md and local skills.
    fn conflicts(&mut self, reg: &registry::Registry, index: &Index, name: &str) -> R<()> {
        let cfg = paths::config_dir();
        let local_path = cfg.join("AGENTS.md");
        let mesh = index
            .contains_key("AGENTS.md")
            .then(|| fs::read(store::cached("AGENTS.md")).ok())
            .flatten();
        match (fs::read(&local_path).ok(), mesh) {
            (Some(local), Some(mesh)) if local != mesh => {
                let mut choice = if self.o.prefer_local { 'u' } else { 'k' };
                let mut local = local;
                if self.interactive {
                    loop {
                        let d = diff(
                            &String::from_utf8_lossy(&local),
                            &String::from_utf8_lossy(&mesh),
                        );
                        let q = format!(
                            "AGENTS.md differs from the mesh:\n{d}  [k]eep mesh / [u]pload this / [e]dit"
                        );
                        choice = self.choose(&q, "kue", choice);
                        if choice != 'e' {
                            break;
                        }
                        edit(&local_path);
                        local = fs::read(&local_path).unwrap_or_default();
                        if local == mesh {
                            choice = 'k';
                            break;
                        }
                    }
                }
                if choice == 'u' {
                    if !self.o.check {
                        reg.commit(
                            &[("AGENTS.md".into(), Some(local))],
                            &format!("mesh up: AGENTS.md from {name}"),
                        )?;
                        sync::fetch(reg)?;
                    }
                    self.ok(
                        "AGENTS.md",
                        self.would(
                            "uploaded this device's version",
                            "would upload this device's version",
                        ),
                    );
                } else {
                    self.ok(
                        "AGENTS.md",
                        "keeping the mesh's version (sync backs up the local one)",
                    );
                }
            }
            (_, Some(_)) => self.ok("AGENTS.md", "matches the mesh"),
            (_, None) => self.ok("AGENTS.md", "not in the mesh; local file left alone"),
        }
        let skills = cfg.join("skills");
        for e in fs::read_dir(&skills).into_iter().flatten().flatten() {
            let id = e.file_name().to_string_lossy().into_owned();
            if id == "opencode-mesh" || !e.path().is_dir() {
                continue;
            }
            let in_mesh = index
                .keys()
                .any(|k| k.starts_with(&format!("skills/{id}/")));
            // Skills are uploaded one by one, only when asked: a per-skill answer or
            // --upload-skill. --prefer decides AGENTS.md only.
            let choice = if self.o.upload_skills.contains(&id) {
                'u'
            } else if in_mesh {
                let q =
                    format!("skill {id} is both here and in the mesh: [k]eep mesh / [u]pload this");
                self.choose(&q, "ku", 'k')
            } else {
                let q = format!(
                    "skill {id} is only on this device: [u]pload / [k]eep local / [d]elete"
                );
                self.choose(&q, "ukd", 'k')
            };
            if self.o.check {
                self.info(
                    "skills",
                    format!(
                        "{id}: would {}",
                        match choice {
                            'u' => "upload",
                            'd' => "delete",
                            'k' if in_mesh => "keep the mesh's",
                            _ => "keep local",
                        }
                    ),
                );
                continue;
            }
            match (choice, in_mesh) {
                ('u', _) => {
                    let mut files = Vec::new();
                    walk(&e.path(), &format!("skills/{id}/"), &mut files);
                    reg.commit(&files, &format!("mesh up: skill {id} from {name}"))?;
                    backup_remove(&e.path(), &id)?;
                    sync::fetch(reg)?;
                    self.ok("skills", format!("{id}: uploaded to the mesh"));
                }
                ('k', true) | ('d', _) => {
                    let b = backup_remove(&e.path(), &id)?;
                    self.ok(
                        "skills",
                        format!("{id}: local copy moved to {}", b.display()),
                    );
                }
                _ => self.ok("skills", format!("{id}: kept local")),
            }
        }
        Ok(())
    }

    /// devices/<name>.{json,pub,host.pub}, pushed in one commit when anything differs.
    fn device(
        &mut self,
        reg: &registry::Registry,
        name: &str,
        host: &str,
        inst: &Installed,
    ) -> R<bool> {
        let (os, arch) = paths::os_arch();
        let d = Device {
            name: name.into(),
            host: host.into(),
            os: os.into(),
            arch: arch.into(),
            shell: paths::detect_shell().0,
            user: user(),
            mesh: inst.mesh.display().to_string(),
            ..Default::default()
        };
        let json = serde_json::to_string_pretty(&d).map_err(|e| e.to_string())? + "\n";
        let mut files = vec![(format!("devices/{name}.json"), json.into_bytes())];
        files.extend(ssh::public_key().map(|k| (format!("devices/{name}.pub"), k.into_bytes())));
        match ssh::host_key() {
            Some(k) => files.push((format!("devices/{name}.host.pub"), k.into_bytes())),
            None => self.warn(
                "device",
                "no SSH host key yet: peers can't verify this device",
            ),
        }
        let changed: Vec<(String, Option<Vec<u8>>)> = files
            .into_iter()
            .filter(|(p, b)| fs::read(store::cached(p)).ok().as_deref() != Some(b.as_slice()))
            .map(|(p, b)| (p, Some(b)))
            .collect();
        let label = format!("devices/{name}.{{json,pub,host.pub}}");
        if changed.is_empty() {
            self.ok("device", format!("{label} up to date"));
            return Ok(false);
        }
        if !self.o.check {
            reg.commit(&changed, &format!("mesh up: {name}"))?;
        }
        self.ok(
            "device",
            self.would(&format!("{label} pushed"), &format!("would push {label}")),
        );
        Ok(true)
    }

    /// Register the MCP server; when the binary was just replaced, restart the running ones so
    /// opencode isn't left talking to old code.
    fn mcp(&mut self, mesh: &Path, replaced: bool) {
        let want = vec![mesh.display().to_string(), "mcp".to_string()];
        let restart = |up: &Self, what: &str| {
            if up.o.check {
                return up.ok("mcp", format!("{what} · would restart it in opencode"));
            }
            match opencode::reconnect_mcp("mesh") {
                Ok(0) => up.ok("mcp", what),
                Ok(n) => up.ok(
                    "mcp",
                    format!("{what} · restarted on the new binary ({n} opencode locations)"),
                ),
                Err(e) => up.warn(
                    "mcp",
                    format!("{what}, but restarting it failed ({e}): run `opencode reload`"),
                ),
            }
        };
        match opencode::mcp_entry("mesh") {
            Ok(Some(cmd)) if cmd == want && replaced => restart(self, "\"mesh\" registered"),
            Ok(Some(cmd)) if cmd == want => self.ok("mcp", "\"mesh\" registered"),
            Ok(existing) => {
                if let Some(other) = existing {
                    let q = format!(
                        "opencode has a different \"mesh\" MCP server ({}). Replace it? [y/n]",
                        other.join(" ")
                    );
                    if self.choose(&q, "yn", 'y') == 'n' {
                        self.warn("mcp", "kept the existing \"mesh\" entry");
                        return;
                    }
                }
                if self.o.check {
                    return self.info("mcp", "would register \"mesh\"");
                }
                match opencode::mcp_add("mesh", &[&want[0], "mcp"]) {
                    Ok(()) => {
                        let _ = opencode::reload();
                        restart(self, "registered \"mesh\"");
                    }
                    Err(e) => self.fail("mcp", e),
                }
            }
            Err(e) => self.fail("mcp", e),
        }
    }

    fn schedule(&mut self, inst: &Installed) {
        let every = schedule::parse_interval(store::mesh().interval.as_deref());
        let exe = inst.meshw.clone().unwrap_or_else(|| inst.mesh.clone());
        let what = format!("sync every {}m", every / 60);
        match schedule::ensure_sync(&exe, every, self.o.check) {
            Ok(schedule::Outcome::Unchanged) => self.ok("schedule", what),
            Ok(schedule::Outcome::Installed) => self.ok(
                "schedule",
                self.would(&what, &format!("would set up {what}")),
            ),
            Err(e) => self.fail("schedule", e),
        }
    }

    /// Self-test over SSH to this device, then a no-op on each online peer.
    fn self_test(&mut self, name: &str) {
        let all = store::all_devices();
        let Some(me) = all.iter().find(|d| d.name == name) else {
            return self.fail("self-test", "this device isn't in the mesh cache");
        };
        let env = Envelope {
            argv: Some(vec!["opencode".into(), "--version".into()]),
            timeout: Some(60.0),
            ..Default::default()
        };
        let r = exec::capture(name, std::slice::from_ref(me), &env, Input::Empty, 4096).remove(0);
        if r.exit == Some(0) {
            let v = r
                .stdout
                .as_deref()
                .unwrap_or("")
                .split_whitespace()
                .last()
                .unwrap_or("?")
                .trim_start_matches('v')
                .to_string();
            self.ok(
                "self-test",
                format!("{name} → opencode {v}  ({} ms)", r.ms.unwrap_or(0)),
            );
        } else {
            let why = r
                .error
                .or(r.stderr.filter(|s| !s.trim().is_empty()))
                .unwrap_or_else(|| format!("exit {:?}", r.exit));
            self.fail("self-test", why.trim().to_string());
        }
        let hosts: Vec<&str> = all.iter().map(|d| d.host.as_str()).collect();
        let online = network::peers(&hosts).unwrap_or_default();
        let peers: Vec<Device> = all
            .iter()
            .filter(|d| d.name != name && online.get(&d.host).is_some_and(|p| p.online))
            .cloned()
            .collect();
        if peers.is_empty() {
            return;
        }
        let interval = store::mesh().interval.unwrap_or_else(|| "10m".into());
        let noop = Envelope {
            argv: Some(vec!["mesh".into(), "--version".into()]),
            timeout: Some(30.0),
            ..Default::default()
        };
        let start = std::time::Instant::now();
        let deadline = start + Duration::from_secs(1800);
        let mut announced = false;
        loop {
            let results = exec::capture(name, &peers, &noop, Input::Empty, 4096);
            let (ok, pending): (Vec<_>, Vec<_>) =
                results.into_iter().partition(|r| r.exit == Some(0));
            let names = |v: &[crate::model::ExecResult]| {
                v.iter()
                    .map(|r| r.device.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            if pending.is_empty() {
                return self.ok("peers", format!("{} accept {name}", names(&ok)));
            }
            if !self.o.wait || std::time::Instant::now() > deadline {
                if !ok.is_empty() {
                    self.ok("peers", format!("{} accept {name}", names(&ok)));
                }
                return self.info(
                    "peers",
                    format!(
                        "{} accept {name} after their next sync (≤{interval})",
                        names(&pending)
                    ),
                );
            }
            if !announced {
                announced = true;
                self.info(
                    "peers",
                    format!(
                        "waiting for {} to accept {name} (≤{interval})",
                        names(&pending)
                    ),
                );
            }
            for _ in 0..4 {
                std::thread::sleep(Duration::from_secs(5));
                (self.ui.progress)("peers", start.elapsed());
            }
        }
    }

    /// Opt-in `Include` in ~/.ssh/config so plain `ssh <device>` works.
    fn include(&mut self) {
        match ssh::include_state(&store::all_devices()) {
            ssh::Include::Present => self.ok("ssh", "~/.ssh/config includes the mesh config"),
            ssh::Include::Clash(n) => self.info(
                "ssh",
                format!("not offering `Include`: ~/.ssh/config already has Host {n}"),
            ),
            ssh::Include::Absent => {
                let q =
                    "Add one `Include` line to ~/.ssh/config so plain `ssh <device>` works? [y/n]";
                if !self.o.check && self.choose(q, "yn", 'n') == 'y' {
                    match ssh::add_include() {
                        Ok(()) => self.ok("ssh", "added `Include` to ~/.ssh/config"),
                        Err(e) => self.fail("ssh", e),
                    }
                } else {
                    self.info(
                        "ssh",
                        "plain `ssh <device>` not set up (opt-in: run `mesh up` interactively)",
                    );
                }
            }
        }
    }
}

/// Lines only here (-) and only in the mesh (+).
fn diff(local: &str, mesh: &str) -> String {
    let (l, m): (Vec<&str>, Vec<&str>) = (local.lines().collect(), mesh.lines().collect());
    let mut s = String::new();
    for x in l.iter().filter(|x| !m.contains(x)) {
        s += &format!("    - {x}\n");
    }
    for x in m.iter().filter(|x| !l.contains(x)) {
        s += &format!("    + {x}\n");
    }
    s
}

fn edit(p: &Path) {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| {
        if cfg!(windows) {
            "notepad".into()
        } else {
            "vi".into()
        }
    });
    let _ = process::command(editor).arg(p).status();
}

fn walk(dir: &Path, prefix: &str, out: &mut Vec<(String, Option<Vec<u8>>)>) {
    for e in fs::read_dir(dir).into_iter().flatten().flatten() {
        let rel = format!("{prefix}{}", e.file_name().to_string_lossy());
        if e.path().is_dir() {
            walk(&e.path(), &format!("{rel}/"), out);
        } else {
            out.push((rel, fs::read(e.path()).ok()));
        }
    }
}

/// Move a local skill into `<state>/backup/<time>/skills/<id>`.
fn backup_remove(dir: &Path, id: &str) -> Result<std::path::PathBuf, String> {
    let dest = store::new_backup_dir().join("skills").join(id);
    let mut files = Vec::new();
    walk(dir, "", &mut files);
    for (rel, data) in files {
        store::write_file(&dest.join(rel), &data.unwrap_or_default())?;
    }
    fs::remove_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    Ok(dest)
}

/// `mesh elevated <hex plan>` (hidden): the one privileged step of `mesh up` on Windows.
pub(crate) fn elevated(hex: &str) -> R<i32> {
    let plan: Plan = serve::unhex(hex)
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| Fail(2, "elevated: bad plan".into()))?;
    println!("elevated: {}", plan.describe());
    #[cfg(windows)]
    {
        if plan.install || plan.start {
            ssh::install_server(plan.install).map_err(|e| format!("sshd: {e}"))?;
            println!("sshd: running");
        }
        if let Some(p) = &plan.keys_file {
            ssh::create_admin_keys_file(Path::new(p))?;
            println!("keys file: created {p}");
        }
        if let Some(k) = &plan.own_key {
            let mut devices = store::all_devices();
            devices.push(Device {
                name: "self".into(),
                key: Some(k.clone()),
                ..Default::default()
            });
            crate::transport::authorize(&devices)?;
            println!("keys block: written");
        }
        if let Some((exe, every)) = &plan.keys_task {
            schedule::ensure_task(
                schedule::KEYS_TASK,
                Path::new(exe),
                "sync --keys-only",
                *every,
                true,
                false,
            )?;
            println!("keys task: {}", schedule::KEYS_TASK);
        }
        if plan.leave {
            println!("keys block removed: {}", ssh::unauthorize()?);
            println!("keys task removed: {}", schedule::remove_keys_task()?);
        }
    }
    Ok(0)
}
