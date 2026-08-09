use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use crate::model::{
    CallTarget, FileArtifact, Import, ImportTarget, Language, LocalSymbol, Resolution,
    ResolvedCall, ResolvedProgram, ResolvedSymbol, SymbolKey,
};

pub(crate) fn program(artifacts: &[FileArtifact]) -> ResolvedProgram {
    let all_symbols = artifacts
        .iter()
        .flat_map(|artifact| artifact.symbols.iter())
        .collect::<Vec<_>>();
    let imports = artifacts
        .iter()
        .map(|artifact| (artifact.path.as_str(), artifact.imports.as_slice()))
        .collect::<BTreeMap<_, _>>();

    let mut symbols = Vec::new();
    for artifact in artifacts {
        let mut calls_by_caller = BTreeMap::<usize, Vec<ResolvedCall>>::new();
        for call in &artifact.calls {
            let caller = &artifact.symbols[call.caller];
            let resolution = target(
                caller,
                &call.target,
                artifact.language,
                imports.get(artifact.path.as_str()).copied().unwrap_or(&[]),
                &all_symbols,
            );
            calls_by_caller
                .entry(call.caller)
                .or_default()
                .push(ResolvedCall {
                    resolution,
                    location: call.location.clone(),
                    order: call.order,
                });
        }

        for (index, symbol) in artifact.symbols.iter().enumerate() {
            let mut calls = calls_by_caller.remove(&index).unwrap_or_default();
            calls.sort_by_key(|call| call.order);
            symbols.push(ResolvedSymbol {
                symbol: symbol.clone(),
                calls,
            });
        }
    }
    symbols.sort_by(|left, right| left.symbol.key.cmp(&right.symbol.key));
    ResolvedProgram { symbols }
}

fn target(
    caller: &LocalSymbol,
    target: &CallTarget,
    language: Language,
    imports: &[Import],
    symbols: &[&crate::model::LocalSymbol],
) -> Resolution {
    let mut candidates = BTreeSet::new();
    match target {
        CallTarget::Bare(name) => {
            if language == Language::Rust {
                let mut qualified = caller.module.clone();
                qualified.push(name.clone());
                same_file(&caller.key, &qualified, symbols, &mut candidates);
            } else {
                same_file(
                    &caller.key,
                    std::slice::from_ref(name),
                    symbols,
                    &mut candidates,
                );
            }
            for import in imports.iter().filter(|import| import.local == *name) {
                match &import.target {
                    ImportTarget::Named(imported) => imported_symbol(
                        &caller.key,
                        import,
                        std::slice::from_ref(imported),
                        symbols,
                        &mut candidates,
                    ),
                    ImportTarget::Default => {
                        imported_default(&caller.key, import, symbols, &mut candidates)
                    }
                    ImportTarget::Namespace => {}
                }
            }
        }
        CallTarget::Bound(_) => {}
        CallTarget::ThisMethod(name)
        | CallTarget::SelfMethod(name)
        | CallTarget::SelfAssociated(name) => {
            let mut qualified =
                caller.key.qualified_name[..caller.key.qualified_name.len() - 1].to_vec();
            qualified.push(name.clone());
            same_file(&caller.key, &qualified, symbols, &mut candidates);
        }
        CallTarget::Path(path) => {
            same_file(&caller.key, path, symbols, &mut candidates);
            if let Some((namespace, rest)) = path.split_first() {
                for import in imports.iter().filter(|import| {
                    import.local == *namespace && import.target == ImportTarget::Namespace
                }) {
                    imported_symbol(&caller.key, import, rest, symbols, &mut candidates);
                }
            }
            if language == Language::Rust {
                rust_path(caller, path, symbols, &mut candidates);
            }
        }
    }

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
    symbols: &[&crate::model::LocalSymbol],
    output: &mut BTreeSet<SymbolKey>,
) {
    output.extend(
        symbols
            .iter()
            .filter(|symbol| {
                symbol.key.language == caller.language
                    && symbol.key.file == caller.file
                    && symbol.key.qualified_name == qualified
            })
            .map(|symbol| symbol.key.clone()),
    );
}

fn imported_symbol(
    caller: &SymbolKey,
    import: &Import,
    qualified: &[String],
    symbols: &[&crate::model::LocalSymbol],
    output: &mut BTreeSet<SymbolKey>,
) {
    let files = import_files(&caller.file, &import.source);
    output.extend(
        symbols
            .iter()
            .filter(|symbol| {
                symbol.key.language == caller.language
                    && files.contains(&symbol.key.file)
                    && symbol.key.qualified_name == qualified
                    && symbol.export == crate::model::Export::Named
            })
            .map(|symbol| symbol.key.clone()),
    );
}

fn imported_default(
    caller: &SymbolKey,
    import: &Import,
    symbols: &[&LocalSymbol],
    output: &mut BTreeSet<SymbolKey>,
) {
    let files = import_files(&caller.file, &import.source);
    output.extend(
        symbols
            .iter()
            .filter(|symbol| {
                symbol.key.language == caller.language
                    && files.contains(&symbol.key.file)
                    && symbol.export == crate::model::Export::Default
            })
            .map(|symbol| symbol.key.clone()),
    );
}

fn import_files(caller: &str, source: &str) -> BTreeSet<String> {
    if !source.starts_with('.') {
        return BTreeSet::new();
    }
    let parent = Path::new(caller).parent().unwrap_or_else(|| Path::new(""));
    let path = clean(parent.join(source));
    let base = path.to_string_lossy().replace('\\', "/");
    if path.extension().is_some() {
        return BTreeSet::from([base]);
    }
    BTreeSet::from([
        format!("{base}.ts"),
        format!("{base}.tsx"),
        format!("{base}/index.ts"),
        format!("{base}/index.tsx"),
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

fn rust_path(
    caller: &LocalSymbol,
    path: &[String],
    symbols: &[&LocalSymbol],
    output: &mut BTreeSet<SymbolKey>,
) {
    let mut qualified = rust_module(&caller.key.file);
    qualified.extend(caller.module.clone());
    let mut offset = 0;
    match path.first().map(String::as_str) {
        Some("crate") => {
            qualified.clear();
            offset = 1;
        }
        Some("self") => offset = 1,
        Some("super") => {
            while path.get(offset).is_some_and(|part| part == "super") {
                qualified.pop();
                offset += 1;
            }
        }
        _ => {}
    }
    qualified.extend_from_slice(&path[offset..]);
    if qualified.is_empty() {
        return;
    }

    for symbol in symbols
        .iter()
        .filter(|symbol| symbol.key.language == Language::Rust)
    {
        let mut identity = rust_module(&symbol.key.file);
        identity.extend(symbol.key.qualified_name.clone());
        if identity == qualified {
            output.insert(symbol.key.clone());
        }
    }
}

fn rust_module(file: &str) -> Vec<String> {
    let path = Path::new(file);
    let mut parts = path
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|part| part.as_os_str().to_str())
        .filter(|part| *part != "src")
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if !matches!(stem, "lib" | "main" | "mod") {
        parts.push(stem.to_owned());
    }
    parts
}
