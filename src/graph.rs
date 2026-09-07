use std::collections::{BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};

use thiserror::Error;

use crate::model::{LocalSymbol, Resolution, ResolvedProgram, ResolvedStep, SymbolKey, Visibility};

// Borrow semantic data; locations and display labels do not affect shape equality.
#[derive(Clone, Copy, Debug, Eq)]
pub(crate) struct LocalShape<'a> {
    visibility: Visibility,
    syntax: &'a str,
    steps: &'a [ResolvedStep],
}

impl PartialEq for LocalShape<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.visibility == other.visibility
            && self.syntax == other.syntax
            && same_steps(self.steps, other.steps)
    }
}

impl Hash for LocalShape<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.visibility.hash(state);
        self.syntax.hash(state);
        hash_steps(self.steps, state);
    }
}

#[derive(Clone, Debug)]
pub(crate) struct GraphNode {
    pub(crate) symbol: LocalSymbol,
    pub(crate) steps: Vec<ResolvedStep>,
}

impl GraphNode {
    pub(crate) fn local_shape(&self) -> LocalShape<'_> {
        LocalShape {
            visibility: self.symbol.visibility,
            syntax: &self.symbol.shape,
            steps: &self.steps,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct Graph {
    pub(crate) symbols: BTreeMap<SymbolKey, GraphNode>,
    pub(crate) incoming: BTreeMap<SymbolKey, BTreeSet<SymbolKey>>,
}

#[derive(Debug, Error)]
pub(crate) enum GraphError {
    #[error("duplicate symbol identity: {0}")]
    Duplicate(SymbolKey),
}

pub(crate) fn build(program: ResolvedProgram) -> Result<Graph, GraphError> {
    let mut symbols = BTreeMap::new();
    let mut incoming = BTreeMap::<SymbolKey, BTreeSet<SymbolKey>>::new();

    for resolved in program.symbols {
        let key = resolved.symbol.key.clone();
        let node = GraphNode {
            symbol: resolved.symbol,
            steps: resolved.steps,
        };
        if symbols.insert(key.clone(), node).is_some() {
            return Err(GraphError::Duplicate(key));
        }
    }

    for (caller, node) in &symbols {
        add_incoming(caller, &node.steps, &mut incoming);
    }

    Ok(Graph { symbols, incoming })
}

fn add_incoming(
    caller: &SymbolKey,
    steps: &[ResolvedStep],
    incoming: &mut BTreeMap<SymbolKey, BTreeSet<SymbolKey>>,
) {
    for step in steps {
        match step {
            ResolvedStep::Call(call) => match &call.resolution {
                Resolution::Resolved(callee) => {
                    incoming
                        .entry(callee.clone())
                        .or_default()
                        .insert(caller.clone());
                }
                Resolution::Ambiguous { candidates, .. } => {
                    for callee in candidates {
                        incoming
                            .entry(callee.clone())
                            .or_default()
                            .insert(caller.clone());
                    }
                }
                Resolution::Unresolved(_) | Resolution::Uncertain(_) => {}
            },
            ResolvedStep::Branch { children, .. } => add_incoming(caller, children, incoming),
        }
    }
}

fn same_steps(left: &[ResolvedStep], right: &[ResolvedStep]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| match (left, right) {
                (ResolvedStep::Call(left), ResolvedStep::Call(right)) => {
                    left.resolution == right.resolution
                }
                (
                    ResolvedStep::Branch {
                        key: left,
                        children: a,
                        ..
                    },
                    ResolvedStep::Branch {
                        key: right,
                        children: b,
                        ..
                    },
                ) => left == right && same_steps(a, b),
                _ => false,
            })
}

fn hash_steps<H: Hasher>(steps: &[ResolvedStep], state: &mut H) {
    steps.len().hash(state);
    for step in steps {
        std::mem::discriminant(step).hash(state);
        match step {
            ResolvedStep::Call(call) => call.resolution.hash(state),
            ResolvedStep::Branch { key, children, .. } => {
                key.hash(state);
                hash_steps(children, state);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrowed_shapes_ignore_locations_and_labels_but_preserve_semantics() {
        let mut repo = crate::Repository::new().unwrap();
        repo.change("lib.rs", b"pub fn run(x: bool) { if x { audit(); } }")
            .unwrap();
        let snapshot = repo.snapshot();
        let node = snapshot.graph.symbols.values().next().unwrap();
        let mut moved = node.clone();
        moved.symbol.location.start_line += 10;
        let ResolvedStep::Branch {
            label,
            location,
            children,
            ..
        } = &mut moved.steps[0]
        else {
            panic!("expected branch")
        };
        label.push_str(" display only");
        location.start_line += 10;
        let ResolvedStep::Call(call) = &mut children[0] else {
            panic!("expected call")
        };
        call.location.start_line += 10;
        let shapes = std::collections::HashSet::from([node.local_shape()]);
        assert_eq!(node.local_shape(), moved.local_shape());
        assert!(shapes.contains(&moved.local_shape()));

        let ResolvedStep::Branch { children, .. } = &mut moved.steps[0] else {
            unreachable!()
        };
        children.clear();
        assert!(!shapes.contains(&moved.local_shape()));
        let mut changed = node.clone();
        changed.symbol.visibility = Visibility::Private;
        assert!(!shapes.contains(&changed.local_shape()));
        changed.symbol.visibility = Visibility::Public;
        changed.symbol.shape.push_str("different");
        assert!(!shapes.contains(&changed.local_shape()));
    }
}
