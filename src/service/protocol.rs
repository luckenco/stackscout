use serde::{Deserialize, Serialize};

pub(super) const VERSION: u32 = 1;
pub(super) const MAX_REQUEST: u64 = 64 * 1024;
pub(super) const MAX_RESPONSE: u64 = 1024 * 1024;
pub(super) const MAX_DIFF: usize = 48 * 1024;

#[derive(Deserialize)]
pub(super) struct Request {
    pub version: u32,
    #[serde(flatten)]
    pub operation: Operation,
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum Operation {
    Status {},
    Sync {
        paths: Vec<String>,
    },
    Rescan {},
    Checkpoint {
        name: String,
    },
    Forget {
        name: String,
    },
    Diff {
        name: String,
        #[serde(default)]
        diagnostics: bool,
    },
}

#[derive(Clone, Default, Serialize)]
pub(super) struct Progress {
    pub files_read: usize,
    pub total_files: Option<usize>,
}

#[derive(Serialize)]
pub(super) struct Response {
    pub version: u32,
    pub instance: String,
    pub root: String,
    pub generation: Option<u64>,
    pub index_complete: bool,
    pub watching: bool,
    pub files: usize,
    pub analysis_issue_count: usize,
    pub analysis_issues: Vec<String>,
    pub progress: Option<Progress>,
    pub last_update_error: Option<String>,
    pub checkpoint_generation: Option<u64>,
    pub diff: Option<String>,
    pub truncated: bool,
    pub error: Option<String>,
}
