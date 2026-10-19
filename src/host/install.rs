//! Keep the binaries at the SPEC §4.4 install path, and that path on the user PATH.

use super::paths;
use std::fs;
use std::path::{Path, PathBuf};

pub(crate) struct Installed {
    pub mesh: PathBuf,
    /// Windows only: the windowless build used by scheduled tasks.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub meshw: Option<PathBuf>,
    pub changed: bool,
}

pub(crate) fn dir() -> PathBuf {
    if cfg!(windows) {
        paths::env_dir("LOCALAPPDATA")
            .unwrap_or_else(|| paths::home().join(r"AppData\Local"))
            .join(r"Programs\mesh")
    } else {
        paths::home().join(".local/bin")
    }
}

fn same(a: &Path, b: &Path) -> bool {
    if let (Ok(x), Ok(y)) = (a.canonicalize(), b.canonicalize())
        && x == y
    {
        return true;
    }
    match (fs::metadata(a), fs::metadata(b)) {
        (Ok(x), Ok(y)) if x.len() == y.len() => fs::read(a).ok() == fs::read(b).ok(),
        _ => false,
    }
}

/// Installed file names: mesh (+ the windowless meshw on Windows).
pub(crate) fn names() -> &'static [&'static str] {
    if cfg!(windows) {
        &["mesh.exe", "meshw.exe"]
    } else {
        &["mesh"]
    }
}

/// Delete leftovers of earlier rename-asides (`*.old`); still-running ones stay until next time.
pub(crate) fn clean_old() {
    for e in fs::read_dir(dir()).into_iter().flatten().flatten() {
        if e.file_name().to_string_lossy().ends_with(".old") {
            let _ = fs::remove_file(e.path());
        }
    }
}

/// Version of the installed mesh (`mesh -V`).
pub(crate) fn installed_version() -> Option<String> {
    let out = super::process::command(dir().join(names()[0]))
        .arg("-V")
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    Some(s.split_whitespace().nth(1)?.to_string())
}

/// The installed mesh, if it is the same build as the running one (the mux runs from there so
/// it never locks a build output or download folder).
pub(crate) fn installed_copy() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let installed = dir().join(names()[0]);
    same(&exe, &installed).then_some(installed)
}

/// Copy `src` over `dst`; a running `dst` (Windows) is renamed aside first.
pub(crate) fn replace(src: &Path, dst: &Path) -> Result<(), String> {
    let name = dst
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let tmp = dst.with_file_name(format!("{name}.new"));
    fs::copy(src, &tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    if fs::rename(&tmp, dst).is_ok() {
        return Ok(());
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let aside = dst.with_file_name(format!("{name}.{stamp}.old"));
    fs::rename(dst, &aside).map_err(|e| format!("{}: {e}", dst.display()))?;
    fs::rename(&tmp, dst).map_err(|e| format!("{}: {e}", dst.display()))
}

/// Make sure the running build is installed; `check` only reports.
pub(crate) fn install(check: bool) -> Result<Installed, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let src_dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    let dir = dir();
    clean_old();
    let names = names();
    let mut changed = false;
    for (i, name) in names.iter().enumerate() {
        let src = if i == 0 {
            exe.clone()
        } else {
            src_dir.join(name)
        };
        let dst = dir.join(name);
        if !src.is_file() || same(&src, &dst) {
            continue;
        }
        changed = true;
        if !check {
            fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            replace(&src, &dst)?;
        }
    }
    let meshw = dir.join("meshw.exe");
    Ok(Installed {
        mesh: dir.join(names[0]),
        meshw: (cfg!(windows) && (meshw.is_file() || check)).then_some(meshw),
        changed,
    })
}

/// Expand `%VAR%` references (Windows registry PATH entries).
#[cfg(windows)]
fn expand(s: &str) -> String {
    let mut out = String::new();
    let mut parts = s.split('%');
    out += parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();
    let mut i = 0;
    while i < rest.len() {
        match (std::env::var(rest[i]), i + 1 < rest.len()) {
            (Ok(v), true) => {
                out += &v;
                out += rest[i + 1];
                i += 2;
            }
            _ => {
                out.push('%');
                out += rest[i];
                i += 1;
            }
        }
    }
    out
}

/// Add `dir` to the user PATH (HKCU\Environment). Some(where) if it was (or would be) added.
#[cfg(windows)]
pub(crate) fn ensure_user_path(dir: &Path, check: bool) -> Result<Option<String>, String> {
    use super::win;
    let (ty, cur) = win::reg_get(win::HKCU, "Environment", "Path")
        .unwrap_or((win::REG_EXPAND_SZ, String::new()));
    let want = dir.display().to_string();
    let norm = |p: &str| expand(p).trim_end_matches('\\').to_lowercase();
    if cur.split(';').any(|p| norm(p) == want.to_lowercase()) {
        return Ok(None);
    }
    if !check {
        let new = if cur.trim().is_empty() {
            want
        } else {
            format!("{};{want}", cur.trim_end_matches(';'))
        };
        if !win::reg_set(win::HKCU, "Environment", "Path", ty, &new) {
            return Err("can't write HKCU\\Environment\\Path".into());
        }
        win::broadcast_env_change();
    }
    Ok(Some("user PATH".into()))
}

/// Ends mesh's one line in a shell profile; `mesh leave` removes lines ending with it.
#[cfg(unix)]
const MARK: &str = "# opencode-mesh: PATH";

/// The profile of the user's shell (`$SHELL`) and the marked line putting `dir` on PATH there.
#[cfg(unix)]
fn profile(dir: &Path) -> (PathBuf, String) {
    let home = paths::home();
    let shell = std::env::var("SHELL").unwrap_or_default();
    let d = dir.display();
    let export = format!("export PATH=\"{d}:$PATH\" {MARK}");
    match shell.rsplit('/').next().unwrap_or("") {
        "zsh" => (home.join(".zshrc"), export),
        // macOS terminals start login shells
        "bash" if cfg!(target_os = "macos") => (home.join(".bash_profile"), export),
        "bash" => (home.join(".bashrc"), export),
        "fish" => (
            home.join(".config/fish/conf.d/opencode-mesh.fish"),
            format!("fish_add_path -g \"{d}\" {MARK}"),
        ),
        _ => (home.join(".profile"), export),
    }
}

/// If `dir` isn't on PATH, add one marked line to the shell profile. Some(where) if it was (or
/// would be) added; None if PATH has it or the line is already there.
#[cfg(unix)]
pub(crate) fn ensure_user_path(dir: &Path, check: bool) -> Result<Option<String>, String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    if std::env::split_paths(&path).any(|p| p == dir) {
        return Ok(None);
    }
    let (file, line) = profile(dir);
    let text = fs::read_to_string(&file).unwrap_or_default();
    if text.lines().any(|l| l.trim_end().ends_with(MARK)) {
        return Ok(None);
    }
    if !check {
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        let sep = if text.is_empty() || text.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        fs::write(&file, format!("{text}{sep}{line}\n"))
            .map_err(|e| format!("{}: {e}", file.display()))?;
    }
    let home = paths::home();
    let shown = file.strip_prefix(&home).map_or_else(
        |_| file.display().to_string(),
        |rel| format!("~/{}", rel.display()),
    );
    Ok(Some(shown))
}

/// `mesh leave`: drop the marked line from every profile `ensure_user_path` may have used
/// (`$SHELL` may have changed since); every other byte stays. Ok(true) if one was there.
#[cfg(unix)]
pub(crate) fn remove_profile_line() -> Result<bool, String> {
    let home = paths::home();
    let mut removed = false;
    for name in [".zshrc", ".bashrc", ".bash_profile", ".profile"] {
        let file = home.join(name);
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        let ours = |l: &str| l.trim_end().ends_with(MARK);
        if !text.lines().any(ours) {
            continue;
        }
        let kept: String = text.split_inclusive('\n').filter(|l| !ours(l)).collect();
        fs::write(&file, kept).map_err(|e| format!("{}: {e}", file.display()))?;
        removed = true;
    }
    let fish = home.join(".config/fish/conf.d/opencode-mesh.fish");
    if fish.is_file() {
        fs::remove_file(&fish).map_err(|e| format!("{}: {e}", fish.display()))?;
        removed = true;
    }
    Ok(removed)
}
