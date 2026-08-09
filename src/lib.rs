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
/// The supported file extensions are `.rs`, `.ts`, and `.tsx`. If a file cannot
/// be analyzed in either directory, that path is omitted from both graphs and
/// the output begins with deterministic `! incomplete` diagnostics.
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
        output.push_str(&format!(
            "! incomplete: omitted {path} from both snapshots\n"
        ));
    }
    for issue in before {
        output.push_str(&format!("! before: {}\n", issue.message));
    }
    for issue in after {
        output.push_str(&format!("! after: {}\n", issue.message));
    }
    if !graph_diff.is_empty() {
        output.push('\n');
        output.push_str(&graph_diff);
    }
    output
}
