use serde::{Deserialize, Serialize};

pub(super) const VERSION: u32 = 1;
pub(super) const MAX_REQUEST: u64 = 64 * 1024;
pub(super) const MAX_RESPONSE: u64 = 1024 * 1024;
pub(super) const MAX_DIFF: usize = 48 * 1024;

pub(super) fn diagnostic(value: impl ToString) -> String {
    let mut text = value.to_string();
    if text.len() > 2048 {
        let mut end = 2048;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str(" [diagnostic truncated]");
    }
    text
}

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
    Callers(crate::query::Request),
    Callees(crate::query::Request),
    Impact(crate::query::Request),
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
    Review {
        name: String,
        baseline: String,
        baseline_generation: u64,
        scope: crate::review::Scope,
    },
    Acknowledge {
        name: String,
        token: String,
    },
    ForgetReview {
        name: String,
    },
}

#[derive(Clone, Serialize)]
pub(super) struct Progress {
    pub phase: &'static str,
    pub files_read: usize,
    pub files_analyzed: usize,
    pub total_files: Option<usize>,
}

impl Default for Progress {
    fn default() -> Self {
        Self {
            phase: "discovering",
            files_read: 0,
            files_analyzed: 0,
            total_files: None,
        }
    }
}

#[derive(Serialize)]
pub(super) struct Response {
    pub version: u32,
    pub instance: String,
    pub root: String,
    pub generation: Option<u64>,
    pub index_complete: bool,
    pub watching: bool,
    pub recovery_needed: bool,
    pub watcher_error: Option<String>,
    pub files: usize,
    pub files_parsed: usize,
    pub analysis_issue_count: usize,
    pub analysis_issues: Vec<String>,
    pub progress: Option<Progress>,
    pub last_update_error: Option<String>,
    pub checkpoint_generation: Option<u64>,
    pub diff: Option<String>,
    pub graph: Option<crate::query::Output>,
    pub review: Option<crate::review::Output>,
    pub review_name: Option<String>,
    pub review_token: Option<String>,
    pub review_initialized: bool,
    pub truncated: bool,
    pub error: Option<String>,
}
