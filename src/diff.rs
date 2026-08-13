use std::collections::BTreeSet;

use crate::graph::{Graph, LocalShape};
use crate::model::{Resolution, ResolvedStep, SourceRange, SymbolKey};

const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 10_000;

#[derive(Clone, Debug)]
pub(crate) struct GraphDiff {
    pub(crate) roots: Vec<RootDiff>,
}

#[derive(Clone, Debug)]
pub(crate) struct RootDiff {
    pub(crate) before: Option<TreeNode>,
    pub(crate) after: Option<TreeNode>,
}

#[derive(Clone, Debug)]
pub(crate) struct TreeNode {
    pub(crate) identity: String,
    pub(crate) label: String,
    pub(crate) definition_location: Option<SourceRange>,
    pub(crate) call_site_location: Option<SourceRange>,
    pub(crate) local_shape: Option<LocalShape>,
    pub(crate) children: Vec<TreeNode>,
}

impl TreeNode {
    pub(crate) fn fingerprint(&self) -> String {
        let mut value = self.identity.clone();
        value.push('{');
        if let Some(shape) = &self.local_shape {
            value.push_str(&shape.fingerprint());
        }
        for child in &self.children {
            value.push_str(&child.fingerprint());
        }
        value.push('}');
        value
    }
}

pub(crate) fn compare(before: &Graph, after: &Graph) -> GraphDiff {
    let keys = before
        .symbols
        .keys()
        .chain(after.symbols.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let direct = keys
        .into_iter()
        .filter(|key| changed(key, before, after))
        .collect::<BTreeSet<_>>();

    let mut roots = BTreeSet::new();
    for key in &direct {
        let mut affected = exported_callers(before, key);
        affected.extend(exported_callers(after, key));
        if affected.is_empty() {
            roots.insert(key.clone());
        } else {
            roots.extend(affected);
        }
    }

    GraphDiff {
        roots: roots
            .into_iter()
            .map(|key| RootDiff {
                before: tree(before, &key),
                after: tree(after, &key),
            })
            .collect(),
    }
}

fn changed(key: &SymbolKey, before: &Graph, after: &Graph) -> bool {
    match (before.symbols.get(key), after.symbols.get(key)) {
        (Some(before), Some(after)) => before.local_shape() != after.local_shape(),
        (None, None) => false,
        _ => true,
    }
}

fn exported_callers(graph: &Graph, changed: &SymbolKey) -> BTreeSet<SymbolKey> {
    let mut exported = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut pending = vec![changed.clone()];
    while let Some(key) = pending.pop() {
        if !seen.insert(key.clone()) {
            continue;
        }
        if graph
            .symbols
            .get(&key)
            .is_some_and(|node| node.symbol.visibility.is_public())
        {
            exported.insert(key.clone());
        }
        if let Some(callers) = graph.incoming.get(&key) {
            pending.extend(callers.iter().cloned());
        }
    }

    let roots = exported
        .iter()
        .filter(|key| !has_exported_caller(graph, key, &exported))
        .cloned()
        .collect::<BTreeSet<_>>();
    if roots.is_empty() && !exported.is_empty() {
        return exported.into_iter().take(1).collect();
    }
    roots
}

fn has_exported_caller(graph: &Graph, key: &SymbolKey, exported: &BTreeSet<SymbolKey>) -> bool {
    let mut seen = BTreeSet::from([key.clone()]);
    let mut pending = graph
        .incoming
        .get(key)
        .into_iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>();
    while let Some(caller) = pending.pop() {
        if !seen.insert(caller.clone()) {
            continue;
        }
        if exported.contains(&caller) {
            return true;
        }
        if let Some(callers) = graph.incoming.get(&caller) {
            pending.extend(callers.iter().cloned());
        }
    }
    false
}

fn tree(graph: &Graph, root: &SymbolKey) -> Option<TreeNode> {
    graph.symbols.get(root)?;
    let mut nodes = 0;
    Some(expand(
        graph,
        root,
        None,
        0,
        &mut nodes,
        &mut BTreeSet::new(),
    ))
}

fn expand(
    graph: &Graph,
    key: &SymbolKey,
    call_site_location: Option<SourceRange>,
    depth: usize,
    nodes: &mut usize,
    path: &mut BTreeSet<SymbolKey>,
) -> TreeNode {
    *nodes += 1;
    let node = &graph.symbols[key];
    let mut output = TreeNode {
        identity: key.to_string(),
        label: key.to_string(),
        definition_location: Some(node.symbol.location.clone()),
        call_site_location,
        local_shape: Some(node.local_shape()),
        children: Vec::new(),
    };
    if depth >= MAX_DEPTH || *nodes >= MAX_NODES {
        output.children.push(truncated());
        return output;
    }

    path.insert(key.clone());
    output.children = expand_steps(graph, &node.steps, depth, nodes, path);
    path.remove(key);
    output
}

fn expand_steps(
    graph: &Graph,
    steps: &[ResolvedStep],
    depth: usize,
    nodes: &mut usize,
    path: &mut BTreeSet<SymbolKey>,
) -> Vec<TreeNode> {
    let mut output = Vec::new();
    for step in steps {
        if *nodes >= MAX_NODES {
            output.push(truncated());
            break;
        }
        match step {
            ResolvedStep::Branch {
                key,
                label,
                location,
                children,
            } => {
                *nodes += 1;
                output.push(TreeNode {
                    identity: format!("[branch:{key}]"),
                    label: label.clone(),
                    definition_location: None,
                    call_site_location: Some(location.clone()),
                    local_shape: None,
                    children: if depth >= MAX_DEPTH {
                        vec![truncated()]
                    } else {
                        expand_steps(graph, children, depth + 1, nodes, path)
                    },
                });
            }
            ResolvedStep::Call(call) => match &call.resolution {
                Resolution::Resolved(callee) if path.contains(callee) => {
                    *nodes += 1;
                    output.push(cycle(callee, &call.location));
                }
                Resolution::Resolved(callee) => output.push(expand(
                    graph,
                    callee,
                    Some(call.location.clone()),
                    depth + 1,
                    nodes,
                    path,
                )),
                Resolution::Unresolved(target) => {
                    *nodes += 1;
                    output.push(TreeNode {
                        identity: call.resolution.identity(),
                        label: format!("[unresolved: {target}]"),
                        definition_location: None,
                        call_site_location: Some(call.location.clone()),
                        local_shape: None,
                        children: Vec::new(),
                    });
                }
                Resolution::Uncertain(target) => {
                    *nodes += 1;
                    output.push(TreeNode {
                        identity: call.resolution.identity(),
                        label: format!("[uncertain: {target} is locally bound]"),
                        definition_location: None,
                        call_site_location: Some(call.location.clone()),
                        local_shape: None,
                        children: Vec::new(),
                    });
                }
                Resolution::Ambiguous { target, candidates } => {
                    *nodes += 1;
                    let label = candidates
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ");
                    let mut children = Vec::new();
                    for candidate in candidates {
                        if *nodes >= MAX_NODES {
                            children.push(truncated());
                            break;
                        }
                        if path.contains(candidate) {
                            *nodes += 1;
                            children.push(cycle(candidate, &call.location));
                        } else {
                            children.push(expand(
                                graph,
                                candidate,
                                Some(call.location.clone()),
                                depth + 1,
                                nodes,
                                path,
                            ));
                        }
                    }
                    output.push(TreeNode {
                        identity: call.resolution.identity(),
                        label: format!("[ambiguous: {target} -> {label}]"),
                        definition_location: None,
                        call_site_location: Some(call.location.clone()),
                        local_shape: None,
                        children,
                    });
                }
            },
        }
    }
    output
}

fn cycle(key: &SymbolKey, location: &SourceRange) -> TreeNode {
    TreeNode {
        identity: key.to_string(),
        label: format!("{key} [cycle]"),
        definition_location: None,
        call_site_location: Some(location.clone()),
        local_shape: None,
        children: Vec::new(),
    }
}

fn truncated() -> TreeNode {
    TreeNode {
        identity: "[truncated]".to_owned(),
        label: "[truncated]".to_owned(),
        definition_location: None,
        call_site_location: None,
        local_shape: None,
        children: Vec::new(),
    }
}
