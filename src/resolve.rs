#[cfg(test)]
mod bench;
mod rust;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Component, Path, PathBuf};

use crate::model::{
    CallTarget, ExportBinding, ExportName, FileArtifact, Import, ImportTarget, Language, LocalStep,
    LocalSymbol, Resolution, ResolvedCall, ResolvedProgram, ResolvedStep, ResolvedSymbol,
    SourceRange, SymbolKey,
};

// Pass-local lookup tables borrow identities; the original symbol stream still
// reaches graph construction, where duplicate SymbolKeys remain fatal errors.
#[derive(Default)]
struct Index<'a> {
    files: BTreeMap<(Language, &'a str, &'a [String]), Vec<&'a SymbolKey>>,
    rust: BTreeMap<(RustOwner, Vec<String>), Vec<&'a SymbolKey>>,
    rust_trait_methods: BTreeSet<(RustOwner, String, String)>,
    rust_roots: BTreeSet<String>,
    imports: HashMap<(&'a str, &'a str), Vec<&'a Import>>,
    exports: HashMap<(&'a str, &'a ExportName), Vec<&'a ExportBinding>>,
}

// Path-derived safety boundary, not a Cargo target model. A sibling module in a
// multi-root src/ directory has unknown ownership until its declarations are known.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum RustOwner {
    Root(String),
    Unknown(String),
}

impl<'a> Index<'a> {
    fn new(symbols: impl IntoIterator<Item = &'a LocalSymbol>, roots: BTreeSet<String>) -> Self {
        let mut index = Self {
            rust_roots: roots,
            ..Self::default()
        };
        for symbol in symbols {
            let key = &symbol.key;
            index
                .files
                .entry((key.language, &key.file, &key.qualified_name))
                .or_default()
                .push(key);
            if key.language == Language::Rust {
                let root = index.rust_owner(&key.file);
                if let Some((owner, method)) = key
                    .qualified_name
                    .iter()
                    .rev()
                    .nth(1)
                    .zip(key.qualified_name.last())
                    && let Some(ty) = owner
                        .strip_prefix('<')
                        .and_then(|part| part.split_once(" as "))
                {
                    index.rust_trait_methods.insert((
                        root.clone(),
                        ty.0.to_owned(),
                        method.clone(),
                    ));
                }
                let mut qualified = rust::module(&key.file);
                qualified.extend_from_slice(&key.qualified_name);
                index.rust.entry((root, qualified)).or_default().push(key);
            }
        }
        index
    }

    fn rust_owner(&self, file: &str) -> RustOwner {
        rust::owner(file, &self.rust_roots)
    }

    fn imports<'b>(&'b self, file: &'b str, local: &'b str) -> &'b [&'b Import] {
        self.imports
            .get(&(file, local))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    fn file(&self, language: Language, file: &str, qualified: &[String], output: &mut Candidates) {
        for key in self
            .files
            .get(&(language, file, qualified))
            .into_iter()
            .flatten()
        {
            if !output.spend() {
                break;
            }
            output.keys.insert((*key).clone());
        }
    }
}

// A per-call bound also bounds retained keys and graph/shape work downstream.
// Charge matching walks, including duplicate bindings; unrelated bindings are indexed once.
const TYPESCRIPT_WORK_LIMIT: usize = 256;

struct Candidates {
    keys: BTreeSet<SymbolKey>,
    remaining: usize,
    limited: bool,
}

impl Candidates {
    fn new() -> Self {
        Self {
            keys: BTreeSet::new(),
            remaining: TYPESCRIPT_WORK_LIMIT,
            limited: false,
        }
    }

    fn spend(&mut self) -> bool {
        if self.remaining == 0 {
            self.limited = true;
            return false;
        }
        self.remaining -= 1;
        true
    }

    fn finish(self, target: &CallTarget) -> Resolution {
        if self.limited {
            return Resolution::Limited {
                target: target.clone(),
                candidates: self.keys.into_iter().collect(),
            };
        }
        resolution(target, self.keys)
    }
}

pub(crate) fn program<'a>(
    artifacts: impl IntoIterator<Item = &'a FileArtifact>,
) -> ResolvedProgram {
    let artifacts = artifacts.into_iter().collect::<Vec<_>>();
    let roots = artifacts
        .iter()
        .filter(|artifact| artifact.language == Language::Rust && rust::is_root(&artifact.path))
        .map(|artifact| artifact.path.clone())
        .collect();
    let mut all_symbols = Index::new(
        artifacts.iter().flat_map(|artifact| &artifact.symbols),
        roots,
    );
    for artifact in &artifacts {
        for import in &artifact.imports {
            all_symbols
                .imports
                .entry((&artifact.path, &import.local))
                .or_default()
                .push(import);
        }
        for binding in &artifact.exports {
            all_symbols
                .exports
                .entry((&artifact.path, &binding.exported))
                .or_default()
                .push(binding);
        }
    }

    let mut symbols = Vec::new();
    for artifact in &artifacts {
        for symbol in &artifact.symbols {
            symbols.push(ResolvedSymbol {
                symbol: symbol.clone(),
                steps: resolve_steps(&symbol.steps, symbol, artifact, &all_symbols),
            });
        }
    }
    symbols.sort_by(|left, right| left.symbol.key.cmp(&right.symbol.key));
    ResolvedProgram { symbols }
}

fn resolve_steps(
    steps: &[LocalStep],
    caller: &LocalSymbol,
    artifact: &FileArtifact,
    symbols: &Index<'_>,
) -> Vec<ResolvedStep> {
    steps
        .iter()
        .map(|step| match step {
            LocalStep::Call(call) => ResolvedStep::Call(ResolvedCall {
                resolution: target(caller, &call.target, &call.location, artifact, symbols),
                location: call.location.clone(),
            }),
            LocalStep::Branch {
                key,
                label,
                location,
                children,
            } => ResolvedStep::Branch {
                key: key.clone(),
                label: label.clone(),
                location: location.clone(),
                children: resolve_steps(children, caller, artifact, symbols),
            },
        })
        .collect()
}

fn target(
    caller: &LocalSymbol,
    target: &CallTarget,
    location: &SourceRange,
    artifact: &FileArtifact,
    symbols: &Index<'_>,
) -> Resolution {
    if artifact.language == Language::Rust {
        return rust::resolve(caller, target, location, &artifact.rust_imports, symbols);
    }
    let mut candidates = Candidates::new();
    match target {
        CallTarget::Bare(name) => {
            same_file(
                &caller.key,
                std::slice::from_ref(name),
                symbols,
                &mut candidates,
            );
            for import in symbols.imports(&artifact.path, name) {
                if !candidates.spend() {
                    break;
                }
                match &import.target {
                    ImportTarget::Named(imported) => imported_symbol(
                        &caller.key,
                        import,
                        &ExportName::Named(imported.clone()),
                        &[],
                        symbols,
                        &mut candidates,
                    ),
                    ImportTarget::Default => imported_symbol(
                        &caller.key,
                        import,
                        &ExportName::Default,
                        &[],
                        symbols,
                        &mut candidates,
                    ),
                    ImportTarget::Namespace => {}
                }
            }
        }
        CallTarget::Bound(_) | CallTarget::Opaque(_) => {
            return Resolution::Uncertain(target.clone());
        }
        CallTarget::Method(_) | CallTarget::TypedMethod { .. } => {
            return Resolution::Unresolved(target.clone());
        }
        CallTarget::ThisMethod(name)
        | CallTarget::SelfMethod(name)
        | CallTarget::SelfAssociated(name) => {
            let mut qualified =
                caller.key.qualified_name[..caller.key.qualified_name.len() - 1].to_vec();
            qualified.push(name.clone());
            same_file(&caller.key, &qualified, symbols, &mut candidates);
        }
        CallTarget::Path(path) => {
            if path.len() > 1 {
                same_file(&caller.key, &static_suffix(path), symbols, &mut candidates);
            } else {
                same_file(&caller.key, path, symbols, &mut candidates);
            }
            if let Some((local, rest)) = path.split_first() {
                for import in symbols.imports(&artifact.path, local) {
                    if !candidates.spend() {
                        break;
                    }
                    match &import.target {
                        ImportTarget::Named(exported) => imported_symbol(
                            &caller.key,
                            import,
                            &ExportName::Named(exported.clone()),
                            &static_suffix(rest),
                            symbols,
                            &mut candidates,
                        ),
                        ImportTarget::Default => imported_symbol(
                            &caller.key,
                            import,
                            &ExportName::Default,
                            &static_suffix(rest),
                            symbols,
                            &mut candidates,
                        ),
                        ImportTarget::Namespace => {
                            if let Some((exported, rest)) = rest.split_first() {
                                imported_symbol(
                                    &caller.key,
                                    import,
                                    &ExportName::Named(exported.clone()),
                                    &static_suffix(rest),
                                    symbols,
                                    &mut candidates,
                                );
                            }
                        }
                    }
                }
            }
        }
        CallTarget::Constructor(path) => {
            let mut qualified = path.clone();
            qualified.push("constructor".to_owned());
            same_file(&caller.key, &qualified, symbols, &mut candidates);
            if let Some((local, rest)) = path.split_first() {
                for import in symbols.imports(&artifact.path, local) {
                    if !candidates.spend() {
                        break;
                    }
                    match &import.target {
                        ImportTarget::Named(imported) => {
                            let mut suffix = rest.to_vec();
                            suffix.push("constructor".to_owned());
                            imported_symbol(
                                &caller.key,
                                import,
                                &ExportName::Named(imported.clone()),
                                &suffix,
                                symbols,
                                &mut candidates,
                            );
                        }
                        ImportTarget::Namespace => {
                            if let Some((exported, rest)) = rest.split_first() {
                                let mut suffix = rest.to_vec();
                                suffix.push("constructor".to_owned());
                                imported_symbol(
                                    &caller.key,
                                    import,
                                    &ExportName::Named(exported.clone()),
                                    &suffix,
                                    symbols,
                                    &mut candidates,
                                );
                            }
                        }
                        ImportTarget::Default if rest.is_empty() => imported_symbol(
                            &caller.key,
                            import,
                            &ExportName::Default,
                            &["constructor".to_owned()],
                            symbols,
                            &mut candidates,
                        ),
                        ImportTarget::Default => {}
                    }
                }
            }
        }
    }

    candidates.finish(target)
}

fn resolution(target: &CallTarget, mut candidates: BTreeSet<SymbolKey>) -> Resolution {
    match candidates.len() {
        0 => Resolution::Unresolved(target.clone()),
        1 => Resolution::Resolved(candidates.pop_first().expect("one candidate exists")),
        _ => Resolution::Ambiguous {
            target: target.clone(),
            candidates: candidates.into_iter().collect(),
        },
    }
}

fn same_file(
    caller: &SymbolKey,
    qualified: &[String],
    symbols: &Index<'_>,
    output: &mut Candidates,
) {
    symbols.file(caller.language, &caller.file, qualified, output);
}

fn static_suffix(path: &[String]) -> Vec<String> {
    let mut qualified = path.to_vec();
    if !qualified.is_empty() {
        qualified.insert(qualified.len() - 1, "[static]".to_owned());
    }
    qualified
}

fn imported_symbol(
    caller: &SymbolKey,
    import: &Import,
    exported: &ExportName,
    suffix: &[String],
    symbols: &Index<'_>,
    output: &mut Candidates,
) {
    for file in import_files(&caller.file, &import.source) {
        if !output.spend() {
            return;
        }
        let Some(bindings) = symbols.exports.get(&(file.as_str(), exported)) else {
            continue;
        };
        for binding in bindings {
            if !output.spend() {
                return;
            }
            let mut qualified = vec![binding.local.clone()];
            qualified.extend_from_slice(suffix);
            symbols.file(caller.language, &file, &qualified, output);
        }
    }
}

fn import_files(caller: &str, source: &str) -> BTreeSet<String> {
    if !source.starts_with('.') {
        return BTreeSet::new();
    }
    let parent = Path::new(caller).parent().unwrap_or_else(|| Path::new(""));
    let path = clean(parent.join(source));
    let base = path.to_string_lossy().replace('\\', "/");
    if let Some(extension) = path.extension().and_then(|value| value.to_str()) {
        let mut files = BTreeSet::from([base]);
        let replacements: &[&str] = match extension {
            "js" => &["ts", "tsx"],
            "jsx" => &["tsx"],
            "mjs" => &["mts"],
            "cjs" => &["cts"],
            _ => &[],
        };
        for extension in replacements {
            let mut candidate = path.clone();
            candidate.set_extension(extension);
            files.insert(candidate.to_string_lossy().replace('\\', "/"));
        }
        return files;
    }
    BTreeSet::from([
        format!("{base}.ts"),
        format!("{base}.tsx"),
        format!("{base}.mts"),
        format!("{base}.cts"),
        format!("{base}/index.ts"),
        format!("{base}/index.tsx"),
        format!("{base}/index.mts"),
        format!("{base}/index.cts"),
    ])
}

fn clean(path: PathBuf) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                output.pop();
            }
            other => output.push(other.as_os_str()),
        }
    }
    output
}
