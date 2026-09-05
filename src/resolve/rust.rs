use std::collections::BTreeSet;
use std::path::Path;

use crate::model::{CallTarget, LocalSymbol, Resolution, RustImport, SourceRange, SymbolKey};

pub(super) fn resolve(
    caller: &LocalSymbol,
    target: &CallTarget,
    location: &SourceRange,
    imports: &[RustImport],
    symbols: &super::Index<'_>,
) -> Resolution {
    let mut candidates = BTreeSet::new();
    let path = match target {
        CallTarget::Bare(name) => std::slice::from_ref(name),
        CallTarget::Path(path) => path,
        CallTarget::SelfMethod(name) | CallTarget::SelfAssociated(name) => {
            let mut path =
                caller.key.qualified_name[..caller.key.qualified_name.len() - 1].to_vec();
            path.push(name.clone());
            super::same_file(&caller.key, &path, symbols, &mut candidates);
            return super::resolution(target, candidates);
        }
        CallTarget::Bound(_) => return Resolution::Uncertain(target.clone()),
        _ => return Resolution::Unresolved(target.clone()),
    };
    let mut caller_module = module(&caller.key.file);
    caller_module.extend_from_slice(&caller.module);
    let position = (location.start_line, location.start_column);
    let unqualified = path
        .first()
        .is_some_and(|part| !matches!(part.as_str(), "crate" | "self" | "super" | ""));
    let mut bindings = imports
        .iter()
        .filter(|import| {
            import.module == caller.module
                && unqualified
                && (path.first() == Some(&import.local) || import.local == "*")
                && (import.scope.start_line, import.scope.start_column) <= position
                && position < (import.scope.end_line, import.scope.end_column)
        })
        .collect::<Vec<_>>();
    if let Some(scope) = bindings
        .iter()
        .map(|import| (import.scope.start_line, import.scope.start_column))
        .max()
    {
        // Named bindings beat globs in the same scope. An inner glob can shadow
        // outer names, but we cannot determine its members without expanding it.
        bindings.retain(|import| {
            (import.scope.start_line, import.scope.start_column) == scope && import.local != "*"
        });
        if bindings.is_empty() {
            return Resolution::Unresolved(target.clone());
        }
        let mut unresolved = false;
        for import in bindings {
            let Some(mut qualified) = qualify(&caller_module, &import.path) else {
                unresolved = true;
                continue;
            };
            qualified.extend_from_slice(&path[1..]);
            let found = matching(&qualified, symbols);
            unresolved |= found.is_empty();
            candidates.extend(found);
        }
        if unresolved && !candidates.is_empty() {
            return Resolution::Unresolved(target.clone());
        }
    } else if let Some(qualified) = qualify(&caller_module, path) {
        candidates = matching(&qualified, symbols);
    }
    super::resolution(target, candidates)
}

fn qualify(module: &[String], path: &[String]) -> Option<Vec<String>> {
    let mut qualified = module.to_vec();
    let mut offset = 0;
    match path.first().map(String::as_str) {
        Some("") => return None, // Absolute external paths are not local crate paths.
        Some("crate") => {
            qualified.clear();
            offset = 1;
        }
        Some("self") => offset = 1,
        Some("super") => {
            while path.get(offset).is_some_and(|part| part == "super") {
                qualified.pop()?;
                offset += 1;
            }
        }
        _ => {}
    }
    qualified.extend_from_slice(&path[offset..]);
    Some(qualified)
}

fn matching(qualified: &[String], symbols: &super::Index<'_>) -> BTreeSet<SymbolKey> {
    symbols
        .rust
        .get(qualified)
        .into_iter()
        .flatten()
        .copied()
        .cloned()
        .collect()
}

pub(super) fn module(file: &str) -> Vec<String> {
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
