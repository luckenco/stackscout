use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

use crate::model::{Export, LocalSymbol, Resolution, ResolvedCall, ResolvedProgram, SymbolKey};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LocalShape {
    pub(crate) export: Export,
    pub(crate) syntax: String,
    pub(crate) calls: Vec<String>,
}

impl LocalShape {
    pub(crate) fn fingerprint(&self) -> String {
        format!("{:?}\n{}\n{:?}", self.export, self.syntax, self.calls)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct GraphNode {
    pub(crate) symbol: LocalSymbol,
    pub(crate) calls: Vec<ResolvedCall>,
}

impl GraphNode {
    pub(crate) fn local_shape(&self) -> LocalShape {
        LocalShape {
            export: self.symbol.export,
            syntax: self.symbol.shape.clone(),
            calls: self
                .calls
                .iter()
                .map(|call| call.resolution.identity())
                .collect(),
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
            calls: resolved.calls,
        };
        if symbols.insert(key.clone(), node).is_some() {
            return Err(GraphError::Duplicate(key));
        }
    }

    for (caller, node) in &symbols {
        for call in &node.calls {
            match &call.resolution {
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
                Resolution::Unresolved(_) => {}
            }
        }
    }

    Ok(Graph { symbols, incoming })
}
