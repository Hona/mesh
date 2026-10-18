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
