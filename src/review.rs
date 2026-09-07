//! Scoped before/after evidence, independent of checkpoint ownership and I/O.
#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{diff, graph, model, query, repository};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Scope {
    #[serde(default)]
    pub files: Vec<String>,
    #[serde(default)]
    pub symbols: Vec<String>,
}

impl Scope {
    pub(crate) fn normalize(&mut self) -> Result<(), String> {
        if !(1..=16).contains(&(self.files.len() + self.symbols.len()))
            || self
                .files
                .iter()
                .chain(&self.symbols)
                .any(|s| s.is_empty() || s.len() > 512)
        {
            return Err("scope requires 1–16 file/symbol selectors of 1–512 bytes".to_owned());
        }
        for file in &self.files {
            repository::source_path(file).map_err(|error| error.to_string())?;
        }
        self.files.sort();
        self.files.dedup();
        self.symbols.sort();
        self.symbols.dedup();
        Ok(())
    }
}

#[derive(Default, Serialize)]
pub(crate) struct Output {
    pub witnesses: Vec<Witness>,
    pub boundaries: Vec<Boundary>,
    pub missing: Vec<Missing>,
    pub analysis_issues: Vec<String>,
    pub truncated: bool,
}

#[derive(Serialize)]
pub(crate) struct Witness {
    side: &'static str,
    relation: &'static str,
    change: &'static str,
    scope: String,
    symbol: query::Symbol,
    path: Vec<query::Call>,
    // Direct outgoing evidence when available, not a complete syntax patch.
    calls: Vec<query::Call>,
}

#[derive(Serialize)]
pub(crate) struct Boundary {
    side: &'static str,
    call: query::Call,
}

#[derive(Serialize)]
pub(crate) struct Missing {
    side: &'static str,
    selector: String,
    reason: String,
}

pub(crate) fn compare(before: &graph::Graph, after: &graph::Graph, scope: &Scope) -> Output {
    let mut output = Output::default();
    let mut bytes = 32 * 1024 - 1024;
    for (side, graph) in [("before", before), ("after", after)] {
        let mut roots = BTreeSet::new();
        for file in &scope.files {
            let language = if file.ends_with(".rs") {
                model::Language::Rust
            } else {
                model::Language::TypeScript
            };
            let start = model::SymbolKey {
                language,
                file: file.clone(),
                qualified_name: Vec::new(),
                conditions: Vec::new(),
            };
            let mut found = false;
            for key in graph
                .symbols
                .range(start..)
                .map(|(key, _)| key)
                .take_while(|key| key.language == language && key.file == *file)
            {
                found = true;
                if roots.len() >= 32 && !roots.contains(key) {
                    output.truncated = true;
                    break;
                }
                roots.insert(key);
            }
            if !found {
                missing(
                    &mut output,
                    &mut bytes,
                    side,
                    file,
                    "no indexed symbols in file",
                );
            }
        }
        for selector in &scope.symbols {
            match query::select(graph, selector) {
                Ok(key) => {
                    if roots.len() >= 32 && !roots.contains(key) {
                        output.truncated = true;
                        break;
                    }
                    roots.insert(key);
                }
                Err(reason) => {
                    output.truncated |= reason.starts_with("selector search limit reached;");
                    missing(&mut output, &mut bytes, side, selector, &reason);
                }
            }
        }
        for root in roots {
            for incoming in [false, true] {
                let direction = if incoming {
                    query::Direction::Impact
                } else {
                    query::Direction::Callees
                };
                let result = query::run(
                    graph,
                    query::Request {
                        symbol: root.to_string(),
                        depth: Some(16),
                        limit: Some(64),
                    },
                    direction,
                );
                let Ok(result) = result else {
                    output.truncated = true;
                    continue;
                };
                output.truncated |= result.truncated;
                let paths = paths(&result, incoming);
                for symbol in &result.symbols {
                    if incoming && symbol.depth == 0 {
                        continue;
                    }
                    let Ok(key) = query::select(graph, &symbol.id) else {
                        output.truncated = true;
                        continue;
                    };
                    let changed = diff::changed(key, before, after);
                    if !(changed || incoming && diff::changed(root, before, after)) {
                        continue;
                    }
                    let Some(path) = paths.get(&symbol.id) else {
                        output.truncated = true;
                        continue;
                    };
                    let relation = if symbol.depth == 0 {
                        "local"
                    } else if incoming && !changed {
                        "affected_caller"
                    } else if incoming {
                        "caller"
                    } else {
                        "dependency"
                    };
                    let change = if !changed {
                        "affected"
                    } else if !before.symbols.contains_key(key) {
                        "added"
                    } else if !after.symbols.contains_key(key) {
                        "removed"
                    } else {
                        "modified"
                    };
                    let witness = Witness {
                        side,
                        relation,
                        change,
                        scope: root.to_string(),
                        symbol: symbol.clone(),
                        path: path.clone(),
                        calls: result
                            .calls
                            .iter()
                            .filter(|call| call.caller == symbol.id)
                            .cloned()
                            .collect(),
                    };
                    if output.witnesses.len() >= 64 || !query::fits(&witness, &mut bytes) {
                        output.truncated = true;
                        return output;
                    }
                    output.witnesses.push(witness);
                }
                for call in result.calls {
                    if call.resolution == "resolved" {
                        continue;
                    }
                    if output.boundaries.iter().any(|b| {
                        b.side == side
                            && b.call.caller == call.caller
                            && b.call.callee == call.callee
                            && b.call.location == call.location
                    }) {
                        continue;
                    }
                    let boundary = Boundary { side, call };
                    if output.boundaries.len() >= 32 || !query::fits(&boundary, &mut bytes) {
                        output.truncated = true;
                        break; // Boundary noise must not prevent later changed-path witnesses.
                    }
                    output.boundaries.push(boundary);
                }
            }
        }
    }
    output
}

fn missing(
    output: &mut Output,
    bytes: &mut usize,
    side: &'static str,
    selector: &str,
    reason: &str,
) {
    let item = Missing {
        side,
        selector: selector.to_owned(),
        reason: reason.to_owned(),
    };
    if query::fits(&item, bytes) {
        output.missing.push(item);
    } else {
        output.truncated = true;
    }
}

// Query symbols are breadth-first; select one shortest witness, retaining ambiguous edge labels.
fn paths(output: &query::Output, incoming: bool) -> BTreeMap<String, Vec<query::Call>> {
    let mut paths = BTreeMap::<String, Vec<query::Call>>::new();
    for symbol in &output.symbols {
        if symbol.depth == 0 {
            paths.insert(symbol.id.clone(), Vec::new());
            continue;
        }
        for call in &output.calls {
            let (next, parent) = if incoming {
                (Some(call.caller.as_str()), call.callee.as_deref())
            } else {
                (call.callee.as_deref(), Some(call.caller.as_str()))
            };
            if next != Some(symbol.id.as_str()) {
                continue;
            }
            let Some(path) = parent.and_then(|parent| paths.get(parent)) else {
                continue;
            };
            if path.len() + 1 != symbol.depth {
                continue;
            }
            let mut path = path.clone();
            if incoming {
                path.insert(0, call.clone());
            } else {
                path.push(call.clone());
            }
            paths.insert(symbol.id.clone(), path);
            break;
        }
    }
    paths
}
