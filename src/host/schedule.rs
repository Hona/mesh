//! Periodic background jobs (SPEC §7.2, §7.3): Task Scheduler, systemd user timer, LaunchAgent.

use super::paths;
use super::process::command;
use std::path::Path;

#[cfg(windows)]
pub(crate) const SYNC_TASK: &str = r"\opencode-mesh\sync";
#[cfg(windows)]
pub(crate) const KEYS_TASK: &str = r"\opencode-mesh\keys";

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

#[cfg(windows)]
fn task_xml(exe: &Path, args: &str, every_secs: u64, elevated: bool) -> Result<String, String> {
    let sid = super::win::user_sid().ok_or("can't read the user SID")?;
    let minutes = (every_secs / 60).max(1);
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
    <TimeTrigger>
      <Repetition><Interval>PT{minutes}M</Interval><StopAtDurationEnd>false</StopAtDurationEnd></Repetition>
      <StartBoundary>2020-01-01T00:00:00</StartBoundary>
      <Enabled>true</Enabled>
    </TimeTrigger>
  </Triggers>
  <Principals>
    <Principal id="Author"><UserId>{sid}</UserId><LogonType>{logon}</LogonType><RunLevel>{level}</RunLevel></Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <StartWhenAvailable>true</StartWhenAvailable>
    <ExecutionTimeLimit>PT10M</ExecutionTimeLimit>
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
    let xml = task_xml(exe, args, every_secs, elevated)?;
    let file = paths::state_dir().join("task.xml");
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(xml.encode_utf16().flat_map(u16::to_le_bytes));
    std::fs::write(&file, bytes).map_err(|e| e.to_string())?;
    let f = file.display().to_string();
    let r = run(command("schtasks").args(["/create", "/tn", name, "/xml", &f, "/f"]));
    let _ = std::fs::remove_file(&file);
    r.map(|_| Outcome::Installed)
}

#[cfg(windows)]
pub(crate) fn run_task(name: &str) -> Result<(), String> {
    run(command("schtasks").args(["/run", "/tn", name])).map(drop)
}

/// The periodic `sync` job for this user.
#[cfg(windows)]
pub(crate) fn ensure_sync(meshw: &Path, every_secs: u64, check: bool) -> Result<Outcome, String> {
    ensure_task(SYNC_TASK, meshw, "sync", every_secs, false, check)
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

#[cfg(target_os = "linux")]
pub(crate) fn ensure_sync(mesh: &Path, every_secs: u64, check: bool) -> Result<Outcome, String> {
    let dir = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| paths::home().join(".config"))
        .join("systemd/user");
    let log = paths::state_dir().join("log").join("sync.log");
    let service = format!(
        "[Unit]\nDescription=opencode mesh sync\n\n[Service]\nType=oneshot\nExecStart=\"{}\" sync\nStandardOutput=append:{}\nStandardError=append:{}\n",
        mesh.display(),
        log.display(),
        log.display()
    );
    let timer = format!(
        "[Unit]\nDescription=opencode mesh sync\n\n[Timer]\nOnBootSec=1min\nOnUnitActiveSec={every_secs}s\n\n[Install]\nWantedBy=timers.target\n"
    );
    let a = write_if_changed(&dir.join("opencode-mesh-sync.service"), &service, check)?;
    let b = write_if_changed(&dir.join("opencode-mesh-sync.timer"), &timer, check)?;
    if !(a || b) {
        return Ok(Outcome::Unchanged);
    }
    if !check {
        let _ = std::fs::create_dir_all(log.parent().unwrap_or(Path::new(".")));
        run(command("systemctl").args(["--user", "daemon-reload"]))?;
        run(command("systemctl").args(["--user", "enable", "--now", "opencode-mesh-sync.timer"]))?;
    }
    Ok(Outcome::Installed)
}

#[cfg(target_os = "macos")]
pub(crate) fn ensure_sync(mesh: &Path, every_secs: u64, check: bool) -> Result<Outcome, String> {
    unsafe extern "C" {
        fn getuid() -> u32;
    }
    let plist = paths::home().join("Library/LaunchAgents/opencode-mesh.sync.plist");
    let log = xml_escape(
        &paths::state_dir()
            .join("log")
            .join("sync.log")
            .display()
            .to_string(),
    );
    let text = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>opencode-mesh.sync</string>
  <key>ProgramArguments</key><array><string>{}</string><string>sync</string></array>
  <key>StartInterval</key><integer>{every_secs}</integer>
  <key>RunAtLoad</key><true/>
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
        let domain = format!("gui/{}", unsafe { getuid() });
        let _ =
            run(command("launchctl").args(["bootout", &format!("{domain}/opencode-mesh.sync")]));
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
