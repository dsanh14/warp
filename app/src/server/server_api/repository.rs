use cloud_object_models::{CodeForge, SourceRepo};
use serde::Deserialize;
use warp_cli::agent::RepositoryHeadRef;

/// Server-resolved repository checkout inputs.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct ResolvedRepository {
    pub forge: CodeForge,
    pub owner: String,
    pub name: String,
    #[serde(default, rename = "ref")]
    pub checkout: Option<RepositoryHeadRef>,
    #[serde(default)]
    pub clone_from: Option<SourceRepo>,
    #[serde(default)]
    pub preserve_origin: bool,
}

impl ResolvedRepository {
    pub(crate) fn source_repo(&self) -> SourceRepo {
        SourceRepo::new(self.forge, self.owner.clone(), self.name.clone())
    }
}
