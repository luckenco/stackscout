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
    let associated;
    let path = match target {
        CallTarget::Bare(name) => std::slice::from_ref(name),
        CallTarget::Path(path) => path,
        CallTarget::TypedMethod { owner, method, .. } => {
            // A generic type parameter or local type binding may have this spelling.
            if owner
                .first()
                .is_some_and(|part| part == "Self" || part.contains('<'))
            {
                return Resolution::Uncertain(target.clone());
            }
            associated = owner
                .iter()
                .cloned()
                .chain([method.clone()])
                .collect::<Vec<_>>();
            &associated
        }
        CallTarget::SelfMethod(name) | CallTarget::SelfAssociated(name) => {
            let owner = &caller.key.qualified_name
                [caller.module.len()..caller.key.qualified_name.len() - 1];
            // Lexical trait ownership does not establish Rust's dispatch precedence.
            if owner.is_empty() || owner[0].starts_with('<') {
                return Resolution::Uncertain(target.clone());
            }
            associated = owner
                .iter()
                .cloned()
                .chain([name.clone()])
                .collect::<Vec<_>>();
            &associated
        }
        CallTarget::Bound(_) | CallTarget::Opaque(_) => {
            return Resolution::Uncertain(target.clone());
        }
        _ => return Resolution::Unresolved(target.clone()),
    };
    let root = symbols.rust_owner(&caller.key.file);
    let mut caller_module = module(&caller.key.file, &root);
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
            let found = matching(&root, &qualified, symbols);
            unresolved |= found.is_empty();
            candidates.extend(found);
        }
        if unresolved && !candidates.is_empty() {
            return Resolution::Unresolved(target.clone());
        }
    } else if let Some(qualified) = qualify(&caller_module, path) {
        candidates = matching(&root, &qualified, symbols);
    }
    if matches!(target, CallTarget::TypedMethod { .. })
        && qualified_type_has_trait_method(&root, path, symbols)
    {
        return Resolution::Uncertain(target.clone());
    }
    super::resolution(target, candidates)
}

fn qualified_type_has_trait_method(
    root: &super::RustOwner,
    path: &[String],
    symbols: &super::Index<'_>,
) -> bool {
    let Some((method, owner)) = path.split_last() else {
        return false;
    };
    owner.last().is_some_and(|ty| {
        symbols
            .rust_trait_methods
            .contains(&(root.clone(), ty.clone(), method.clone()))
    })
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

fn matching(
    root: &super::RustOwner,
    qualified: &[String],
    symbols: &super::Index<'_>,
) -> BTreeSet<SymbolKey> {
    symbols
        .rust
        .get(&(root.clone(), qualified.to_vec()))
        .into_iter()
        .flatten()
        .copied()
        .cloned()
        .collect()
}

pub(super) fn is_root(file: &str) -> bool {
    let path = Path::new(file);
    let Some(parent) = path.parent() else {
        return false;
    };
    (parent.file_name().is_some_and(|part| part == "src")
        && matches!(
            path.file_name().and_then(|part| part.to_str()),
            Some("lib.rs" | "main.rs")
        ))
        || (parent.file_name().is_some_and(|part| part == "bin")
            && parent
                .parent()
                .is_some_and(|part| part.file_name().is_some_and(|name| name == "src"))
            && path.extension().is_some_and(|extension| extension == "rs"))
        || (path.file_name().is_some_and(|name| name == "main.rs")
            && parent
                .parent()
                .is_some_and(|part| part.file_name().is_some_and(|name| name == "bin"))
            && parent
                .parent()
                .and_then(Path::parent)
                .is_some_and(|part| part.file_name().is_some_and(|name| name == "src")))
}

pub(super) fn owners(
    artifacts: &[&crate::model::FileArtifact],
    roots: &BTreeSet<String>,
) -> std::collections::BTreeMap<String, super::RustOwner> {
    use std::collections::BTreeMap;
    let files = artifacts
        .iter()
        .filter(|artifact| artifact.language == crate::model::Language::Rust)
        .map(|artifact| (artifact.path.as_str(), *artifact))
        .collect::<BTreeMap<_, _>>();
    let mut claims = BTreeMap::<String, BTreeSet<String>>::new();
    let mut pending = roots
        .iter()
        .map(|root| (root.clone(), root.clone()))
        .collect::<Vec<_>>();
    while let Some((file, root)) = pending.pop() {
        if !claims.entry(file.clone()).or_default().insert(root.clone()) {
            continue;
        }
        let Some(artifact) = files.get(file.as_str()) else {
            continue;
        };
        for (inline, name) in &artifact.rust_modules {
            let path = Path::new(&file);
            let mut base = path.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
            if !matches!(
                path.file_stem().and_then(|stem| stem.to_str()),
                Some("lib" | "main" | "mod")
            ) {
                base.push(path.file_stem().unwrap_or_default());
            }
            base.extend(inline);
            let module = base.join(name);
            for candidate in [module.with_extension("rs"), module.join("mod.rs")] {
                let candidate = candidate.to_string_lossy().replace('\\', "/");
                if files.contains_key(candidate.as_str()) && !roots.contains(&candidate) {
                    pending.push((candidate, root.clone()));
                }
            }
        }
    }
    claims
        .into_iter()
        .map(|(file, owners)| {
            let owner = if owners.len() == 1 {
                super::RustOwner::Root(owners.into_iter().next().unwrap())
            } else {
                super::RustOwner::Unknown(file.clone())
            };
            (file, owner)
        })
        .collect()
}

pub(super) fn owner(file: &str, roots: &BTreeSet<String>) -> super::RustOwner {
    if roots.is_empty() {
        return super::RustOwner::Root(String::new());
    }
    if roots.contains(file) {
        return super::RustOwner::Root(file.to_owned());
    }
    let path = Path::new(file);
    let src = path
        .ancestors()
        .find(|part| part.file_name().is_some_and(|name| name == "src"));
    if let Some(src) = src {
        let owners = roots
            .iter()
            .filter(|root| Path::new(root).starts_with(src))
            .collect::<Vec<_>>();
        if owners.len() == 1 {
            return super::RustOwner::Root(owners[0].clone());
        }
    }
    super::RustOwner::Unknown(file.to_owned())
}

pub(super) fn module(file: &str, root: &super::RustOwner) -> Vec<String> {
    let path = Path::new(file);
    let mut parts = path
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .filter_map(|part| part.as_os_str().to_str())
        .filter(|part| *part != "src")
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if let super::RustOwner::Root(root) = root {
        let root = Path::new(root);
        if let Some(base) = root.parent()
            && let Ok(relative) = path.strip_prefix(base)
            && file != root.to_string_lossy()
        {
            parts = relative
                .parent()
                .into_iter()
                .flat_map(Path::components)
                .filter_map(|part| part.as_os_str().to_str().map(str::to_owned))
                .collect();
            // Flat bin roots (`src/bin/tool.rs`) load children from `tool/`,
            // but that directory is not part of the crate's module path.
            if root
                .parent()
                .is_some_and(|parent| parent.file_name().is_some_and(|name| name == "bin"))
                && parts.first().is_some_and(|part| {
                    Some(part.as_str()) == root.file_stem().and_then(|stem| stem.to_str())
                })
            {
                parts.remove(0);
            }
        } else if file == root.to_string_lossy() {
            parts.clear();
        }
    }
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if !matches!(stem, "lib" | "main" | "mod")
        && !matches!(root, super::RustOwner::Root(root) if root == file)
    {
        parts.push(stem.to_owned());
    }
    parts
}
