//! Persistent seal shared by publication, operational readers and reclamation.

use serde::{Deserialize, Serialize};

pub(super) const SEAL_FORMAT: u32 = 1;

#[derive(Deserialize, Serialize)]
pub(super) struct GenerationSeal {
    pub(super) format: u32,
    pub(super) installation_epoch: [u8; 16],
    pub(super) project_id: i64,
    pub(super) generation_id: i64,
    pub(super) content_revision: i64,
    pub(super) metadata_schema: u32,
    pub(super) base_id: Option<i64>,
    pub(super) base_revision: i64,
    pub(super) plan: String,
    pub(super) index_recipe: String,
    pub(super) files: Vec<(String, u64)>,
}
