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

pub(crate) fn program_files() -> PathBuf {
    env_dir("ProgramFiles").unwrap_or_else(|| PathBuf::from(r"C:\Program Files"))
}

/// Usual macOS/Linux bin dirs that a desktop-app shell's PATH may lack.
pub(crate) fn unix_bin_dirs(name: &str) -> Vec<PathBuf> {
    let dirs = [
        "/opt/homebrew/bin",
        "/usr/local/bin",
        "/usr/bin",
        "/home/linuxbrew/.linuxbrew/bin",
    ];
    dirs.iter().map(|d| PathBuf::from(d).join(name)).collect()
}

/// A tool on PATH, else the first existing `known` install location
/// (desktop shells often have a stale PATH).
pub(crate) fn find(name: &str, known: &[PathBuf]) -> Option<PathBuf> {
    on_path(name).or_else(|| known.iter().find(|p| p.is_file()).cloned())
}
