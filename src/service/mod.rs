//! Experimental Unix repository service. See `docs/service.md` for protocol v1.
//!
//! Filesystem reads, transport, and publication live here, outside the semantic core.
//! A single writer builds updates while bounded connection workers query immutable generations.

mod protocol;
mod review;
#[cfg(test)]
mod tests;
mod unix;
mod watch;

pub use unix::{endpoint, request, serve};

use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use rayon::prelude::*;

use crate::{Repository, analyze, discover, query, repository};
use protocol::{Operation, Response};

struct Generation {
    number: u64,
    snapshot: repository::Snapshot,
    files: usize,
    issue_count: usize,
    issues: Vec<String>,
}

struct CachedDiff {
    // Generation numbers are unique within this owner. Service diffs always include locations.
    key: (u64, u64, bool), // before, after, diagnostics
    text: String,
    truncated: bool,
}

#[derive(Default)]
struct State {
    current: Option<Arc<Generation>>,
    checkpoints: BTreeMap<String, Arc<Generation>>,
    reviews: BTreeMap<String, review::Checkpoint>,
    next_review: u64,
    diffs: Vec<CachedDiff>,
    progress: Option<protocol::Progress>,
    last_error: Option<String>,
    files_parsed: usize,
}

struct Shared {
    root: PathBuf,
    instance: String,
    state: Mutex<State>,
    watch: Arc<Mutex<watch::Pending>>,
}

impl Shared {
    fn response(&self, state: &State) -> Response {
        let current = state.current.as_ref();
        let watch = self.watch.lock().expect("watch state poisoned");
        Response {
            version: protocol::VERSION,
            instance: self.instance.clone(),
            root: self.root.display().to_string(),
            generation: current.map(|value| value.number),
            index_complete: current.is_some(),
            watching: watch.watching,
            recovery_needed: watch.recovery_needed,
            watcher_error: watch.error.as_ref().map(protocol::diagnostic),
            files: current.map_or(0, |value| value.files),
            files_parsed: state.files_parsed,
            analysis_issue_count: current.map_or(0, |value| value.issue_count),
            analysis_issues: current.map_or_else(Vec::new, |value| value.issues.clone()),
            progress: state.progress.clone(),
            last_update_error: state.last_error.clone(),
            checkpoint_generation: None,
            diff: None,
            graph: None,
            review: None,
            review_name: None,
            review_token: None,
            review_initialized: false,
            truncated: false,
            error: None,
        }
    }

    fn failure(&self, error: impl ToString) -> Response {
        let mut response = self.response(&self.state.lock().expect("service state poisoned"));
        response.error = Some(protocol::diagnostic(error));
        response
    }
}

struct Update {
    // None rescans the worktree; Some updates only the named source paths.
    paths: Option<Vec<String>>,
    reply: UnixStream,
}

fn query(shared: &Shared, operation: Operation) -> Response {
    if matches!(
        operation,
        Operation::Review { .. } | Operation::Acknowledge { .. } | Operation::ForgetReview { .. }
    ) {
        return review::run(shared, operation);
    }
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
    let direction = match &operation {
        Operation::Callers(_) => Some(query::Direction::Callers),
        Operation::Callees(_) => Some(query::Direction::Callees),
        Operation::Impact(_) => Some(query::Direction::Impact),
        _ => None,
    };
    match operation {
        Operation::Callers(request) | Operation::Callees(request) | Operation::Impact(request) => {
            drop(state);
            match query::run(
                &current.snapshot.graph,
                request,
                direction.expect("graph query direction"),
            ) {
                Ok(output) => {
                    response.truncated = output.truncated;
                    response.graph = Some(output);
                }
                Err(error) => response.error = Some(error),
            }
        }
        Operation::Checkpoint { name } => {
            if name.is_empty() || name.len() > 256 {
                response.error = Some("checkpoint names must contain 1–256 bytes".to_owned());
            } else if state.checkpoints.len() + 2 * state.reviews.len() >= 64
                && !state.checkpoints.contains_key(&name)
            {
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
            let key = (before.number, current.number, diagnostics);
            if let Some(cached) = state.diffs.iter().find(|entry| entry.key == key) {
                response.diff = Some(cached.text.clone());
                response.truncated = cached.truncated;
                return response; // Response headers above describe current health, not cached health.
            }
            drop(state); // Expensive comparisons must not block publication or other readers.
            match before
                .snapshot
                .compare(&current.snapshot, diagnostics, true)
            {
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
                    if before.number != current.number {
                        let mut state = shared.state.lock().expect("service state poisoned");
                        // Four bounded payloads, FIFO; no snapshots are retained. Concurrent misses
                        // may compute independently, but cannot create duplicate cache entries.
                        if !state.diffs.iter().any(|entry| entry.key == key) {
                            if state.diffs.len() == 4 {
                                state.diffs.remove(0);
                            }
                            state.diffs.push(CachedDiff {
                                key,
                                text: output.clone(),
                                truncated: response.truncated,
                            });
                        }
                    }
                    response.diff = Some(output);
                }
                Err(error) => response.error = Some(protocol::diagnostic(error)),
            }
        }
        _ => unreachable!("mutation requests go to the update worker"),
    }
    response
}

fn worker(
    shared: &Shared,
    mut repo: Repository,
    updates: mpsc::Receiver<Update>,
    pool: rayon::ThreadPool,
    watch_enabled: bool,
) {
    // The callback only buffers invalidations; all filesystem reconciliation stays here.
    let mut watcher = None;
    let mut maintenance = Instant::now();
    let mut retry = Instant::now();
    if watch_enabled {
        watcher = start_watch(shared);
    }
    refresh(shared, &mut repo, &pool, None);
    loop {
        if watch_enabled && maintenance.elapsed() >= Duration::from_secs(30) {
            // Also catches external/global ignore configuration changes and silent event loss.
            if !shared.watch.lock().expect("watch state poisoned").watching {
                drop(watcher.take());
                watcher = start_watch(shared);
            }
            shared
                .watch
                .lock()
                .expect("watch state poisoned")
                .recover(None);
            maintenance = Instant::now();
        }
        if watch_enabled && Instant::now() >= retry {
            let pending = shared.watch.lock().expect("watch state poisoned").take();
            if let Some(paths) = pending {
                let response = refresh(shared, &mut repo, &pool, paths);
                if response.error.is_some() {
                    shared
                        .watch
                        .lock()
                        .expect("watch state poisoned")
                        .recover(None);
                    retry = Instant::now() + Duration::from_secs(1);
                }
            }
        }
        match updates.recv_timeout(Duration::from_millis(20)) {
            Ok(update) => {
                let response = refresh(shared, &mut repo, &pool, update.paths);
                let _ = unix::reply(update.reply, &response);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn start_watch(shared: &Shared) -> Option<watch::Watcher> {
    match watch::start(shared.root.clone(), shared.watch.clone()) {
        Ok(watcher) => Some(watcher),
        Err(error) => {
            let mut watch = shared.watch.lock().expect("watch state poisoned");
            watch.watching = false;
            watch.recover(Some(error.to_string()));
            None
        }
    }
}

fn refresh(
    shared: &Shared,
    repo: &mut Repository,
    pool: &rayon::ThreadPool,
    paths: Option<Vec<String>>,
) -> Response {
    let epoch = shared.watch.lock().expect("watch state poisoned").epoch();
    let previous = {
        let mut state = shared.state.lock().expect("service state poisoned");
        state.progress = Some(protocol::Progress::default());
        state.current.clone()
    };
    let rescan = paths.is_none();
    let result = load(shared, repo, pool, previous.as_deref(), paths);
    let mut state = shared.state.lock().expect("service state poisoned");
    state.progress = None;
    let error = match result {
        Ok(generation) => {
            if let Some(generation) = generation {
                state.current = Some(Arc::new(generation));
            }
            if rescan {
                state.last_error = None;
                shared
                    .watch
                    .lock()
                    .expect("watch state poisoned")
                    .reconciled(epoch);
            }
            None
        }
        Err(error) => {
            let error = protocol::diagnostic(error);
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
    pool: &rayon::ThreadPool,
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
    let empty = repository::Snapshot::default();
    let snapshot = previous.map_or(&empty, |generation| &generation.snapshot);
    // Only cached paths can be deleted. Unchanged prepared files cancel these tombstones.
    let cached = snapshot.paths().collect::<std::collections::BTreeSet<_>>();
    changes.retain(|path, _| cached.contains(path));
    shared
        .state
        .lock()
        .expect("service state poisoned")
        .progress = Some(protocol::Progress {
        phase: "indexing",
        files_read: 0,
        files_analyzed: 0,
        total_files: Some(files.len()),
    });
    let prepared = pool.install(|| {
        files
            .par_iter()
            .map_init(analyze::Analyzers::new, |analyzers, path| {
                let relative =
                    discover::relative(&shared.root, path).map_err(|error| error.to_string())?;
                // At most 8 MiB (+ one overflow byte) per active job, not all batch sources in RAM.
                const MAX_SOURCE: u64 = 8 * 1024 * 1024;
                let mut bytes = Vec::new();
                fs::File::open(path)
                    .and_then(|file| file.take(MAX_SOURCE + 1).read_to_end(&mut bytes))
                    .map_err(|error| format!("reading {relative}: {error}"))?;
                if bytes.len() as u64 > MAX_SOURCE {
                    return Err(format!("source exceeds 8 MiB: {relative}"));
                }
                shared
                    .state
                    .lock()
                    .expect("service state poisoned")
                    .progress
                    .as_mut()
                    .unwrap()
                    .files_read += 1;
                let analyzers = analyzers.as_mut().map_err(|error| error.to_string())?;
                let file = snapshot
                    .prepare(&relative, &bytes, analyzers)
                    .map_err(|error| error.to_string())?;
                let mut state = shared.state.lock().expect("service state poisoned");
                state.progress.as_mut().unwrap().files_analyzed += 1;
                if file.is_some() {
                    state.files_parsed += 1;
                }
                Ok((relative, file))
            })
            .collect::<Vec<Result<_, String>>>()
    });
    // Collect in sorted input order so concurrent failures and diagnostics are deterministic.
    for result in prepared {
        let (path, file) = result?;
        if file.is_some() {
            changes.insert(path, file);
        } else {
            changes.remove(&path);
        }
    }
    shared
        .state
        .lock()
        .expect("service state poisoned")
        .progress
        .as_mut()
        .unwrap()
        .phase = "resolving";
    let changed = repo.apply(changes).map_err(|error| error.to_string())?;
    if !changed && previous.is_some() {
        return Ok(None);
    }
    shared
        .state
        .lock()
        .expect("service state poisoned")
        .progress
        .as_mut()
        .unwrap()
        .phase = "publishing";
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
            .map(|issue| protocol::diagnostic(issue.message()))
            .collect(),
        snapshot,
    }))
}
