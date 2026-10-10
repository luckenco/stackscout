mod analyze;
mod diff;
mod discover;
mod graph;
mod model;
#[cfg(unix)]
mod query;
mod render;
mod repository;
mod resolve;
#[cfg(unix)]
mod review;
#[cfg(unix)]
pub mod service;

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

    let unsupported = diff::unsupported(&before.artifacts, &after.artifacts);
    let before_graph = build(&before.artifacts)?;
    let after_graph = build(&after.artifacts)?;
    let graph_diff = render::render(&diff::compare(&before_graph, &after_graph));
    Ok(render_issues(
        &before.issues.iter().collect::<Vec<_>>(),
        &after.issues.iter().collect::<Vec<_>>(),
        &omitted,
        &unsupported,
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
    unsupported: &[(String, &'static str)],
    graph_diff: String,
    diagnostics: bool,
) -> String {
    if omitted.is_empty() && unsupported.is_empty() {
        return graph_diff;
    }

    let mut output = render::Output::new();
    let mut wrote_issue = false;
    for path in omitted {
        if output.truncated {
            break;
        }
        let before = before.iter().find(|issue| issue.path == path.as_str());
        let after = after.iter().find(|issue| issue.path == path.as_str());
        let status = match (before, after) {
            (Some(before), Some(after)) if before.same_as(after) => {
                if !diagnostics {
                    continue;
                }
                "unchanged"
            }
            (Some(before), Some(after)) if diagnostics => {
                issue_line(&mut output, "changed before", &before.message());
                issue_line(&mut output, "changed after", &after.message());
                wrote_issue = true;
                continue;
            }
            (Some(_), Some(_)) => "changed",
            (Some(_), None) => "resolved",
            (None, Some(_)) => "introduced",
            (None, None) => unreachable!("an omitted path has an analysis issue"),
        };
        if diagnostics {
            issue_line(&mut output, status, &after.or(before).unwrap().message());
        } else {
            issue_line(&mut output, status, path);
        }
        wrote_issue = true;
    }
    for (path, category) in unsupported {
        if output.truncated {
            break;
        }
        use std::fmt::Write;
        let path = if path.chars().any(char::is_control) {
            format!("{path:?}")
        } else {
            path.clone()
        };
        let _ = writeln!(output, "! unsupported (rust {category}): {path}");
        wrote_issue = true;
    }
    let graph_truncated = graph_diff.ends_with(render::TRUNCATED);
    let text = graph_diff
        .strip_suffix(render::TRUNCATED)
        .unwrap_or(&graph_diff);
    if !text.is_empty() {
        if wrote_issue {
            output.push('\n');
        }
        output.push_str(text);
    }
    output.finish(graph_truncated)
}

// Diagnostics are line-oriented too: never let source-derived text forge records.
fn issue_line(output: &mut render::Output, status: &str, message: &str) {
    use std::fmt::Write;
    let _ = write!(output, "! incomplete ({status}; omitted from both): ");
    if message.chars().any(char::is_control) {
        let _ = writeln!(output, "{message:?}");
    } else {
        let _ = writeln!(output, "{message}");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn diagnostics_escape_source_text_and_bound_combined_output() {
        let path = "bad\n+ forged\u{1b}[2J.rs";
        let issue = crate::analyze::Issue::new(
            path.to_owned(),
            crate::analyze::AnalyzeError::Unsupported {
                path: path.to_owned(),
                message: "bad\r\n- forged\t".to_owned(),
            },
        );
        for diagnostics in [false, true] {
            let output = super::render_issues(
                &[],
                &[&issue],
                &super::BTreeSet::from([path.to_owned()]),
                &[],
                String::new(),
                diagnostics,
            );
            assert_eq!(output.lines().count(), 1, "{output:?}");
            assert!(
                !output.trim_end().chars().any(char::is_control),
                "{output:?}"
            );
        }
        let issues = (0..2_000)
            .map(|i| {
                let path = format!("file{i:04}.rs");
                crate::analyze::Issue::new(
                    path.clone(),
                    crate::analyze::AnalyzeError::Syntax { path, line: 1 },
                )
            })
            .collect::<Vec<_>>();
        let omitted = issues.iter().map(|issue| issue.path.clone()).collect();
        let refs = issues.iter().collect::<Vec<_>>();
        let graph_diff = format!("{}{}", "x".repeat(48 * 1024), crate::render::TRUNCATED);
        let output = super::render_issues(&[], &refs, &omitted, &[], graph_diff, true);
        assert!(output.len() <= 48 * 1024 + crate::render::TRUNCATED.len());
        assert_eq!(output.matches(crate::render::TRUNCATED).count(), 1);
        assert!(output.ends_with(crate::render::TRUNCATED));
    }
}
