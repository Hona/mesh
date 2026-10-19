//! Background jobs (SPEC §7.2, §7.3): the daemon's autostart (Task Scheduler, systemd user
//! service, LaunchAgent) and the elevated Windows keys task.

use super::paths;
use super::process::command;
use std::path::Path;

#[cfg(windows)]
pub(crate) const KEYS_TASK: &str = r"\opencode-mesh\keys";
#[cfg(windows)]
pub(crate) const DAEMON_TASK: &str = r"\opencode-mesh\daemon";
#[cfg(target_os = "linux")]
const DAEMON_UNIT: &str = "opencode-mesh.service";
#[cfg(target_os = "macos")]
const DAEMON_LABEL: &str = "opencode-mesh.daemon";

#[derive(PartialEq)]
pub(crate) enum Outcome {
    Unchanged,
    Installed,
}

fn run(cmd: &mut std::process::Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| e.to_string())?;
    let text = decode(&out.stdout) + &decode(&out.stderr);
    if out.status.success() {
        Ok(text)
    } else {
        Err(text.trim().to_string())
    }
}

/// schtasks prints UTF-16 for /xml queries and the console code page otherwise.
fn decode(b: &[u8]) -> String {
    if b.len() >= 2
        && b.len().is_multiple_of(2)
        && b.iter().skip(1).step_by(2).filter(|&&c| c == 0).count() > b.len() / 4
    {
        let w: Vec<u16> = b
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&w)
    } else {
        String::from_utf8_lossy(b).into_owned()
    }
}

#[cfg(not(target_os = "linux"))]
fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Every `every_secs`; `daemon`: also at this user's logon, parallel instances (a new build
/// starts while the old one finishes its commands; a second one of the same build just exits),
/// no time limit, normal priority (what it starts inherits it).
#[cfg(windows)]
fn task_xml(
    exe: &Path,
    args: &str,
    every_secs: u64,
    daemon: bool,
    elevated: bool,
) -> Result<String, String> {
    let sid = super::win::user_sid().ok_or("can't read the user SID")?;
    let mut triggers = format!(
        "<TimeTrigger>
      <Repetition><Interval>PT{}M</Interval><StopAtDurationEnd>false</StopAtDurationEnd></Repetition>
      <StartBoundary>2020-01-01T00:00:00</StartBoundary>
      <Enabled>true</Enabled>
    </TimeTrigger>",
        (every_secs / 60).max(1)
    );
    let (instances, limit, priority) = if daemon {
        triggers += &format!(
            "\n    <LogonTrigger><Enabled>true</Enabled><UserId>{sid}</UserId></LogonTrigger>"
        );
        ("Parallel", "PT0S", "\n    <Priority>5</Priority>")
    } else {
        ("IgnoreNew", "PT10M", "")
    };
    let (logon, level, sd) = if elevated {
        // S4U: runs whether or not the user is logged on; the user may read and run it.
        (
            "S4U",
            "HighestAvailable",
            format!(
                "<SecurityDescriptor>D:(A;;FA;;;BA)(A;;FA;;;SY)(A;;FRFX;;;{sid})</SecurityDescriptor>"
            ),
        )
    } else {
        ("InteractiveToken", "LeastPrivilege", String::new())
    };
    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>opencode mesh</Description>{sd}</RegistrationInfo>
  <Triggers>
    {triggers}
  </Triggers>
  <Principals>
    <Principal id="Author"><UserId>{sid}</UserId><LogonType>{logon}</LogonType><RunLevel>{level}</RunLevel></Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>{instances}</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <ExecutionTimeLimit>{limit}</ExecutionTimeLimit>{priority}
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author">
    <Exec><Command>{}</Command><Arguments>{args}</Arguments></Exec>
  </Actions>
</Task>
"#,
        xml_escape(&exe.display().to_string())
    ))
}

/// Does the registered task already run `exe args` every `every_secs`?
#[cfg(windows)]
fn task_matches(name: &str, exe: &Path, args: &str, every_secs: u64) -> bool {
    let Ok(xml) = run(command("schtasks").args(["/query", "/tn", name, "/xml"])) else {
        return false;
    };
    let minutes = (every_secs / 60).max(1);
    xml.contains(&format!(
        "<Command>{}</Command>",
        xml_escape(&exe.display().to_string())
    )) && xml.contains(&format!("<Arguments>{args}</Arguments>"))
        && xml.contains(&format!("<Interval>PT{minutes}M</Interval>"))
}

#[cfg(windows)]
pub(crate) fn ensure_task(
    name: &str,
    exe: &Path,
    args: &str,
    every_secs: u64,
    elevated: bool,
    check: bool,
) -> Result<Outcome, String> {
    if task_matches(name, exe, args, every_secs) {
        return Ok(Outcome::Unchanged);
    }
    if check {
        return Ok(Outcome::Installed);
    }
    create(name, &task_xml(exe, args, every_secs, false, elevated)?).map(|_| Outcome::Installed)
}

#[cfg(windows)]
fn create(name: &str, xml: &str) -> Result<(), String> {
    let file = paths::state_dir().join(format!("task-{}.xml", std::process::id()));
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(&file, bytes).map_err(|e| e.to_string())?;
    let f = file.display().to_string();
    let r = run(command("schtasks").args(["/create", "/tn", name, "/xml", &f, "/f"]));
    let _ = std::fs::remove_file(&file);
    r.map(drop)
}

/// The daemon's autostart: at logon, kept alive every `every_secs` (a running one makes the
/// new instance exit at once).
#[cfg(windows)]
pub(crate) fn ensure_daemon(meshw: &Path, every_secs: u64, check: bool) -> Result<Outcome, String> {
    if task_matches(DAEMON_TASK, meshw, "daemon", every_secs) {
        return Ok(Outcome::Unchanged);
    }
    if check {
        return Ok(Outcome::Installed);
    }
    create(
        DAEMON_TASK,
        &task_xml(meshw, "daemon", every_secs, true, false)?,
    )
    .map(|_| Outcome::Installed)
}

/// Start the daemon now (no-op where one already runs, except that a new build retires it).
pub(crate) fn start_daemon() -> Result<(), String> {
    #[cfg(windows)]
    let r = run(command("schtasks").args(["/run", "/tn", DAEMON_TASK]));
    #[cfg(target_os = "linux")]
    let r = run(command("systemctl").args(["--user", "start", DAEMON_UNIT]));
    #[cfg(target_os = "macos")]
    let r = run(command("launchctl").args(["kickstart", &format!("{}/{DAEMON_LABEL}", gui())]));
    r.map(drop)
}

#[cfg(target_os = "macos")]
fn gui() -> String {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    format!("gui/{}", unsafe { getuid() })
}

#[cfg(target_os = "linux")]
fn unit_dir() -> std::path::PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| paths::home().join(".config"))
        .join("systemd/user")
}

/// Undo `ensure_daemon` (mesh leave). Ok(false) if there was nothing to remove.
pub(crate) fn remove_daemon() -> Result<bool, String> {
    #[cfg(windows)]
    return remove_task(DAEMON_TASK);
    #[cfg(target_os = "linux")]
    {
        let unit = unit_dir().join(DAEMON_UNIT);
        if !unit.exists() {
            return Ok(false);
        }
        let _ = run(command("systemctl").args(["--user", "disable", "--now", DAEMON_UNIT]));
        std::fs::remove_file(&unit).map_err(|e| format!("{}: {e}", unit.display()))?;
        let _ = run(command("systemctl").args(["--user", "daemon-reload"]));
        Ok(true)
    }
    #[cfg(target_os = "macos")]
    {
        let plist = paths::home().join(format!("Library/LaunchAgents/{DAEMON_LABEL}.plist"));
        if !plist.exists() {
            return Ok(false);
        }
        let _ = run(command("launchctl").args(["bootout", &format!("{}/{DAEMON_LABEL}", gui())]));
        std::fs::remove_file(&plist).map_err(|e| format!("{}: {e}", plist.display()))?;
        Ok(true)
    }
}

/// Delete a task; Ok(false) if it didn't exist.
#[cfg(windows)]
pub(crate) fn remove_task(name: &str) -> Result<bool, String> {
    if run(command("schtasks").args(["/query", "/tn", name])).is_err() {
        return Ok(false);
    }
    run(command("schtasks").args(["/delete", "/tn", name, "/f"])).map(|_| true)
}

/// Elevated (mesh leave): the keys task and the `\opencode-mesh` task folder.
#[cfg(windows)]
pub(crate) fn remove_keys_task() -> Result<bool, String> {
    let removed = remove_task(KEYS_TASK)?;
    let _ = remove_task(DAEMON_TASK);
    let script = "$s = New-Object -ComObject Schedule.Service; $s.Connect(); \
        try { $s.GetFolder('\\').DeleteFolder('opencode-mesh', 0) } catch {}";
    let _ =
        run(command("powershell.exe").args(["-NoProfile", "-NonInteractive", "-Command", script]));
    Ok(removed)
}

#[cfg(windows)]
pub(crate) fn run_task(name: &str) -> Result<(), String> {
    run(command("schtasks").args(["/run", "/tn", name])).map(drop)
}

#[cfg(unix)]
fn write_if_changed(p: &Path, text: &str, check: bool) -> Result<bool, String> {
    if std::fs::read_to_string(p).ok().as_deref() == Some(text) {
        return Ok(false);
    }
    if !check {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        std::fs::write(p, text).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    Ok(true)
}

/// systemd user service, restarted whenever it exits. `KillMode=process`: stopping or
/// restarting it ends only the daemon, never what its commands started.
#[cfg(target_os = "linux")]
pub(crate) fn ensure_daemon(mesh: &Path, _every_secs: u64, check: bool) -> Result<Outcome, String> {
    let log = paths::state_dir().join("log").join("daemon.log");
    let service = format!(
        "[Unit]\nDescription=opencode mesh daemon\n\n[Service]\nExecStart=\"{}\" daemon\nRestart=always\nRestartSec=1\nKillMode=process\nStandardOutput=append:{}\nStandardError=append:{}\n\n[Install]\nWantedBy=default.target\n",
        mesh.display(),
        log.display(),
        log.display()
    );
    if !write_if_changed(&unit_dir().join(DAEMON_UNIT), &service, check)? {
        return Ok(Outcome::Unchanged);
    }
    if !check {
        let _ = std::fs::create_dir_all(log.parent().unwrap_or(Path::new(".")));
        run(command("systemctl").args(["--user", "daemon-reload"]))?;
        run(command("systemctl").args(["--user", "enable", DAEMON_UNIT]))?;
    }
    Ok(Outcome::Installed)
}

/// LaunchAgent: at login, and again whenever it exits.
#[cfg(target_os = "macos")]
pub(crate) fn ensure_daemon(mesh: &Path, _every_secs: u64, check: bool) -> Result<Outcome, String> {
    let plist = paths::home().join(format!("Library/LaunchAgents/{DAEMON_LABEL}.plist"));
    let log = xml_escape(
        &paths::state_dir()
            .join("log")
            .join("daemon.log")
            .display()
            .to_string(),
    );
    let text = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>{DAEMON_LABEL}</string>
  <key>ProgramArguments</key><array><string>{}</string><string>daemon</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict></plist>
"#,
        xml_escape(&mesh.display().to_string())
    );
    if !write_if_changed(&plist, &text, check)? {
        return Ok(Outcome::Unchanged);
    }
    if !check {
        let domain = gui();
        let _ = run(command("launchctl").args(["bootout", &format!("{domain}/{DAEMON_LABEL}")]));
        run(command("launchctl").args(["bootstrap", &domain, &plist.display().to_string()]))?;
    }
    Ok(Outcome::Installed)
}

/// "10m" / "1h" / "30s" / "600" -> seconds (default 10 minutes).
pub(crate) fn parse_interval(s: Option<&str>) -> u64 {
    let s = s.unwrap_or("10m").trim();
    let (num, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        _ => (s, 1),
    };
    num.trim()
        .parse::<u64>()
        .map(|n| n * mult)
        .unwrap_or(600)
        .max(60)
}
