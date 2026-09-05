mod analyze;
mod diff;
mod discover;
mod graph;
mod model;
mod render;
mod repository;
mod resolve;

pub use repository::Repository;

use std::collections::BTreeSet;
use std::path::Path;

use thiserror::Error;

/// A fatal error produced by source input, graph construction, or checkpoint lookup.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct Error {
    message: String,
}

impl From<analyze::AnalyzeError> for Error {
    fn from(error: analyze::AnalyzeError) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

impl From<discover::DiscoverError> for Error {
    fn from(error: discover::DiscoverError) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

impl From<graph::GraphError> for Error {
    fn from(error: graph::GraphError) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

/// Compares two source directories and returns a deterministic semantic graph diff.
///
/// The supported file extensions are `.rs`, `.ts`, `.tsx`, `.mts`, and `.cts`. If a file cannot
/// be analyzed in either directory, that path is omitted from both graphs and
/// introduced, resolved, and changed analysis issues produce concise `! incomplete`
/// diagnostics. Unchanged issues are hidden by default.
///
/// Callee bodies without changed reachable paths may be explicitly omitted from the output.
/// Labels containing control characters are quoted and escaped; call identity is unchanged.
///
/// ```
/// let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
///     .join("test/fixtures/typescript-basic");
/// let output = stackscout::diff_dirs(fixture.join("before"), fixture.join("after"))?;
/// assert!(output.contains("ts:api.ts::run"));
/// # Ok::<(), stackscout::Error>(())
/// ```
pub fn diff_dirs(before: impl AsRef<Path>, after: impl AsRef<Path>) -> Result<String, Error> {
    compare_dirs(before.as_ref(), after.as_ref(), false)
}

/// Compares two source directories and includes detailed unchanged analysis diagnostics.
pub fn diff_dirs_with_diagnostics(
    before: impl AsRef<Path>,
    after: impl AsRef<Path>,
) -> Result<String, Error> {
    compare_dirs(before.as_ref(), after.as_ref(), true)
}

fn compare_dirs(before: &Path, after: &Path, diagnostics: bool) -> Result<String, Error> {
    let mut analyzers = analyze::Analyzers::new()?;
    let mut before = analyzers.directory(before)?;
    let mut after = analyzers.directory(after)?;
    let omitted = before
        .issues
        .iter()
        .chain(&after.issues)
        .map(|issue| issue.path.clone())
        .collect::<BTreeSet<_>>();

    before
        .artifacts
        .retain(|artifact| !omitted.contains(&artifact.path));
    after
        .artifacts
        .retain(|artifact| !omitted.contains(&artifact.path));

    let before_graph = build(&before.artifacts)?;
    let after_graph = build(&after.artifacts)?;
    let graph_diff = render::render(&diff::compare(&before_graph, &after_graph));
    Ok(render_issues(
        &before.issues.iter().collect::<Vec<_>>(),
        &after.issues.iter().collect::<Vec<_>>(),
        &omitted,
        graph_diff,
        diagnostics,
    ))
}

fn build<'a>(
    artifacts: impl IntoIterator<Item = &'a model::FileArtifact>,
) -> Result<graph::Graph, Error> {
    let program = resolve::program(artifacts);
    Ok(graph::build(program)?)
}

fn render_issues(
    before: &[&analyze::Issue],
    after: &[&analyze::Issue],
    omitted: &BTreeSet<String>,
    graph_diff: String,
    diagnostics: bool,
) -> String {
    if omitted.is_empty() {
        return graph_diff;
    }

    let mut output = String::new();
    for path in omitted {
        let before = before.iter().find(|issue| issue.path == path.as_str());
        let after = after.iter().find(|issue| issue.path == path.as_str());
        match (before, after) {
            (Some(before), Some(after)) if before.same_as(after) => {
                if diagnostics {
                    output.push_str(&format!(
                        "! incomplete (unchanged; omitted from both): {}\n",
                        before.message()
                    ));
                }
            }
            (Some(before), Some(after)) if diagnostics => {
                output.push_str(&format!(
                    "! incomplete (changed before; omitted from both): {}\n",
                    before.message()
                ));
                output.push_str(&format!(
                    "! incomplete (changed after; omitted from both): {}\n",
                    after.message()
                ));
            }
            (Some(_), Some(_)) => output.push_str(&format!(
                "! incomplete (changed; omitted from both): {path}\n"
            )),
            (Some(before), None) if diagnostics => output.push_str(&format!(
                "! incomplete (resolved; omitted from both): {}\n",
                before.message()
            )),
            (Some(_), None) => output.push_str(&format!(
                "! incomplete (resolved; omitted from both): {path}\n"
            )),
            (None, Some(after)) if diagnostics => output.push_str(&format!(
                "! incomplete (introduced; omitted from both): {}\n",
                after.message()
            )),
            (None, Some(_)) => output.push_str(&format!(
                "! incomplete (introduced; omitted from both): {path}\n"
            )),
            (None, None) => unreachable!("an omitted path has an analysis issue"),
        }
    }
    if !graph_diff.is_empty() {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&graph_diff);
    }
    output
}
