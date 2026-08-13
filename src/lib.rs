mod analyze;
mod diff;
mod discover;
mod graph;
mod model;
mod render;
mod resolve;

use std::collections::BTreeSet;
use std::path::Path;

use thiserror::Error;

/// A fatal error produced while discovering, reading, or constructing a graph.
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
/// the output begins with deterministic `! incomplete` diagnostics. Diagnostics
/// are classified as introduced, resolved, changed, or unchanged.
///
/// ```
/// let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
///     .join("test/fixtures/typescript-basic");
/// let output = stackscout::diff_dirs(fixture.join("before"), fixture.join("after"))?;
/// assert!(output.contains("ts:api.ts::run"));
/// # Ok::<(), stackscout::Error>(())
/// ```
pub fn diff_dirs(before: impl AsRef<Path>, after: impl AsRef<Path>) -> Result<String, Error> {
    let mut analyzers = analyze::Analyzers::new()?;
    let mut before = analyzers.directory(before.as_ref())?;
    let mut after = analyzers.directory(after.as_ref())?;
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

    let before_graph = build(before.artifacts)?;
    let after_graph = build(after.artifacts)?;
    let graph_diff = render::render(&diff::compare(&before_graph, &after_graph));
    Ok(render_issues(
        &before.issues,
        &after.issues,
        &omitted,
        graph_diff,
    ))
}

fn build(artifacts: Vec<model::FileArtifact>) -> Result<graph::Graph, Error> {
    let program = resolve::program(&artifacts);
    Ok(graph::build(program)?)
}

fn render_issues(
    before: &[analyze::Issue],
    after: &[analyze::Issue],
    omitted: &BTreeSet<String>,
    graph_diff: String,
) -> String {
    if omitted.is_empty() {
        return graph_diff;
    }

    let mut output = String::new();
    for path in omitted {
        let before = before.iter().find(|issue| issue.path == path.as_str());
        let after = after.iter().find(|issue| issue.path == path.as_str());
        match (before, after) {
            (Some(before), Some(after)) if before.message == after.message => {
                output.push_str(&format!(
                    "! incomplete (unchanged; omitted from both): {}\n",
                    before.message
                ));
            }
            (Some(before), Some(after)) => {
                output.push_str(&format!(
                    "! incomplete (changed before; omitted from both): {}\n",
                    before.message
                ));
                output.push_str(&format!(
                    "! incomplete (changed after; omitted from both): {}\n",
                    after.message
                ));
            }
            (Some(before), None) => output.push_str(&format!(
                "! incomplete (resolved; omitted from both): {}\n",
                before.message
            )),
            (None, Some(after)) => output.push_str(&format!(
                "! incomplete (introduced; omitted from both): {}\n",
                after.message
            )),
            (None, None) => unreachable!("an omitted path has an analysis issue"),
        }
    }
    if !graph_diff.is_empty() {
        output.push('\n');
        output.push_str(&graph_diff);
    }
    output
}
