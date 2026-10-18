//! The only place that runs `gh`.

use crate::host::{paths, vlog};
use crate::model::{RegistryConfig, RepoFile};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const PARALLEL: usize = 8;

pub(crate) struct GitHub {
    gh: PathBuf,
    repo: String,
    /// Mesh root inside the repo: "" or "dir/".
    prefix: String,
    branch: String,
}

#[derive(Deserialize)]
struct Tree {
    tree: Vec<TreeEntry>,
    #[serde(default)]
    truncated: bool,
}

#[derive(Deserialize)]
struct TreeEntry {
    path: String,
    mode: String,
    #[serde(rename = "type")]
    kind: String,
    sha: String,
}

impl GitHub {
    pub(crate) fn open(cfg: &RegistryConfig) -> Result<GitHub, String> {
        let known = if cfg!(windows) {
            let mut k = vec![paths::program_files().join(r"GitHub CLI\gh.exe")];
            k.extend(paths::env_dir("LOCALAPPDATA").map(|l| l.join(r"Programs\GitHub CLI\gh.exe")));
            k
        } else {
            paths::unix_bin_dirs("gh")
        };
        let gh = paths::find("gh", &known)
            .ok_or("gh not found: install GitHub CLI, then run `gh auth login`")?;
        vlog!("gh: {}", gh.display());
        let path = cfg.path.trim_matches('/');
        let prefix = if path.is_empty() {
            String::new()
        } else {
            format!("{path}/")
        };
        Ok(GitHub {
            gh,
            repo: cfg.repo.clone(),
            prefix,
            branch: cfg.branch.clone(),
        })
    }

    fn api(&self, accept: Option<&str>, endpoint: &str) -> Command {
        vlog!("gh api {endpoint}");
        let mut c = Command::new(&self.gh);
        c.arg("api");
        if let Some(a) = accept {
            c.args(["-H", &format!("Accept: {a}")]);
        }
        c.arg(endpoint);
        c
    }

    fn run(&self, mut c: Command, endpoint: &str) -> Result<Vec<u8>, String> {
        let out = c.output().map_err(|e| format!("gh: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "gh api {endpoint}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(out.stdout)
    }

    /// Commit at the tip of the branch.
    pub(crate) fn head(&self) -> Result<String, String> {
        let endpoint = format!("repos/{}/commits/{}", self.repo, self.branch);
        let out = self.run(
            self.api(Some("application/vnd.github.sha"), &endpoint),
            &endpoint,
        )?;
        let sha = String::from_utf8_lossy(&out).trim().to_string();
        if sha.len() != 40 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(format!("gh api {endpoint}: unexpected reply {sha:?}"));
        }
        Ok(sha)
    }

    /// Every regular file under the mesh root at `commit`, in one call.
    pub(crate) fn list(&self, commit: &str) -> Result<Vec<RepoFile>, String> {
        let endpoint = format!("repos/{}/git/trees/{commit}?recursive=1", self.repo);
        let out = self.run(self.api(None, &endpoint), &endpoint)?;
        let tree: Tree =
            serde_json::from_slice(&out).map_err(|e| format!("gh api {endpoint}: {e}"))?;
        if tree.truncated {
            return Err(format!(
                "{}: tree too large, GitHub truncated the listing",
                self.repo
            ));
        }
        Ok(tree
            .tree
            .into_iter()
            .filter(|e| e.kind == "blob" && e.mode != "120000")
            .filter_map(|e| {
                let path = e.path.strip_prefix(&self.prefix)?.to_string();
                Some(RepoFile {
                    path,
                    id: e.sha,
                    exec: e.mode == "100755",
                })
            })
            .collect())
    }

    /// Download files in parallel, each streamed straight to its destination.
    pub(crate) fn fetch(&self, jobs: &[(&RepoFile, PathBuf)]) -> Result<(), String> {
        let next = AtomicUsize::new(0);
        std::thread::scope(|s| {
            let workers: Vec<_> = (0..jobs.len().min(PARALLEL))
                .map(|_| {
                    s.spawn(|| {
                        while let Some((file, dest)) =
                            jobs.get(next.fetch_add(1, Ordering::Relaxed))
                        {
                            self.blob(file, dest)?;
                        }
                        Ok(())
                    })
                })
                .collect();
            workers.into_iter().try_for_each(|w| {
                w.join()
                    .unwrap_or_else(|_| Err("download thread panicked".into()))
            })
        })
    }

    fn blob(&self, file: &RepoFile, dest: &Path) -> Result<(), String> {
        let io = |p: &Path, e: std::io::Error| format!("{}: {e}", p.display());
        if let Some(dir) = dest.parent() {
            std::fs::create_dir_all(dir).map_err(|e| io(dir, e))?;
        }
        let mut tmp = dest.as_os_str().to_owned();
        tmp.push(".part");
        let tmp = PathBuf::from(tmp);
        let out_file = std::fs::File::create(&tmp).map_err(|e| io(&tmp, e))?;
        let endpoint = format!("repos/{}/git/blobs/{}", self.repo, file.id);
        let mut c = self.api(Some("application/vnd.github.raw"), &endpoint);
        c.stdout(out_file).stderr(Stdio::piped());
        if let Err(e) = self.run(c, &endpoint) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        if file.exec {
            make_executable(&tmp).map_err(|e| io(&tmp, e))?;
        }
        std::fs::rename(&tmp, dest).map_err(|e| io(dest, e))
    }
}

#[cfg(unix)]
fn make_executable(p: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn make_executable(_: &Path) -> std::io::Result<()> {
    Ok(())
}
