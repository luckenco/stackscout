mod rust;
mod typescript;

use std::fs;
use std::path::Path;

use thiserror::Error;
use tree_sitter::{Node, Parser};

use crate::discover;
use crate::model::{FileArtifact, Language, SourceRange};

#[derive(Debug, Error)]
pub(crate) enum AnalyzeError {
    #[error(transparent)]
    Discover(#[from] discover::DiscoverError),
    #[error("failed to read {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("failed to configure the {language} parser: {message}")]
    Parser {
        language: &'static str,
        message: String,
    },
    #[error("the parser rejected {0}")]
    Rejected(String),
    #[error("syntax error in {path} near line {line}")]
    Syntax { path: String, line: usize },
    #[error("invalid UTF-8 syntax node in {0}")]
    InvalidNode(String),
    #[error("unsupported or malformed syntax in {path}: {message}")]
    Unsupported { path: String, message: String },
}

#[derive(Debug)]
pub(crate) struct Issue {
    pub(crate) path: String,
    error: AnalyzeError,
}

impl Issue {
    fn new(path: String, error: AnalyzeError) -> Self {
        Self { path, error }
    }

    pub(crate) fn same_as(&self, other: &Self) -> bool {
        match (&self.error, &other.error) {
            (AnalyzeError::Rejected(_), AnalyzeError::Rejected(_)) => true,
            (
                AnalyzeError::Syntax { line: before, .. },
                AnalyzeError::Syntax { line: after, .. },
            ) => before == after,
            (AnalyzeError::InvalidNode(_), AnalyzeError::InvalidNode(_)) => true,
            (
                AnalyzeError::Unsupported {
                    message: before, ..
                },
                AnalyzeError::Unsupported { message: after, .. },
            ) => before == after,
            _ => false,
        }
    }

    pub(crate) fn message(&self) -> String {
        self.error.to_string()
    }
}

pub(crate) struct Directory {
    pub(crate) artifacts: Vec<FileArtifact>,
    pub(crate) issues: Vec<Issue>,
}

pub(crate) struct Analyzers {
    rust: Parser,
    typescript: Parser,
    tsx: Parser,
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
        })
    }

    pub(crate) fn directory(&mut self, root: &Path) -> Result<Directory, AnalyzeError> {
        let mut artifacts = Vec::new();
        let mut issues = Vec::new();
        for path in discover::sources(root)? {
            let relative = discover::relative(root, &path)?;
            let source = fs::read_to_string(&path).map_err(|source| AnalyzeError::Read {
                path: relative.clone(),
                source,
            })?;
            match self.file(&relative, &source) {
                Ok(artifact) => artifacts.push(artifact),
                Err(error) => issues.push(Issue::new(relative, error)),
            }
        }
        Ok(Directory { artifacts, issues })
    }

    fn file(&mut self, path: &str, source: &str) -> Result<FileArtifact, AnalyzeError> {
        let (language, parser) = match Path::new(path).extension().and_then(|value| value.to_str())
        {
            Some("rs") => (Language::Rust, &mut self.rust),
            Some("ts" | "mts" | "cts") => (Language::TypeScript, &mut self.typescript),
            Some("tsx") => (Language::TypeScript, &mut self.tsx),
            _ => unreachable!("discovery only returns supported extensions"),
        };

        let tree = parser
            .parse(source, None)
            .ok_or_else(|| AnalyzeError::Rejected(path.to_owned()))?;
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

fn first_error<'tree>(node: Node<'tree>, source: &str, language: Language) -> Option<Node<'tree>> {
    if (node.is_error() || node.is_missing()) && !known_typescript_error(node, source, language) {
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
