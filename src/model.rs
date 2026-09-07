use std::fmt;
use std::path::Path;

/// Parser mode, including the TS/TSX distinction within the TypeScript language.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Syntax {
    Rust,
    TypeScript,
    Tsx,
}

impl Syntax {
    pub(crate) fn for_path(path: &Path) -> Option<Self> {
        let name = path.file_name()?.to_str().unwrap_or("");
        if name.ends_with(".d.ts") || name.ends_with(".d.mts") || name.ends_with(".d.cts") {
            return None;
        }
        match path.extension()?.to_str()? {
            "rs" => Some(Self::Rust),
            "ts" | "mts" | "cts" => Some(Self::TypeScript),
            "tsx" => Some(Self::Tsx),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub(crate) enum Language {
    Rust,
    TypeScript,
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rust => f.write_str("rust"),
            Self::TypeScript => f.write_str("ts"),
        }
    }
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub(crate) struct SymbolKey {
    pub(crate) language: Language,
    pub(crate) file: String,
    pub(crate) qualified_name: Vec<String>,
    /// Unevaluated Rust cfg attributes distinguish alternative definitions.
    pub(crate) conditions: Vec<String>,
}

impl SymbolKey {
    pub(crate) fn name(&self) -> String {
        self.conditions
            .iter()
            .chain(&self.qualified_name)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("::")
    }
}

impl fmt::Display for SymbolKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}::{}", self.language, self.file, self.name())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
pub(crate) struct SourceRange {
    pub(crate) start_line: usize,
    pub(crate) start_column: usize,
    pub(crate) end_line: usize,
    pub(crate) end_column: usize,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
pub(crate) enum CallTarget {
    /// An unqualified call such as `work()`.
    Bare(String),
    /// A TypeScript call through `this`, such as `this.work()`.
    ThisMethod(String),
    /// A Rust method call through `self`, such as `self.work()`.
    SelfMethod(String),
    /// A Rust associated call through `Self`, such as `Self::work()`.
    SelfAssociated(String),
    /// A qualified call such as `store.work()` or `crate::store::work()`.
    Path(Vec<String>),
    /// A Rust method call through a receiver whose type is not known syntactically.
    Method(Vec<String>),
    /// A TypeScript constructor call such as `new Worker()`.
    Constructor(Vec<String>),
    /// A bare call shadowed by a parameter or local binding.
    Bound(String),
    /// An unexpanded/deferred body or unsupported callee, retained without guessing edges.
    Opaque(String),
}

impl fmt::Display for CallTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bare(name) => f.write_str(name),
            Self::ThisMethod(name) => write!(f, "this.{name}"),
            Self::SelfMethod(name) => write!(f, "self.{name}"),
            Self::SelfAssociated(name) => write!(f, "Self::{name}"),
            Self::Path(path) | Self::Method(path) => f.write_str(&path.join("::")),
            Self::Constructor(path) => write!(f, "new {}", path.join(".")),
            Self::Bound(name) | Self::Opaque(name) => f.write_str(name),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ImportTarget {
    Named(String),
    Default,
    Namespace,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Import {
    pub(crate) local: String,
    pub(crate) target: ImportTarget,
    pub(crate) source: String,
}

/// An explicit Rust `use` binding in one lexical scope of its owning file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RustImport {
    pub(crate) module: Vec<String>,
    pub(crate) scope: SourceRange,
    /// `*` marks an unexpanded glob; its path is empty.
    pub(crate) local: String,
    pub(crate) path: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ExportName {
    Named(String),
    Default,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ExportBinding {
    pub(crate) local: String,
    pub(crate) exported: ExportName,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub(crate) enum Visibility {
    Private,
    Public,
}

impl Visibility {
    pub(crate) fn is_public(self) -> bool {
        self == Self::Public
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalSymbol {
    pub(crate) key: SymbolKey,
    pub(crate) module: Vec<String>,
    pub(crate) location: SourceRange,
    pub(crate) visibility: Visibility,
    pub(crate) shape: String,
    pub(crate) steps: Vec<LocalStep>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct CallSite {
    pub(crate) target: CallTarget,
    pub(crate) location: SourceRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum LocalStep {
    Call(CallSite),
    Branch {
        key: String,
        label: String,
        location: SourceRange,
        children: Vec<LocalStep>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FileArtifact {
    pub(crate) language: Language,
    pub(crate) path: String,
    pub(crate) symbols: Vec<LocalSymbol>,
    pub(crate) imports: Vec<Import>,
    pub(crate) rust_imports: Vec<RustImport>,
    pub(crate) exports: Vec<ExportBinding>,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub(crate) enum Resolution {
    Resolved(SymbolKey),
    Unresolved(CallTarget),
    Uncertain(CallTarget),
    Ambiguous {
        target: CallTarget,
        candidates: Vec<SymbolKey>,
    },
}

impl Resolution {
    pub(crate) fn identity(&self) -> String {
        match self {
            Self::Resolved(key) => key.to_string(),
            Self::Unresolved(target) => format!("?{target}"),
            Self::Uncertain(target) => format!("~{target}"),
            Self::Ambiguous { target, candidates } => format!(
                "!{target}:{}",
                candidates
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedCall {
    pub(crate) resolution: Resolution,
    pub(crate) location: SourceRange,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedStep {
    Call(ResolvedCall),
    Branch {
        key: String,
        label: String,
        location: SourceRange,
        children: Vec<ResolvedStep>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedSymbol {
    pub(crate) symbol: LocalSymbol,
    pub(crate) steps: Vec<ResolvedStep>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedProgram {
    pub(crate) symbols: Vec<ResolvedSymbol>,
}
