mod rust;
mod typescript;

use std::fs;
use std::path::Path;

use thiserror::Error;
use tree_sitter::{Node, Parser};

use crate::discover;
use crate::model::{FileArtifact, Language, SourceRange, Syntax};

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub(crate) enum AnalyzeError {
    #[error("failed to configure the {language} parser: {message}")]
    Parser {
        language: &'static str,
        message: String,
    },
    #[error("the parser rejected {0}")]
    Rejected(String),
    #[error("syntax error in {path} near line {line}")]
    Syntax { path: String, line: usize },
    #[error("AST {budget} limit exceeded in {path} (maximum {limit})")]
    Limit {
        path: String,
        budget: &'static str,
        limit: usize,
    },
    #[error("invalid UTF-8 syntax node in {0}")]
    InvalidNode(String),
    #[error("unsupported or malformed syntax in {path}: {message}")]
    Unsupported { path: String, message: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Issue {
    pub(crate) path: String,
    error: AnalyzeError,
}

impl Issue {
    pub(crate) fn new(path: String, error: AnalyzeError) -> Self {
        Self { path, error }
    }

    pub(crate) fn same_as(&self, other: &Self) -> bool {
        self == other
    }

    pub(crate) fn message(&self) -> String {
        self.error.to_string()
    }
}

pub(crate) struct Directory {
    pub(crate) artifacts: Vec<FileArtifact>,
    pub(crate) issues: Vec<Issue>,
}

// Bump when extraction semantics change. Parser mode distinguishes TS from TSX.
pub(crate) const VERSION: u32 = 5;

// Keep recursive visitors comfortably below typical thread stacks. Count every
// node (including tokens and errors) to bound broad trees as well as nesting.
const MAX_AST_DEPTH: usize = 128;
const MAX_AST_NODES: usize = 100_000;

pub(crate) struct Analyzers {
    rust: Parser,
    typescript: Parser,
    tsx: Parser,
    #[cfg(test)]
    pub(crate) parsed: Vec<String>,
}

impl Analyzers {
    pub(crate) fn new() -> Result<Self, AnalyzeError> {
        Ok(Self {
            rust: parser("Rust", tree_sitter_rust::LANGUAGE.into())?,
            typescript: parser(
                "TypeScript",
                tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            )?,
            tsx: parser("TSX", tree_sitter_typescript::LANGUAGE_TSX.into())?,
            #[cfg(test)]
            parsed: Vec::new(),
        })
    }

    pub(crate) fn directory(&mut self, root: &Path) -> Result<Directory, crate::Error> {
        let mut artifacts = Vec::new();
        let mut issues = Vec::new();
        for path in discover::sources(root)? {
            let relative = discover::relative(root, &path)?;
            crate::repository::source_path(&relative)?;
            let source = fs::read_to_string(&path).map_err(|error| crate::Error {
                message: format!("failed to read {relative}: {error}"),
            })?;
            match self.file(&relative, &source) {
                Ok(artifact) => artifacts.push(artifact),
                Err(error) => issues.push(Issue::new(relative, error)),
            }
        }
        Ok(Directory { artifacts, issues })
    }

    pub(crate) fn file(&mut self, path: &str, source: &str) -> Result<FileArtifact, AnalyzeError> {
        let (language, parser) = match Syntax::for_path(Path::new(path)) {
            Some(Syntax::Rust) => (Language::Rust, &mut self.rust),
            Some(Syntax::TypeScript) => (Language::TypeScript, &mut self.typescript),
            Some(Syntax::Tsx) => (Language::TypeScript, &mut self.tsx),
            None => {
                return Err(AnalyzeError::Unsupported {
                    path: path.to_owned(),
                    message: "unsupported source extension".to_owned(),
                });
            }
        };

        #[cfg(test)]
        self.parsed.push(path.to_owned());
        let tree = parser
            .parse(source, None)
            .ok_or_else(|| AnalyzeError::Rejected(path.to_owned()))?;
        validate_tree(path, tree.root_node())?;
        if tree.root_node().has_error()
            && let Some(error) = first_error(tree.root_node(), source, language)
        {
            return Err(AnalyzeError::Syntax {
                path: path.to_owned(),
                line: error.start_position().row + 1,
            });
        }

        match language {
            Language::Rust => rust::extract(path, source, tree.root_node()),
            Language::TypeScript => typescript::extract(path, source, tree.root_node()),
        }
    }
}

fn parser(name: &'static str, language: tree_sitter::Language) -> Result<Parser, AnalyzeError> {
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .map_err(|error| AnalyzeError::Parser {
            language: name,
            message: error.to_string(),
        })?;
    Ok(parser)
}

// Cursor DFS does not queue wide sibling lists or recurse. The root is depth 1.
// This must run before ANY syntax-error, shape, or extraction visitor.
fn validate_tree(path: &str, root: Node<'_>) -> Result<(), AnalyzeError> {
    let mut cursor = root.walk();
    let mut depth = 1;
    let mut nodes = 0;
    loop {
        nodes += 1;
        for (budget, value, limit) in [
            ("depth", depth, MAX_AST_DEPTH),
            ("node count", nodes, MAX_AST_NODES),
        ] {
            if value > limit {
                return Err(AnalyzeError::Limit {
                    path: path.to_owned(),
                    budget,
                    limit,
                });
            }
        }
        if cursor.goto_first_child() {
            depth += 1;
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(());
            }
            depth -= 1;
        }
    }
}

fn first_error<'tree>(node: Node<'tree>, source: &str, language: Language) -> Option<Node<'tree>> {
    if (node.is_error() || node.is_missing())
        && !known_typescript_error(node, source, language)
        && !(language == Language::Rust && rust::safe_foreign(node, source))
    {
        return Some(node);
    }
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .find_map(|child| first_error(child, source, language))
}

fn known_typescript_error(node: Node<'_>, source: &str, language: Language) -> bool {
    if language != Language::TypeScript || !node.is_error() {
        return false;
    }
    let Some(parent) = node.parent() else {
        return false;
    };
    parent.kind() == "export_statement"
        && node
            .utf8_text(source.as_bytes())
            .is_ok_and(|value| value == "type")
}

pub(super) fn text<'a>(
    path: &str,
    source: &'a str,
    node: Node<'_>,
) -> Result<&'a str, AnalyzeError> {
    node.utf8_text(source.as_bytes())
        .map_err(|_| AnalyzeError::InvalidNode(path.to_owned()))
}

pub(super) fn first_named(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).next()
}

pub(super) fn compact(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn range(node: Node<'_>) -> SourceRange {
    let start = node.start_position();
    let end = node.end_position();
    SourceRange {
        start_line: start.row + 1,
        start_column: start.column + 1,
        end_line: end.row + 1,
        end_column: end.column + 1,
    }
}

pub(super) fn shape(path: &str, source: &str, node: Node<'_>) -> Result<String, AnalyzeError> {
    fn visit(
        path: &str,
        source: &str,
        node: Node<'_>,
        output: &mut String,
    ) -> Result<(), AnalyzeError> {
        if node.kind() == "comment" {
            return Ok(());
        }
        if node.child_count() == 0 {
            output.push_str(node.kind());
            output.push(':');
            output.push_str(text(path, source, node)?);
            output.push('|');
            return Ok(());
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            visit(path, source, child, output)?;
        }
        Ok(())
    }

    let mut output = String::new();
    visit(path, source, node, &mut output)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ast_depth_rejects_extreme_valid_and_malformed_sources() {
        let mut analyzers = Analyzers::new().unwrap();
        for (path, prefix) in [
            ("deep.rs", "fn run() {"),
            ("deep.ts", "function run() {"),
            ("deep.tsx", "function run() {"),
        ] {
            assert!(analyzers.file(path, "").unwrap().symbols.is_empty());
            // Root, function, block, statement and literal add five levels.
            // Exactly the depth limit still reaches extraction and shape.
            let nesting = MAX_AST_DEPTH - 5;
            let source = format!(
                "{prefix} {}0{}; }}",
                "(".repeat(nesting),
                ")".repeat(nesting)
            );
            assert_eq!(analyzers.file(path, &source).unwrap().symbols.len(), 1);
            let source = format!(
                "{prefix} {}0{}; }}",
                "(".repeat(nesting + 1),
                ")".repeat(nesting + 1)
            );
            assert!(matches!(
                analyzers.file(path, &source),
                Err(AnalyzeError::Limit {
                    budget: "depth",
                    ..
                })
            ));
            let deep = format!("{prefix} {}0{}; }}", "(".repeat(4096), ")".repeat(4096));
            for source in [deep.clone(), format!("{deep}\nfunction (")] {
                assert_eq!(
                    analyzers.file(path, &source).unwrap_err(),
                    AnalyzeError::Limit {
                        path: path.to_owned(),
                        budget: "depth",
                        limit: MAX_AST_DEPTH,
                    },
                    "{path}"
                );
            }
        }
    }

    #[test]
    fn ast_node_budget_counts_broad_trees_including_tokens() {
        let mut analyzers = Analyzers::new().unwrap();
        for (path, prefix) in [
            ("wide.rs", "fn run() {"),
            ("wide.ts", "function run() {"),
            ("wide.tsx", "function run() {"),
        ] {
            // Each `0;` has a statement, a literal and an unnamed semicolon.
            // Named-only counting would incorrectly accept the oversized tree.
            let near_limit = format!("{prefix} {} }}", "0;".repeat(MAX_AST_NODES / 3 - 20));
            assert_eq!(analyzers.file(path, &near_limit).unwrap().symbols.len(), 1);
            let oversized = format!("{prefix} {} }}", "0;".repeat(MAX_AST_NODES / 3));
            for source in [oversized.clone(), format!("{oversized}\nfunction (")] {
                assert_eq!(
                    analyzers.file(path, &source).unwrap_err(),
                    AnalyzeError::Limit {
                        path: path.to_owned(),
                        budget: "node count",
                        limit: MAX_AST_NODES,
                    },
                    "{path}"
                );
            }
        }
    }

    #[test]
    fn ast_limits_preserve_failed_file_caching_filtering_and_recovery() {
        for (path, valid, prefix) in [
            ("bad.rs", "fn run() {}", "fn run() {"),
            ("bad.ts", "function run() {}", "function run() {"),
        ] {
            let deep = format!("{prefix} {}0{}; }}", "(".repeat(4096), ")".repeat(4096));
            let wide = format!("{prefix} {} }}", "0;".repeat(MAX_AST_NODES / 3));
            for (source, budget) in [(deep, "depth"), (wide, "node count")] {
                let mut repo = crate::Repository::new().unwrap();
                repo.change("good.rs", b"fn good() {}").unwrap();
                repo.change(path, valid.as_bytes()).unwrap();
                repo.checkpoint("valid");
                assert!(repo.change(path, source.as_bytes()).unwrap());
                assert!(!repo.change(path, source.as_bytes()).unwrap());
                let snapshot = repo.snapshot();
                assert_eq!(snapshot.graph.symbols.len(), 1);
                assert_eq!(
                    snapshot.graph.symbols.keys().next().unwrap().file,
                    "good.rs"
                );
                let diff = repo.diff_with_diagnostics("valid").unwrap();
                assert!(
                    diff.starts_with("! incomplete (introduced; omitted from both):"),
                    "{diff}"
                );
                assert!(
                    diff.contains(&format!("AST {budget} limit exceeded in {path}")),
                    "{diff}"
                );
                assert!(!diff.contains("::run"), "{diff}");
                repo.checkpoint("failed");
                assert_eq!(repo.diff("failed").unwrap(), "");
                assert!(
                    repo.diff_with_diagnostics("failed")
                        .unwrap()
                        .contains("unchanged; omitted from both")
                );
                assert!(repo.change(path, valid.as_bytes()).unwrap());
                assert_eq!(repo.snapshot().graph.symbols.len(), 2);
                let diff = repo.diff("failed").unwrap();
                assert!(diff.contains("resolved; omitted from both"), "{diff}");
                assert!(!diff.contains("::run"), "{diff}");

                let directory = tempfile::tempdir().unwrap();
                fs::write(directory.path().join(path), &source).unwrap();
                fs::write(directory.path().join("good.rs"), "fn good() {}").unwrap();
                let analysis = Analyzers::new()
                    .unwrap()
                    .directory(directory.path())
                    .unwrap();
                assert_eq!(analysis.artifacts.len(), 1);
                assert_eq!(analysis.artifacts[0].path, "good.rs");
                assert_eq!(analysis.issues.len(), 1);
                assert_eq!(analysis.issues[0].path, path);
                assert!(
                    analysis.issues[0]
                        .message()
                        .contains(&format!("AST {budget} limit exceeded"))
                );
            }
        }
    }
}
