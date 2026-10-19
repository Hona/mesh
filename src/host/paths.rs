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
    on_path_in(name, None)
}

/// `name` on `path` (default: this process's PATH).
pub(crate) fn on_path_in(name: &str, path: Option<std::ffi::OsString>) -> Option<PathBuf> {
    let exts: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ".com"]
    } else {
        &[""]
    };
    let path = path.or_else(|| env::var_os("PATH"))?;
    env::split_paths(&path).find_map(|dir| {
        exts.iter()
            .map(|e| dir.join(format!("{name}{e}")))
            .find(|p| p.is_file())
    })
}

/// The shell mesh uses for `exec -c` (SPEC §4.2), decided once by `mesh up`: Windows pwsh →
/// powershell → cmd, first installed; unix the login shell. File checks only, no spawning.
pub(crate) fn detect_shell() -> (String, PathBuf) {
    let p = if cfg!(windows) {
        let sys = env_dir("SystemRoot")
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
            .join("System32");
        [
            on_path("pwsh"),
            Some(program_files().join(r"PowerShell\7\pwsh.exe")),
            Some(sys.join(r"WindowsPowerShell\v1.0\powershell.exe")),
        ]
        .into_iter()
        .flatten()
        .find(|p| p.is_file())
        .unwrap_or_else(|| sys.join("cmd.exe"))
    } else {
        env::var_os("SHELL")
            .map(PathBuf::from)
            .filter(|p| p.is_file())
            .unwrap_or_else(|| "/bin/sh".into())
    };
    (
        p.file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase(),
        p,
    )
}

/// SPEC names: (linux | macos | windows, x64 | arm64).
pub(crate) fn os_arch() -> (&'static str, &'static str) {
    let arch = match env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        a => a,
    };
    (env::consts::OS, arch)
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
