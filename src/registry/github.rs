//! The only place that runs `gh`.

use crate::host::{paths, process, vlog};
use crate::model::{RegistryConfig, Release, RepoFile};
use serde::Deserialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const PARALLEL: usize = 8;

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            s.push(if i <= c.len() {
                T[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    s
}

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
        let mut c = process::command(&self.gh);
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

    /// `gh api -X <method> <endpoint> --input -` with a JSON body.
    fn send(
        &self,
        method: &str,
        endpoint: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let endpoint = format!("repos/{}/{endpoint}", self.repo);
        let mut c = self.api(None, &endpoint);
        c.args(["-X", method, "--input", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = c.spawn().map_err(|e| format!("gh: {e}"))?;
        if let Some(mut w) = child.stdin.take() {
            w.write_all(&serde_json::to_vec(body).unwrap_or_default())
                .map_err(|e| e.to_string())?;
        }
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if !out.status.success() {
            let msg = String::from_utf8_lossy(&out.stderr).to_string()
                + &String::from_utf8_lossy(&out.stdout);
            return Err(format!("gh api {endpoint}: {}", msg.trim()));
        }
        serde_json::from_slice(&out.stdout).map_err(|e| format!("gh api {endpoint}: {e}"))
    }

    /// The signed-in GitHub user.
    pub(crate) fn login(&self) -> Result<String, String> {
        let out = self.run(self.api(None, "user"), "user")?;
        let v: serde_json::Value = serde_json::from_slice(&out).map_err(|e| e.to_string())?;
        v["login"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| "gh: not logged in (run `gh auth login`)".into())
    }

    /// The repo's default branch; None if the repo doesn't exist.
    pub(crate) fn default_branch(&self) -> Result<Option<String>, String> {
        let endpoint = format!("repos/{}", self.repo);
        match self.run(self.api(None, &endpoint), &endpoint) {
            Ok(out) => {
                let v: serde_json::Value =
                    serde_json::from_slice(&out).map_err(|e| e.to_string())?;
                Ok(v["default_branch"].as_str().map(String::from))
            }
            Err(e) if e.contains("Not Found") || e.contains("404") => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub(crate) fn create(&self, public: bool) -> Result<(), String> {
        let vis = if public { "--public" } else { "--private" };
        let out = process::command(&self.gh)
            .args(["repo", "create", &self.repo, vis])
            .output()
            .map_err(|e| format!("gh: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "gh repo create: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }

    /// The repo's releases (drafts excluded), newest first.
    pub(crate) fn releases(&self) -> Result<Vec<Release>, String> {
        let endpoint = format!("repos/{}/releases?per_page=30", self.repo);
        let out = self.run(self.api(None, &endpoint), &endpoint)?;
        let v: serde_json::Value = serde_json::from_slice(&out).map_err(|e| e.to_string())?;
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .filter(|r| !r["draft"].as_bool().unwrap_or(false))
            .filter_map(|r| {
                let tag = r["tag_name"].as_str()?.to_string();
                Some(Release {
                    tag,
                    prerelease: r["prerelease"].as_bool().unwrap_or(false),
                })
            })
            .collect())
    }

    /// Download release assets `names` of `tag` into `dir`.
    pub(crate) fn download(&self, tag: &str, names: &[String], dir: &Path) -> Result<(), String> {
        vlog!("gh release download {tag} {}", names.join(" "));
        let mut c = process::command(&self.gh);
        c.args([
            "release",
            "download",
            tag,
            "-R",
            &self.repo,
            "--clobber",
            "-D",
        ])
        .arg(dir);
        for n in names {
            c.args(["-p", n]);
        }
        let out = c.output().map_err(|e| format!("gh: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "gh release download {tag}: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        Ok(())
    }

    /// Head commit, None for an empty repo.
    pub(crate) fn head_opt(&self) -> Result<Option<String>, String> {
        match self.head() {
            Ok(h) => Ok(Some(h)),
            Err(e) if e.contains("empty") || e.contains("409") => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Write files (None = delete) under the mesh root in one commit on the branch.
    pub(crate) fn commit(
        &self,
        files: &[(String, Option<Vec<u8>>)],
        message: &str,
    ) -> Result<(), String> {
        let Some(head) = self.head_opt()? else {
            // The git data API refuses empty repos; the first file goes through the contents API.
            let Some((path, Some(data))) = files.first() else {
                return Err("nothing to commit".into());
            };
            let body = serde_json::json!({ "message": message, "content": base64(data), "branch": self.branch });
            self.send("PUT", &format!("contents/{}{path}", self.prefix), &body)?;
            return if files.len() > 1 {
                self.commit(&files[1..], message)
            } else {
                Ok(())
            };
        };
        let endpoint = format!("repos/{}/git/commits/{head}", self.repo);
        let base: serde_json::Value =
            serde_json::from_slice(&self.run(self.api(None, &endpoint), &endpoint)?)
                .map_err(|e| format!("gh api {endpoint}: {e}"))?;
        let mut entries = Vec::new();
        for (path, data) in files {
            let path = format!("{}{path}", self.prefix);
            entries.push(match data {
                None => serde_json::json!({ "path": path, "mode": "100644", "type": "blob", "sha": null }),
                Some(d) => match std::str::from_utf8(d) {
                    Ok(s) => serde_json::json!({ "path": path, "mode": "100644", "type": "blob", "content": s }),
                    Err(_) => {
                        let blob = self.send("POST", "git/blobs", &serde_json::json!({ "content": base64(d), "encoding": "base64" }))?;
                        serde_json::json!({ "path": path, "mode": "100644", "type": "blob", "sha": blob["sha"] })
                    }
                },
            });
        }
        let tree = self.send(
            "POST",
            "git/trees",
            &serde_json::json!({ "base_tree": base["tree"]["sha"], "tree": entries }),
        )?;
        let commit = self.send(
            "POST",
            "git/commits",
            &serde_json::json!({ "message": message, "tree": tree["sha"], "parents": [head] }),
        )?;
        self.send(
            "PATCH",
            &format!("git/refs/heads/{}", self.branch),
            &serde_json::json!({ "sha": commit["sha"] }),
        )?;
        Ok(())
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
