//! Experimental Unix repository service. See `docs/service.md` for protocol v1.
//!
//! Filesystem reads, transport, and publication live here, outside the semantic core.
//! A single writer builds updates while bounded connection workers query immutable generations.

mod protocol;
mod unix;

pub use unix::{request, serve};

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};

use crate::{Repository, discover, repository};
use protocol::{Operation, Response};

struct Generation {
    number: u64,
    snapshot: repository::Snapshot,
    files: usize,
    issue_count: usize,
    issues: Vec<String>,
}

#[derive(Default)]
struct State {
    current: Option<Arc<Generation>>,
    checkpoints: BTreeMap<String, Arc<Generation>>,
    progress: Option<protocol::Progress>,
    last_error: Option<String>,
}

struct Shared {
    root: PathBuf,
    instance: String,
    state: Mutex<State>,
}

impl Shared {
    fn response(&self, state: &State) -> Response {
        let current = state.current.as_ref();
        Response {
            version: protocol::VERSION,
            instance: self.instance.clone(),
            root: self.root.display().to_string(),
            generation: current.map(|value| value.number),
            index_complete: current.is_some(),
            watching: false,
            files: current.map_or(0, |value| value.files),
            analysis_issue_count: current.map_or(0, |value| value.issue_count),
            analysis_issues: current.map_or_else(Vec::new, |value| value.issues.clone()),
            progress: state.progress.clone(),
            last_update_error: state.last_error.clone(),
            checkpoint_generation: None,
            diff: None,
            truncated: false,
            error: None,
        }
    }

    fn failure(&self, error: impl ToString) -> Response {
        let mut response = self.response(&self.state.lock().expect("service state poisoned"));
        response.error = Some(error.to_string());
        response
    }
}

struct Update {
    // None rescans the worktree; Some updates only the named source paths.
    paths: Option<Vec<String>>,
    reply: mpsc::SyncSender<Response>,
}

fn handle(shared: &Shared, updates: &mpsc::SyncSender<Update>, bytes: &[u8]) -> Response {
    let request: protocol::Request = match serde_json::from_slice(bytes) {
        Ok(request) => request,
        Err(error) => return shared.failure(format!("invalid request: {error}")),
    };
    if request.version != protocol::VERSION {
        return shared.failure(format!("unsupported protocol version: {}", request.version));
    }
    let paths = match request.operation {
        Operation::Sync { paths } => Some(paths),
        Operation::Rescan {} => None,
        operation => return query(shared, operation),
    };
    let (reply, result) = mpsc::sync_channel(1);
    if updates.try_send(Update { paths, reply }).is_err() {
        return shared.failure("update queue unavailable; retry the request");
    }
    result
        .recv()
        .unwrap_or_else(|_| shared.failure("update worker stopped"))
}

fn query(shared: &Shared, operation: Operation) -> Response {
    let mut state = shared.state.lock().expect("service state poisoned");
    let mut response = shared.response(&state);
    if matches!(operation, Operation::Status {}) {
        return response;
    }
    let Some(current) = state.current.clone() else {
        response.error =
            Some("initial index is not ready; inspect status or wait with sync([])".to_owned());
        return response;
    };
    match operation {
        Operation::Checkpoint { name } => {
            if name.is_empty() || name.len() > 256 {
                response.error = Some("checkpoint names must contain 1–256 bytes".to_owned());
            } else if state.checkpoints.len() >= 64 && !state.checkpoints.contains_key(&name) {
                response.error =
                    Some("checkpoint limit reached; forget unused checkpoints".to_owned());
            } else {
                // A reconnect must not silently replace the original baseline.
                let checkpoint = state.checkpoints.entry(name).or_insert(current);
                response.checkpoint_generation = Some(checkpoint.number);
            }
        }
        Operation::Forget { name } => {
            if let Some(checkpoint) = state.checkpoints.remove(&name) {
                response.checkpoint_generation = Some(checkpoint.number);
            } else {
                response.error = Some(format!("unknown checkpoint: {name}"));
            }
        }
        Operation::Diff { name, diagnostics } => {
            let Some(before) = state.checkpoints.get(&name).cloned() else {
                response.error = Some(format!("unknown checkpoint: {name}"));
                return response;
            };
            response.checkpoint_generation = Some(before.number);
            drop(state); // Expensive comparisons must not block publication or other readers.
            match before.snapshot.compare(&current.snapshot, diagnostics) {
                Ok(mut output) => {
                    response.truncated = output.lines().any(|line| {
                        line.get(2..)
                            .is_some_and(|text| text.trim() == "[truncated]")
                    });
                    if output.len() > protocol::MAX_DIFF {
                        let mut end = protocol::MAX_DIFF;
                        while !output.is_char_boundary(end) {
                            end -= 1;
                        }
                        output.truncate(end);
                        output.push_str("\n[truncated: diff exceeds 48 KiB]\n");
                        response.truncated = true;
                    }
                    response.diff = Some(output);
                }
                Err(error) => response.error = Some(error.to_string()),
            }
        }
        _ => unreachable!("mutation requests go to the update worker"),
    }
    response
}

fn worker(shared: &Shared, mut repo: Repository, updates: mpsc::Receiver<Update>) {
    refresh(shared, &mut repo, None);
    for update in updates {
        let response = refresh(shared, &mut repo, update.paths);
        let _ = update.reply.send(response);
    }
}

fn refresh(shared: &Shared, repo: &mut Repository, paths: Option<Vec<String>>) -> Response {
    let previous = {
        let mut state = shared.state.lock().expect("service state poisoned");
        state.progress = Some(protocol::Progress::default());
        state.current.clone()
    };
    let rescan = paths.is_none();
    let result = load(shared, repo, previous.as_deref(), paths);
    let mut state = shared.state.lock().expect("service state poisoned");
    state.progress = None;
    let error = match result {
        Ok(generation) => {
            if let Some(generation) = generation {
                state.current = Some(Arc::new(generation));
            }
            if rescan {
                state.last_error = None;
            }
            None
        }
        Err(error) => {
            state.last_error = Some(error.clone());
            Some(error)
        }
    };
    let mut response = shared.response(&state);
    response.error = error;
    response
}

fn load(
    shared: &Shared,
    repo: &mut Repository,
    previous: Option<&Generation>,
    paths: Option<Vec<String>>,
) -> Result<Option<Generation>, String> {
    if let Some(paths) = &paths {
        if previous.is_none() {
            return Err("initial index unavailable; use rescan before syncing paths".to_owned());
        }
        if paths.is_empty() {
            return Ok(None);
        }
    }
    let mut changes = BTreeMap::new();
    let selected = if let Some(paths) = paths {
        for path in paths {
            repository::source_path(&path).map_err(|error| error.to_string())?;
            changes.insert(path, None);
        }
        Some(changes.keys().map(|path| shared.root.join(path)).collect())
    } else {
        if let Some(previous) = previous {
            changes.extend(previous.snapshot.paths().map(|path| (path.clone(), None)));
        }
        None
    };
    let files = discover::selected(&shared.root, selected).map_err(|error| error.to_string())?;
    for (index, path) in files.iter().enumerate() {
        {
            let mut state = shared.state.lock().expect("service state poisoned");
            state.progress = Some(protocol::Progress {
                files_read: index,
                total_files: Some(files.len()),
            });
        }
        let relative = discover::relative(&shared.root, path).map_err(|error| error.to_string())?;
        let bytes = fs::read(path).map_err(|error| format!("reading {relative}: {error}"))?;
        changes.insert(relative, Some(bytes));
    }
    shared
        .state
        .lock()
        .expect("service state poisoned")
        .progress = Some(protocol::Progress {
        files_read: files.len(),
        total_files: Some(files.len()),
    });
    let changed = repo.update(&changes).map_err(|error| error.to_string())?;
    if !changed && previous.is_some() {
        return Ok(None);
    }
    let snapshot = repo.snapshot();
    Ok(Some(Generation {
        number: previous.map_or(1, |value| {
            value.number.checked_add(1).expect("generation exhausted")
        }),
        files: snapshot.paths().count(),
        issue_count: snapshot.issues().count(),
        issues: snapshot
            .issues()
            .take(32)
            .map(|issue| issue.message())
            .collect(),
        snapshot,
    }))
}
