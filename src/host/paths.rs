use std::env;
use std::path::PathBuf;

pub(crate) fn home() -> PathBuf {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    env::var_os(var).map(PathBuf::from).unwrap_or_default()
}

pub(crate) fn env_dir(var: &str) -> Option<PathBuf> {
    env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn xdg(var: &str, default: &str) -> PathBuf {
    env_dir(var).unwrap_or_else(|| home().join(default))
}

pub(crate) fn state_dir() -> PathBuf {
    if cfg!(windows) {
        env_dir("LOCALAPPDATA")
            .unwrap_or_else(|| home().join("AppData").join("Local"))
            .join("opencode-mesh")
    } else {
        xdg("XDG_DATA_HOME", ".local/share").join("opencode-mesh")
    }
}

/// `~/.config/opencode`
pub(crate) fn config_dir() -> PathBuf {
    xdg("XDG_CONFIG_HOME", ".config").join("opencode")
}

/// `~/.local/state/opencode` (holds `service.json`)
pub(crate) fn opencode_state_dir() -> PathBuf {
    xdg("XDG_STATE_HOME", ".local/state").join("opencode")
}

/// Per-user app data dir (where the opencode desktop app keeps its CLI).
pub(crate) fn app_data_dir() -> PathBuf {
    if cfg!(windows) {
        env_dir("APPDATA").unwrap_or_else(|| home().join("AppData").join("Roaming"))
    } else if cfg!(target_os = "macos") {
        home().join("Library").join("Application Support")
    } else {
        xdg("XDG_DATA_HOME", ".local/share")
    }
}

pub(crate) fn on_path(name: &str) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd"]
    } else {
        &[""]
    };
    env::split_paths(&env::var_os("PATH")?).find_map(|dir| {
        exts.iter()
            .map(|e| dir.join(format!("{name}{e}")))
            .find(|p| p.is_file())
    })
}

/// A tool on PATH, else in its known install locations (desktop shells often have a stale PATH).
pub(crate) fn find(name: &str) -> Option<PathBuf> {
    if let Some(p) = on_path(name) {
        return Some(p);
    }
    let mut known: Vec<PathBuf> = Vec::new();
    if cfg!(windows) {
        let pf = env_dir("ProgramFiles").unwrap_or_else(|| PathBuf::from(r"C:\Program Files"));
        let sys = env_dir("SystemRoot").unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
        match name {
            "gh" => {
                known.push(pf.join("GitHub CLI").join("gh.exe"));
                known
                    .extend(env_dir("LOCALAPPDATA").map(|l| l.join(r"Programs\GitHub CLI\gh.exe")));
            }
            "tailscale" => known.push(pf.join("Tailscale").join("tailscale.exe")),
            _ => known.push(sys.join(r"System32\OpenSSH").join(format!("{name}.exe"))),
        }
    } else {
        if cfg!(target_os = "macos") && name == "tailscale" {
            known.push("/Applications/Tailscale.app/Contents/MacOS/Tailscale".into());
        }
        for dir in [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/usr/bin",
            "/home/linuxbrew/.linuxbrew/bin",
        ] {
            known.push(PathBuf::from(dir).join(name));
        }
    }
    known.into_iter().find(|p| p.is_file())
}
