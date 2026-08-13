use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::model::{LocalSymbol, Resolution, ResolvedProgram, ResolvedStep, SymbolKey, Visibility};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalShape {
    pub(crate) visibility: Visibility,
    pub(crate) syntax: String,
    pub(crate) steps: Vec<String>,
}

impl LocalShape {
    pub(crate) fn fingerprint(&self) -> String {
        format!("{:?}\n{}\n{:?}", self.visibility, self.syntax, self.steps)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct GraphNode {
    pub(crate) symbol: LocalSymbol,
    pub(crate) steps: Vec<ResolvedStep>,
}

impl GraphNode {
    pub(crate) fn local_shape(&self) -> LocalShape {
        LocalShape {
            visibility: self.symbol.visibility,
            syntax: self.symbol.shape.clone(),
            steps: self.steps.iter().map(step_identity).collect(),
        }
    }
}

#[derive(Clone, Debug)]
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

fn step_identity(step: &ResolvedStep) -> String {
    match step {
        ResolvedStep::Call(call) => call.resolution.identity(),
        ResolvedStep::Branch { key, children, .. } => {
            let children = children
                .iter()
                .map(step_identity)
                .collect::<Vec<_>>()
                .join("\0");
            format!("#{key}[{children}]")
        }
    }
}
