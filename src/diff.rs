use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::graph::{Graph, LocalShape};
use crate::model::{Resolution, ResolvedStep, SourceRange, SymbolKey};

const MAX_DEPTH: usize = 32;
const MAX_NODES: usize = 10_000;

// Exact keys: hash collisions are resolved by equality, not treated as equal trees.
type Fingerprints<'a> = HashMap<(String, Option<LocalShape<'a>>, Vec<usize>), usize>;

#[derive(Clone, Debug)]
pub(crate) struct GraphDiff<'a> {
    pub(crate) roots: Vec<RootDiff<'a>>,
    /// Symbols with no changed paths reachable in either graph.
    pub(crate) unchanged: BTreeSet<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct RootDiff<'a> {
    pub(crate) before: Option<TreeNode<'a>>,
    pub(crate) after: Option<TreeNode<'a>>,
}

#[derive(Clone, Debug)]
pub(crate) struct TreeNode<'a> {
    pub(crate) identity: String,
    pub(crate) label: String,
    pub(crate) location_file: Option<String>,
    pub(crate) definition_location: Option<SourceRange>,
    pub(crate) call_site_location: Option<SourceRange>,
    pub(crate) hide_when_unchanged: bool,
    pub(crate) local_shape: Option<LocalShape<'a>>,
    pub(crate) children: Vec<TreeNode<'a>>,
    // Assigned together for both sides of a comparison; nodes are immutable afterwards.
    fingerprint: Option<usize>,
}

impl<'a> TreeNode<'a> {
    pub(crate) fn fingerprint(&self) -> usize {
        self.fingerprint
            .expect("tree fingerprints must be assigned before comparison")
    }

    fn identify(&mut self, keys: &mut Fingerprints<'a>) {
        for child in &mut self.children {
            child.identify(keys);
        }
        let key = (
            self.identity.clone(),
            self.local_shape,
            self.children.iter().map(Self::fingerprint).collect(),
        );
        let next = keys.len();
        self.fingerprint = Some(*keys.entry(key).or_insert(next));
    }

    pub(crate) fn contains(&self, identity: &str) -> bool {
        self.identity == identity || self.children.iter().any(|child| child.contains(identity))
    }
}

pub(crate) fn compare<'a>(before: &'a Graph, after: &'a Graph) -> GraphDiff<'a> {
    let keys = before
        .symbols
        .keys()
        .chain(after.symbols.keys())
        .cloned()
        .collect::<BTreeSet<_>>();
    let direct = keys
        .iter()
        .filter(|key| changed(key, before, after))
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut affected = direct.clone();
    let mut pending = direct.iter().cloned().collect::<Vec<_>>();
    while let Some(key) = pending.pop() {
        for caller in before
            .incoming
            .get(&key)
            .into_iter()
            .flatten()
            .chain(after.incoming.get(&key).into_iter().flatten())
        {
            if affected.insert(caller.clone()) {
                pending.push(caller.clone());
            }
        }
    }
    let unchanged = keys
        .difference(&affected)
        .map(ToString::to_string)
        .collect();

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

    let mut fingerprints = Fingerprints::new();
    let mut minimal = minimal_roots(&roots, before);
    minimal.extend(minimal_roots(&roots, after));
    let mut output = minimal
        .into_iter()
        .filter_map(|key| {
            let root = root_diff(before, after, &key, &mut fingerprints);
            root.is_changed().then_some(root)
        })
        .collect::<Vec<_>>();

    for key in direct {
        let identity = key.to_string();
        if !output.iter().any(|root| root.contains(&identity)) {
            output.push(root_diff(before, after, &key, &mut fingerprints));
        }
    }
    output.sort_by(|left, right| left.identity().cmp(right.identity()));

    GraphDiff {
        roots: output,
        unchanged,
    }
}

impl RootDiff<'_> {
    fn is_changed(&self) -> bool {
        match (&self.before, &self.after) {
            (Some(before), Some(after)) => before.fingerprint() != after.fingerprint(),
            (None, None) => false,
            _ => true,
        }
    }

    fn contains(&self, identity: &str) -> bool {
        self.before
            .iter()
            .chain(&self.after)
            .any(|tree| tree.contains(identity))
    }

    fn identity(&self) -> &str {
        self.after
            .as_ref()
            .or(self.before.as_ref())
            .map(|tree| tree.identity.as_str())
            .expect("a diff root exists in at least one graph")
    }
}

fn root_diff<'a>(
    before: &'a Graph,
    after: &'a Graph,
    key: &SymbolKey,
    fingerprints: &mut Fingerprints<'a>,
) -> RootDiff<'a> {
    let mut root = RootDiff {
        before: tree(before, key),
        after: tree(after, key),
    };
    for tree in root.before.iter_mut().chain(&mut root.after) {
        tree.identify(fingerprints);
    }
    root
}

fn minimal_roots(candidates: &BTreeSet<SymbolKey>, graph: &Graph) -> BTreeSet<SymbolKey> {
    let callers = candidates
        .iter()
        .filter(|key| graph.symbols.contains_key(*key))
        .map(|key| (key.clone(), callers_in(graph, key, candidates)))
        .collect::<BTreeMap<_, _>>();

    callers
        .iter()
        .filter(|(key, incoming)| {
            let outside_cycle = incoming
                .iter()
                .any(|caller| !callers.get(caller).is_some_and(|seen| seen.contains(*key)));
            if outside_cycle {
                return false;
            }
            incoming.iter().all(|caller| *key < caller)
        })
        .map(|(key, _)| key.clone())
        .collect()
}

pub(crate) fn changed(key: &SymbolKey, before: &Graph, after: &Graph) -> bool {
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

    minimal_roots(&exported, graph)
}

fn callers_in(
    graph: &Graph,
    key: &SymbolKey,
    candidates: &BTreeSet<SymbolKey>,
) -> BTreeSet<SymbolKey> {
    let mut found = BTreeSet::new();
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
        if candidates.contains(&caller) {
            found.insert(caller.clone());
        }
        if let Some(callers) = graph.incoming.get(&caller) {
            pending.extend(callers.iter().cloned());
        }
    }
    found
}

fn tree<'a>(graph: &'a Graph, root: &SymbolKey) -> Option<TreeNode<'a>> {
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

fn expand<'a>(
    graph: &'a Graph,
    key: &SymbolKey,
    call_site: Option<(&str, &SourceRange)>,
    depth: usize,
    nodes: &mut usize,
    path: &mut BTreeSet<SymbolKey>,
) -> TreeNode<'a> {
    *nodes += 1;
    let node = &graph.symbols[key];
    let mut output = TreeNode {
        fingerprint: None,
        identity: key.to_string(),
        label: key.to_string(),
        location_file: Some(
            call_site
                .map_or(key.file.as_str(), |(file, _)| file)
                .to_owned(),
        ),
        definition_location: Some(node.symbol.location.clone()),
        call_site_location: call_site.map(|(_, location)| location.clone()),
        hide_when_unchanged: false,
        local_shape: Some(node.local_shape()),
        children: Vec::new(),
    };
    if depth >= MAX_DEPTH || *nodes >= MAX_NODES {
        output.children.push(truncated());
        return output;
    }

    path.insert(key.clone());
    output.children = expand_steps(graph, &node.steps, &key.file, depth, nodes, path);
    path.remove(key);
    output
}

fn expand_steps<'a>(
    graph: &'a Graph,
    steps: &[ResolvedStep],
    file: &str,
    depth: usize,
    nodes: &mut usize,
    path: &mut BTreeSet<SymbolKey>,
) -> Vec<TreeNode<'a>> {
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
                    fingerprint: None,
                    identity: format!("[branch:{key}]"),
                    label: label.clone(),
                    location_file: Some(file.to_owned()),
                    definition_location: None,
                    call_site_location: Some(location.clone()),
                    hide_when_unchanged: false,
                    local_shape: None,
                    children: if depth >= MAX_DEPTH {
                        vec![truncated()]
                    } else {
                        expand_steps(graph, children, file, depth + 1, nodes, path)
                    },
                });
            }
            ResolvedStep::Call(call) => match &call.resolution {
                Resolution::Resolved(callee) if path.contains(callee) => {
                    *nodes += 1;
                    output.push(cycle(callee, file, &call.location));
                }
                Resolution::Resolved(callee) => output.push(expand(
                    graph,
                    callee,
                    Some((file, &call.location)),
                    depth + 1,
                    nodes,
                    path,
                )),
                Resolution::Unresolved(target) => {
                    *nodes += 1;
                    output.push(TreeNode {
                        fingerprint: None,
                        identity: call.resolution.identity(),
                        label: format!("[unresolved: {target}]"),
                        location_file: Some(file.to_owned()),
                        definition_location: None,
                        call_site_location: Some(call.location.clone()),
                        hide_when_unchanged: true,
                        local_shape: None,
                        children: Vec::new(),
                    });
                }
                Resolution::Uncertain(target) => {
                    *nodes += 1;
                    output.push(TreeNode {
                        fingerprint: None,
                        identity: call.resolution.identity(),
                        label: format!("[uncertain: {target} is locally bound]"),
                        location_file: Some(file.to_owned()),
                        definition_location: None,
                        call_site_location: Some(call.location.clone()),
                        hide_when_unchanged: true,
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
                            children.push(cycle(candidate, file, &call.location));
                        } else {
                            children.push(expand(
                                graph,
                                candidate,
                                Some((file, &call.location)),
                                depth + 1,
                                nodes,
                                path,
                            ));
                        }
                    }
                    output.push(TreeNode {
                        fingerprint: None,
                        identity: call.resolution.identity(),
                        label: format!("[ambiguous: {target} -> {label}]"),
                        location_file: Some(file.to_owned()),
                        definition_location: None,
                        call_site_location: Some(call.location.clone()),
                        hide_when_unchanged: false,
                        local_shape: None,
                        children,
                    });
                }
            },
        }
    }
    output
}

fn cycle(key: &SymbolKey, file: &str, location: &SourceRange) -> TreeNode<'static> {
    TreeNode {
        fingerprint: None,
        identity: key.to_string(),
        label: format!("{key} [cycle]"),
        location_file: Some(file.to_owned()),
        definition_location: None,
        call_site_location: Some(location.clone()),
        hide_when_unchanged: false,
        local_shape: None,
        children: Vec::new(),
    }
}

fn truncated() -> TreeNode<'static> {
    TreeNode {
        fingerprint: None,
        identity: "[truncated]".to_owned(),
        label: "[truncated]".to_owned(),
        location_file: None,
        definition_location: None,
        call_site_location: None,
        hide_when_unchanged: false,
        local_shape: None,
        children: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{Fingerprints, truncated};

    #[test]
    fn fingerprints_preserve_child_boundaries_and_ignore_display_metadata() {
        let mut left = truncated();
        left.identity = "root".to_owned();
        left.children = vec![truncated(), truncated()];
        left.children[0].identity = "a".to_owned();
        left.children[1].identity = "b".to_owned();
        let mut right = left.clone();
        right.children.truncate(1);
        right.children[0].identity = "a{}b".to_owned();
        // Recursive string concatenation gave both trees "root{a{}b{}}".
        let mut keys = Fingerprints::new();
        left.identify(&mut keys);
        right.identify(&mut keys);
        assert_ne!(left.fingerprint(), right.fingerprint());
        let mut moved = left.clone();
        moved.label = "display only".to_owned();
        moved.location_file = Some("elsewhere.rs".to_owned());
        moved.identify(&mut keys);
        assert_eq!(left.fingerprint(), moved.fingerprint());
        assert_eq!(keys.len(), 5);
    }
}
