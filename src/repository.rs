#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Arc;

use crate::{Error, analyze, diff, graph, model, render};

#[derive(Clone, Debug, Eq, PartialEq)]
struct Key {
    version: u32,
    syntax: model::Syntax,
    hash: blake3::Hash,
}

#[derive(Clone)]
pub(crate) struct File {
    key: Key,
    analysis: Result<model::FileArtifact, analyze::Issue>,
}

/// An in-memory repository updated exclusively with caller-supplied source bytes.
///
/// Paths must be normalized, repository-relative UTF-8 paths using `/`, without
/// empty, `.` or `..` components, backslashes, or colons. Unsupported extensions
/// and declaration-only TypeScript files are rejected. No method reads files.
/// Updates reparse only the changed file, then rebuild resolution and all graph
/// edges from cached artifacts. Checkpoints share immutable artifacts, diagnostics and graph.
///
/// ```
/// let mut repo = stackscout::Repository::new()?;
/// repo.change("lib.rs", b"pub fn run() {}")?;
/// repo.checkpoint("session");
/// repo.change("lib.rs", b"pub fn run() { audit(); } fn audit() {}")?;
/// assert!(repo.diff("session")?.contains("audit"));
/// # Ok::<(), stackscout::Error>(())
/// ```
pub struct Repository {
    analyzers: analyze::Analyzers,
    current: Snapshot,
    checkpoints: BTreeMap<String, Snapshot>,
}

impl Repository {
    /// Creates an empty repository and initializes its parsers.
    pub fn new() -> Result<Self, Error> {
        Ok(Self {
            analyzers: analyze::Analyzers::new()?,
            current: Snapshot::default(),
            checkpoints: BTreeMap::new(),
        })
    }

    /// Adds or replaces one file. Returns false for identical content and parser
    /// identity, without parsing or rebuilding the graph (including cached failures).
    /// Syntax failures replace the file's old evidence with a diagnostic. Invalid
    /// paths, non-UTF-8 bytes and graph-construction errors leave the state unchanged.
    pub fn change(&mut self, path: &str, bytes: &[u8]) -> Result<bool, Error> {
        let Some(file) = self.file(path, bytes)? else {
            return Ok(false);
        };
        self.current.replace(path, Some(file))?;
        Ok(true)
    }

    fn file(&mut self, path: &str, bytes: &[u8]) -> Result<Option<File>, Error> {
        self.current.prepare(path, bytes, &mut self.analyzers)
    }

    // Stage a deduplicated batch, then resolve once and commit all or nothing.
    #[cfg(test)]
    pub(crate) fn update(
        &mut self,
        changes: &BTreeMap<String, Option<Vec<u8>>>,
    ) -> Result<bool, Error> {
        let mut updates = BTreeMap::new();
        for (path, bytes) in changes {
            let file = if let Some(bytes) = bytes {
                let Some(file) = self.file(path, bytes)? else {
                    continue;
                };
                Some(file)
            } else {
                source_path(path)?;
                if !self.current.files.contains_key(path) {
                    continue;
                }
                None
            };
            updates.insert(path.clone(), file);
        }
        self.apply(updates)
    }

    // Prepared artifacts are plain data; I/O and parallel scheduling belong to the caller.
    #[cfg(any(unix, test))]
    pub(crate) fn apply(&mut self, updates: BTreeMap<String, Option<File>>) -> Result<bool, Error> {
        if updates.is_empty() {
            return Ok(false);
        }
        let mut files = (*self.current.files).clone();
        for (path, file) in updates {
            if let Some(file) = file {
                files.insert(path, Arc::new(file));
            } else {
                files.remove(&path);
            }
        }
        let graph = crate::build(
            files
                .values()
                .filter_map(|file| file.analysis.as_ref().ok()),
        )?;
        self.current = Snapshot {
            files: Arc::new(files),
            graph: Arc::new(graph),
        };
        Ok(true)
    }

    #[cfg(any(unix, test))]
    pub(crate) fn snapshot(&self) -> Snapshot {
        self.current.clone()
    }

    /// Removes a file and its diagnostic, if any. Returns false if absent. An
    /// invalid path or graph-construction error leaves the state unchanged.
    pub fn delete(&mut self, path: &str) -> Result<bool, Error> {
        source_path(path)?;
        if !self.current.files.contains_key(path) {
            return Ok(false);
        }
        self.current.replace(path, None)?;
        Ok(true)
    }

    /// Saves the current state under a name, replacing that name's prior snapshot.
    /// Other checkpoints remain unchanged by subsequent updates or deletions.
    pub fn checkpoint(&mut self, name: &str) {
        self.checkpoints
            .insert(name.to_owned(), self.current.clone());
    }

    /// Compares a named checkpoint with current state using directory-diff semantics.
    /// Returns an error for an unknown checkpoint.
    pub fn diff(&self, name: &str) -> Result<String, Error> {
        self.compare(name, false)
    }

    /// Like [`Self::diff`], but includes detailed unchanged analysis diagnostics.
    pub fn diff_with_diagnostics(&self, name: &str) -> Result<String, Error> {
        self.compare(name, true)
    }

    fn compare(&self, name: &str, diagnostics: bool) -> Result<String, Error> {
        let before = self.checkpoints.get(name).ok_or_else(|| Error {
            message: format!("unknown checkpoint: {name}"),
        })?;
        before.compare(&self.current, diagnostics, false)
    }
}

pub(crate) fn source_path(path: &str) -> Result<model::Syntax, Error> {
    if path.contains(['\\', ':']) || path.split('/').any(|part| matches!(part, "" | "." | "..")) {
        return Err(Error {
            message: format!("expected a normalized repository-relative source path: {path}"),
        });
    }
    model::Syntax::for_path(Path::new(path)).ok_or_else(|| Error {
        message: format!("unsupported source path: {path}"),
    })
}

#[derive(Clone, Default)]
pub(crate) struct Snapshot {
    files: Arc<BTreeMap<String, Arc<File>>>,
    pub(crate) graph: Arc<graph::Graph>,
}

impl Snapshot {
    pub(crate) fn prepare(
        &self,
        path: &str,
        bytes: &[u8],
        analyzers: &mut analyze::Analyzers,
    ) -> Result<Option<File>, Error> {
        let key = Key {
            version: analyze::VERSION,
            syntax: source_path(path)?,
            hash: blake3::hash(bytes),
        };
        if self.files.get(path).is_some_and(|file| file.key == key) {
            return Ok(None);
        }
        let source = std::str::from_utf8(bytes).map_err(|error| Error {
            message: format!("invalid UTF-8 source in {path}: {error}"),
        })?;
        let analysis = analyzers
            .file(path, source)
            .map_err(|error| analyze::Issue::new(path.to_owned(), error));
        Ok(Some(File { key, analysis }))
    }

    #[cfg(any(unix, test))]
    pub(crate) fn paths(&self) -> impl Iterator<Item = &String> {
        self.files.keys()
    }

    #[cfg(unix)]
    pub(crate) fn issues(&self) -> impl Iterator<Item = &analyze::Issue> {
        self.files
            .values()
            .filter_map(|file| file.analysis.as_ref().err())
    }

    fn replace(&mut self, path: &str, file: Option<File>) -> Result<(), Error> {
        // Build before mutating state so fatal errors cannot publish a partial update.
        let artifacts = self
            .files
            .iter()
            .filter(|(existing, _)| existing.as_str() != path)
            .filter_map(|(_, file)| file.analysis.as_ref().ok())
            .chain(file.as_ref().and_then(|file| file.analysis.as_ref().ok()));
        let graph = crate::build(artifacts)?;
        if let Some(file) = file {
            Arc::make_mut(&mut self.files).insert(path.to_owned(), Arc::new(file));
        } else {
            Arc::make_mut(&mut self.files).remove(path);
        }
        self.graph = Arc::new(graph);
        Ok(())
    }

    pub(crate) fn compare(
        &self,
        after: &Self,
        diagnostics: bool,
        locations: bool,
    ) -> Result<String, Error> {
        let unchanged = Arc::ptr_eq(&self.graph, &after.graph);
        if unchanged && !diagnostics {
            return Ok(String::new());
        }
        let before_issues = self
            .files
            .values()
            .filter_map(|file| file.analysis.as_ref().err())
            .collect::<Vec<_>>();
        let after_issues = after
            .files
            .values()
            .filter_map(|file| file.analysis.as_ref().err())
            .collect::<Vec<_>>();
        let omitted = before_issues
            .iter()
            .chain(&after_issues)
            .map(|issue| issue.path.clone())
            .collect::<BTreeSet<_>>();
        // Identical publications need no graph walk, but unchanged diagnostics still matter.
        if unchanged {
            return Ok(crate::render_issues(
                &before_issues,
                &after_issues,
                &omitted,
                String::new(),
                diagnostics,
            ));
        }
        if omitted.is_empty() {
            return Ok(render::render_with_locations(
                &diff::compare(&self.graph, &after.graph),
                locations,
            ));
        }

        // A failed path must be omitted from BOTH graphs, including resolution.
        // Checkpoints retain artifacts so this never needs to read or parse sources.
        let before_graph = self.without(&omitted)?;
        let after_graph = after.without(&omitted)?;
        let output =
            render::render_with_locations(&diff::compare(&before_graph, &after_graph), locations);
        Ok(crate::render_issues(
            &before_issues,
            &after_issues,
            &omitted,
            output,
            diagnostics,
        ))
    }

    #[cfg(unix)]
    pub(crate) fn review(
        &self,
        after: &Self,
        scope: &crate::review::Scope,
    ) -> Result<crate::review::Output, Error> {
        let issues = self.issues().chain(after.issues()).collect::<Vec<_>>();
        let omitted = issues
            .iter()
            .map(|issue| issue.path.clone())
            .collect::<BTreeSet<_>>();
        if omitted.is_empty() {
            return Ok(crate::review::compare(&self.graph, &after.graph, scope));
        }
        // Preserve the diff contract: analysis failures are not symbol deletions/additions.
        let before_graph = self.without(&omitted)?;
        let after_graph = after.without(&omitted)?;
        let mut output = crate::review::compare(&before_graph, &after_graph, scope);
        output.truncated |= issues.len() > 32;
        output.analysis_issues = issues
            .iter()
            .take(32)
            .map(|issue| {
                let mut text = issue.message();
                if text.len() > 2048 {
                    let mut end = 2048;
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    text.truncate(end);
                    text.push_str(" [truncated]");
                }
                text
            })
            .collect();
        Ok(output)
    }

    fn without(&self, omitted: &BTreeSet<String>) -> Result<Arc<graph::Graph>, Error> {
        // Failed/missing files already contribute nothing to this graph. Re-resolve only
        // when the other snapshot's failures exclude a successfully analyzed artifact here.
        if !omitted.iter().any(|path| {
            self.files
                .get(path)
                .is_some_and(|file| file.analysis.is_ok())
        }) {
            return Ok(Arc::clone(&self.graph));
        }
        crate::build(self.files.iter().filter_map(|(path, file)| {
            if omitted.contains(path) {
                return None;
            }
            file.analysis.as_ref().ok()
        }))
        .map(Arc::new)
    }
}
