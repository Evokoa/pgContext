use super::BuildJobStatus;
use crate::domain_types::ArtifactKind;

#[derive(Debug, Clone)]
pub(super) struct BuildJobRow {
    pub(super) build_job_id: i64,
    pub(super) collection_name: String,
    pub(super) artifact_kind: ArtifactKind,
    pub(super) artifact_name: String,
    pub(super) target_name: String,
    pub(super) stored_status: BuildJobStatus,
    pub(super) backend_pid: Option<i32>,
    pub(super) backend_identity: Option<String>,
    pub(super) attempt: i32,
    pub(super) total_units: i64,
    pub(super) processed_units: i64,
    pub(super) last_source_point_id: i64,
    pub(super) cancel_requested: bool,
    pub(super) error_message: Option<String>,
}

pub(super) struct ActiveBuildJobCandidate {
    pub(super) build_job_id: i64,
    pub(super) stored_status: BuildJobStatus,
    pub(super) backend_pid: Option<i32>,
    pub(super) backend_identity: Option<String>,
}
