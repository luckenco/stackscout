use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

use crate::model::{
    CallTarget, ExportBinding, ExportName, FileArtifact, Import, ImportTarget, Language, LocalStep,
    LocalSymbol, Resolution, ResolvedCall, ResolvedProgram, ResolvedStep, ResolvedSymbol,
    SymbolKey,
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
    let exports = artifacts
        .iter()
        .map(|artifact| (artifact.path.as_str(), artifact.exports.as_slice()))
        .collect::<BTreeMap<_, _>>();

    let mut symbols = Vec::new();
    for artifact in artifacts {
        for symbol in &artifact.symbols {
            symbols.push(ResolvedSymbol {
                symbol: symbol.clone(),
                steps: resolve_steps(
                    &symbol.steps,
                    symbol,
                    artifact.language,
                    imports.get(artifact.path.as_str()).copied().unwrap_or(&[]),
                    &exports,
                    &all_symbols,
                ),
            });
        }
    }
    symbols.sort_by(|left, right| left.symbol.key.cmp(&right.symbol.key));
    ResolvedProgram { symbols }
}

fn resolve_steps(
    steps: &[LocalStep],
    caller: &LocalSymbol,
    language: Language,
    imports: &[Import],
    exports: &BTreeMap<&str, &[ExportBinding]>,
    symbols: &[&LocalSymbol],
) -> Vec<ResolvedStep> {
    steps
        .iter()
        .map(|step| match step {
            LocalStep::Call(call) => ResolvedStep::Call(ResolvedCall {
                resolution: target(caller, &call.target, language, imports, exports, symbols),
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
                children: resolve_steps(children, caller, language, imports, exports, symbols),
            },
        })
        .collect()
}

fn target(
    caller: &LocalSymbol,
    target: &CallTarget,
    language: Language,
    imports: &[Import],
    exports: &BTreeMap<&str, &[ExportBinding]>,
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
                        &ExportName::Named(imported.clone()),
                        &[],
                        exports,
                        symbols,
                        &mut candidates,
                    ),
                    ImportTarget::Default => imported_symbol(
                        &caller.key,
                        import,
                        &ExportName::Default,
                        &[],
                        exports,
                        symbols,
                        &mut candidates,
                    ),
                    ImportTarget::Namespace => {}
                }
            }
        }
        CallTarget::Bound(_) => return Resolution::Uncertain(target.clone()),
        CallTarget::ThisMethod(name)
        | CallTarget::SelfMethod(name)
        | CallTarget::SelfAssociated(name) => {
            let mut qualified =
                caller.key.qualified_name[..caller.key.qualified_name.len() - 1].to_vec();
            qualified.push(name.clone());
            same_file(&caller.key, &qualified, symbols, &mut candidates);
        }
        CallTarget::Path(path) => {
            if language == Language::TypeScript && path.len() > 1 {
                same_file(&caller.key, &static_suffix(path), symbols, &mut candidates);
            } else {
                same_file(&caller.key, path, symbols, &mut candidates);
            }
            if let Some((local, rest)) = path.split_first() {
                for import in imports.iter().filter(|import| import.local == *local) {
                    match &import.target {
                        ImportTarget::Named(exported) => imported_symbol(
                            &caller.key,
                            import,
                            &ExportName::Named(exported.clone()),
                            &static_suffix(rest),
                            exports,
                            symbols,
                            &mut candidates,
                        ),
                        ImportTarget::Default => imported_symbol(
                            &caller.key,
                            import,
                            &ExportName::Default,
                            &static_suffix(rest),
                            exports,
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
                                    exports,
                                    symbols,
                                    &mut candidates,
                                );
                            }
                        }
                    }
                }
            }
            if language == Language::Rust {
                rust_path(caller, path, symbols, &mut candidates);
            }
        }
        CallTarget::Constructor(path) => {
            let mut qualified = path.clone();
            qualified.push("constructor".to_owned());
            same_file(&caller.key, &qualified, symbols, &mut candidates);
            if let Some((local, rest)) = path.split_first() {
                for import in imports.iter().filter(|import| import.local == *local) {
                    match &import.target {
                        ImportTarget::Named(imported) => {
                            let mut suffix = rest.to_vec();
                            suffix.push("constructor".to_owned());
                            imported_symbol(
                                &caller.key,
                                import,
                                &ExportName::Named(imported.clone()),
                                &suffix,
                                exports,
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
                                    exports,
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
                            exports,
                            symbols,
                            &mut candidates,
                        ),
                        ImportTarget::Default => {}
                    }
                }
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
    exports: &BTreeMap<&str, &[ExportBinding]>,
    symbols: &[&crate::model::LocalSymbol],
    output: &mut BTreeSet<SymbolKey>,
) {
    for file in import_files(&caller.file, &import.source) {
        let Some(bindings) = exports.get(file.as_str()) else {
            continue;
        };
        for binding in bindings
            .iter()
            .filter(|binding| binding.exported == *exported)
        {
            let mut qualified = vec![binding.local.clone()];
            qualified.extend_from_slice(suffix);
            output.extend(
                symbols
                    .iter()
                    .filter(|symbol| {
                        symbol.key.language == caller.language
                            && symbol.key.file == file
                            && symbol.key.qualified_name == qualified
                    })
                    .map(|symbol| symbol.key.clone()),
            );
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
