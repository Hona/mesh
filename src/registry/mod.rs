//! Where the mesh lives. GitHub is the only backend.

mod github;

use crate::model::RegistryConfig;

pub(crate) type Registry = github::GitHub;

pub(crate) fn open(cfg: &RegistryConfig) -> Result<Registry, String> {
    match cfg.kind.as_str() {
        "github" => github::GitHub::open(cfg),
        kind => Err(format!("unknown registry kind {kind:?} in state.json")),
    }
}

/// The signed-in registry user (default mesh repo owner).
pub(crate) fn login() -> Result<String, String> {
    let cfg = RegistryConfig {
        kind: "github".into(),
        repo: String::new(),
        path: String::new(),
        branch: String::new(),
    };
    open(&cfg)?.login()
}
