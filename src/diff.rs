use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::graph::{Graph, LocalShape};
use crate::model::{Resolution, ResolvedStep, SourceRange, SymbolKey};

const MAX_DEPTH: usize = 32;
// Shared across all roots, independently for each side.
const MAX_NODES: usize = 10_000;
const MAX_WORK: usize = 20_000;
const MAX_WITNESSES: usize = 8;

fn spend(work: &mut usize) -> bool {
    if *work == 0 {
        return false;
    }
    *work -= 1;
    true
}

// Exact keys: hash collisions are resolved by equality, not treated as equal trees.
type Fingerprints<'a> = HashMap<(String, Option<LocalShape<'a>>, Vec<usize>), usize>;

#[derive(Clone, Debug)]
pub(crate) struct GraphDiff<'a> {
    pub(crate) roots: Vec<RootDiff<'a>>,
    /// Resolution, selection, or expansion is incomplete.
    pub(crate) truncated: bool,
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

    fn identities(&self, output: &mut BTreeSet<String>) {
        output.insert(self.identity.clone());
        for child in &self.children {
            child.identities(output);
        }
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
    // Computing safe omissions must not consume root-selection work.
    let mut closure_work = MAX_WORK;
    let mut affected = direct.clone();
    let mut pending = direct.iter().cloned().collect::<Vec<_>>();
    while let Some(key) = pending.pop() {
        if !spend(&mut closure_work) {
            break;
        }
        for caller in before
            .incoming
            .get(&key)
            .into_iter()
            .flatten()
            .chain(after.incoming.get(&key).into_iter().flatten())
        {
            if !spend(&mut closure_work) {
                break;
            }
            if affected.insert(caller.clone()) {
                pending.push(caller.clone());
            }
        }
    }
    let unchanged = if closure_work == 0 {
        BTreeSet::new()
    } else {
        keys.difference(&affected)
            .map(ToString::to_string)
            .collect()
    };

    let mut work = MAX_WORK;
    let mut roots = BTreeSet::new();
    for key in &direct {
        if !spend(&mut work) {
            break;
        }
        let mut affected = exported_callers(before, key, &mut work);
        affected.extend(exported_callers(after, key, &mut work));
        if affected.is_empty() {
            roots.insert(key.clone());
        } else {
            roots.extend(affected);
        }
    }

    let mut fingerprints = Fingerprints::new();
    let mut minimal = minimal_roots(&roots, before, &mut work);
    minimal.extend(minimal_roots(&roots, after, &mut work));
    let mut before_nodes = 0;
    let mut after_nodes = 0;
    let mut output = Vec::new();
    for key in minimal {
        if before_nodes >= MAX_NODES || after_nodes >= MAX_NODES {
            break;
        }
        let root = root_diff(
            before,
            after,
            &key,
            &mut fingerprints,
            &mut before_nodes,
            &mut after_nodes,
        );
        if root.is_changed() {
            output.push(root);
        }
    }

    let mut represented = BTreeSet::new();
    for root in &output {
        for tree in root.before.iter().chain(&root.after) {
            tree.identities(&mut represented);
        }
    }
    for key in direct {
        // The node cap bounds fallback expansion even after selection runs out.
        if before_nodes >= MAX_NODES || after_nodes >= MAX_NODES {
            break;
        }
        if !represented.contains(&key.to_string()) {
            let root = root_diff(
                before,
                after,
                &key,
                &mut fingerprints,
                &mut before_nodes,
                &mut after_nodes,
            );
            for tree in root.before.iter().chain(&root.after) {
                tree.identities(&mut represented);
            }
            output.push(root);
        }
    }
    output.sort_by(|left, right| left.identity().cmp(right.identity()));

    GraphDiff {
        roots: output,
        truncated: closure_work == 0
            || work == 0
            || before_nodes >= MAX_NODES
            || after_nodes >= MAX_NODES
            || before
                .symbols
                .values()
                .chain(after.symbols.values())
                .any(|node| incomplete(&node.steps)),
        unchanged,
    }
}

fn incomplete(steps: &[ResolvedStep]) -> bool {
    steps.iter().any(|step| match step {
        ResolvedStep::Call(call) => match &call.resolution {
            Resolution::Limited { .. } => true,
            Resolution::Ambiguous { candidates, .. } => candidates.len() > MAX_WITNESSES,
            _ => false,
        },
        ResolvedStep::Branch { children, .. } => incomplete(children),
    })
}

impl RootDiff<'_> {
    fn is_changed(&self) -> bool {
        match (&self.before, &self.after) {
            (Some(before), Some(after)) => before.fingerprint() != after.fingerprint(),
            (None, None) => false,
            _ => true,
        }
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
    before_nodes: &mut usize,
    after_nodes: &mut usize,
) -> RootDiff<'a> {
    let mut root = RootDiff {
        before: tree(before, key, before_nodes),
        after: tree(after, key, after_nodes),
    };
    for tree in root.before.iter_mut().chain(&mut root.after) {
        tree.identify(fingerprints);
    }
    root
}

fn minimal_roots(
    candidates: &BTreeSet<SymbolKey>,
    graph: &Graph,
    work: &mut usize,
) -> BTreeSet<SymbolKey> {
    if *work == 0 {
        return candidates.clone();
    }
    let callers = candidates
        .iter()
        .filter(|key| graph.symbols.contains_key(*key))
        .map(|key| (key.clone(), callers_in(graph, key, candidates, work)))
        .collect::<BTreeMap<_, _>>();

    if *work == 0 {
        return candidates.clone();
    }
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

fn exported_callers(graph: &Graph, changed: &SymbolKey, work: &mut usize) -> BTreeSet<SymbolKey> {
    let mut exported = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut pending = vec![changed.clone()];
    while let Some(key) = pending.pop() {
        if !spend(work) {
            break;
        }
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
            for caller in callers {
                if !spend(work) {
                    break;
                }
                pending.push(caller.clone());
            }
        }
    }

    minimal_roots(&exported, graph, work)
}

fn callers_in(
    graph: &Graph,
    key: &SymbolKey,
    candidates: &BTreeSet<SymbolKey>,
    work: &mut usize,
) -> BTreeSet<SymbolKey> {
    let mut found = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut pending = vec![key.clone()];
    while let Some(caller) = pending.pop() {
        if !spend(work) {
            break;
        }
        if !seen.insert(caller.clone()) {
            continue;
        }
        if caller != *key && candidates.contains(&caller) {
            found.insert(caller.clone());
        }
        if let Some(callers) = graph.incoming.get(&caller) {
            for caller in callers {
                if !spend(work) {
                    break;
                }
                pending.push(caller.clone());
            }
        }
    }
    found
}

fn tree<'a>(graph: &'a Graph, root: &SymbolKey, nodes: &mut usize) -> Option<TreeNode<'a>> {
    graph.symbols.get(root)?;
    Some(expand(graph, root, None, 0, nodes, &mut BTreeSet::new()))
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
                        label: match target {
                            crate::model::CallTarget::Bound(_) => {
                                format!("[uncertain: {target} is locally bound]")
                            }
                            _ => format!("[uncertain: {target}]"),
                        },
                        location_file: Some(file.to_owned()),
                        definition_location: None,
                        call_site_location: Some(call.location.clone()),
                        hide_when_unchanged: true,
                        local_shape: None,
                        children: Vec::new(),
                    });
                }
                Resolution::Ambiguous { target, candidates }
                | Resolution::Limited { target, candidates } => {
                    *nodes += 1;
                    let limited = matches!(call.resolution, Resolution::Limited { .. });
                    let kind = if limited { "limited" } else { "ambiguous" };
                    let mut label = candidates
                        .iter()
                        .take(MAX_WITNESSES)
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ");
                    if candidates.len() > MAX_WITNESSES {
                        label.push_str(&format!(", … ({} witnesses)", candidates.len()));
                    }
                    let mut children = Vec::new();
                    for candidate in candidates.iter().take(MAX_WITNESSES) {
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
                    if (limited || candidates.len() > MAX_WITNESSES)
                        && children
                            .last()
                            .is_none_or(|node| node.identity != "[truncated]")
                    {
                        children.push(truncated());
                    }
                    output.push(TreeNode {
                        fingerprint: None,
                        identity: call.resolution.identity(),
                        label: format!("[{kind}: {target} -> {label}]"),
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
    fn shared_graph_expansion_is_bounded_across_roots_and_both_sides() {
        use crate::{graph, model};

        let key = model::SymbolKey {
            language: model::Language::Rust,
            file: "lib.rs".to_owned(),
            qualified_name: vec!["shared".to_owned()],
            conditions: Vec::new(),
        };
        let location = model::SourceRange {
            start_line: 1,
            start_column: 1,
            end_line: 1,
            end_column: 2,
        };
        let leaf = graph::GraphNode {
            symbol: model::LocalSymbol {
                key: key.clone(),
                module: Vec::new(),
                location: location.clone(),
                visibility: model::Visibility::Private,
                shape: "before".to_owned(),
                steps: Vec::new(),
            },
            steps: Vec::new(),
        };
        let empty = graph::Graph::default();
        let mut before = graph::Graph::default();
        before.symbols.insert(key.clone(), leaf.clone());
        for index in 0..100 {
            let mut root = leaf.clone();
            root.symbol.key.qualified_name = vec![format!("root{index:03}")];
            root.symbol.visibility = model::Visibility::Public;
            root.steps = vec![
                model::ResolvedStep::Call(model::ResolvedCall {
                    resolution: model::Resolution::Resolved(key.clone()),
                    location: location.clone(),
                });
                300
            ];
            before
                .incoming
                .entry(key.clone())
                .or_default()
                .insert(root.symbol.key.clone());
            before.symbols.insert(root.symbol.key.clone(), root);
        }
        let mut after = before.clone();
        after.symbols.get_mut(&key).unwrap().symbol.shape = "after".to_owned();
        for (before, after) in [
            (&before, &after),
            (&after, &before),
            (&empty, &after),
            (&after, &empty),
        ] {
            let diff = super::compare(before, after);
            assert!(diff.truncated);
            assert!(!diff.roots.is_empty());
            assert!(diff.roots.len() < 100);
            for trees in [
                diff.roots
                    .iter()
                    .filter_map(|root| root.before.as_ref())
                    .collect::<Vec<_>>(),
                diff.roots
                    .iter()
                    .filter_map(|root| root.after.as_ref())
                    .collect::<Vec<_>>(),
            ] {
                let mut pending = trees;
                let mut count = 0;
                while let Some(node) = pending.pop() {
                    if node.identity != "[truncated]" {
                        count += 1;
                    }
                    pending.extend(&node.children);
                }
                assert!(count <= super::MAX_NODES, "{count}");
            }
            assert!(crate::render::render(&diff).contains("[truncated"));
        }

        // Repeated caller searches also share a work budget, even with tiny trees.
        let mut work = 12;
        let callers = super::exported_callers(&before, &key, &mut work);
        assert_eq!(work, 0);
        assert!(callers.len() <= 12);

        let dense_steps = before
            .symbols
            .keys()
            .map(|key| {
                model::ResolvedStep::Call(model::ResolvedCall {
                    resolution: model::Resolution::Resolved(key.clone()),
                    location: location.clone(),
                })
            })
            .collect::<Vec<_>>();
        let dense = graph::build(model::ResolvedProgram {
            symbols: before
                .symbols
                .values()
                .map(|node| model::ResolvedSymbol {
                    symbol: node.symbol.clone(),
                    steps: dense_steps.clone(),
                })
                .collect(),
        })
        .unwrap();
        let mut changed = dense.clone();
        changed.symbols.get_mut(&key).unwrap().symbol.shape = "after".to_owned();
        let diff = super::compare(&dense, &changed);
        assert!(!diff.roots.is_empty());
        assert!(diff.truncated);
        assert!(crate::render::render(&diff).contains("[truncated"));
    }

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
