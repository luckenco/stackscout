//! Bounded, deterministic queries over one immutable syntactic graph; no I/O or service state.
use std::collections::{BTreeSet, VecDeque};

use serde::{Deserialize, Serialize};

use crate::{graph, model};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Request {
    pub symbol: String,
    pub depth: Option<usize>,
    pub limit: Option<usize>,
}

#[derive(Clone, Copy)]
pub(crate) enum Direction {
    Callers,
    Callees,
    Impact,
}

#[derive(Serialize)]
pub(crate) struct Output {
    pub symbols: Vec<Symbol>,
    pub calls: Vec<Call>,
    pub truncated: bool,
    pub warning: &'static str,
}

#[derive(Serialize)]
pub(crate) struct Symbol {
    id: String,
    file: String,
    location: model::SourceRange,
    depth: usize,
}

#[derive(Serialize)]
pub(crate) struct Call {
    caller: String,
    callee: Option<String>,
    target: String,
    resolution: &'static str,
    location: model::SourceRange,
}

fn select<'a>(graph: &'a graph::Graph, selector: &str) -> Result<&'a model::SymbolKey, String> {
    let mut name = selector;
    let mut qualified = false;
    let keys: Box<dyn Iterator<Item = &model::SymbolKey>> = if let Some((language, rest)) =
        selector.split_once(':')
        && !rest.starts_with(':')
    {
        let language = match language {
            "rust" => model::Language::Rust,
            "ts" => model::Language::TypeScript,
            _ => return Err("symbol language must be rust or ts".to_owned()),
        };
        let (file, scoped_name) = rest
            .split_once("::")
            .ok_or("expected language:file::qualified::name")?;
        name = scoped_name;
        qualified = true;
        let start = model::SymbolKey {
            language,
            file: file.to_owned(),
            qualified_name: Vec::new(),
        };
        // Scope components (e.g. Rust impl types) may themselves contain `::`.
        Box::new(
            graph
                .symbols
                .range(start..)
                .map(|(key, _)| key)
                .take_while(move |key| key.language == language && key.file == file),
        )
    } else {
        Box::new(graph.symbols.keys())
    };
    let mut found = None;
    for (index, key) in keys.enumerate() {
        if index >= 10_000 {
            return Err(
                "selector search limit reached; use language:file::qualified::name".to_owned(),
            );
        }
        if (!qualified && key.qualified_name.last().is_some_and(|last| last == name))
            || key.qualified_name.join("::") == name
        {
            if found.is_some() {
                return Err("ambiguous symbol; use language:file::qualified::name".to_owned());
            }
            found = Some(key);
        }
    }
    found.ok_or("unknown symbol".to_owned())
}

// Charge encoded bytes before retaining items, including JSON escaping. Reserve envelope space.
fn fits(value: &impl Serialize, bytes: &mut usize) -> bool {
    let size = serde_json::to_vec(value)
        .expect("query data is serializable")
        .len()
        + 1;
    if size > *bytes {
        return false;
    }
    *bytes -= size;
    true
}

pub(crate) fn run(
    graph: &graph::Graph,
    request: Request,
    direction: Direction,
) -> Result<Output, String> {
    let default_depth = if matches!(direction, Direction::Impact) {
        16
    } else {
        1
    };
    let depth_limit = request.depth.unwrap_or(default_depth);
    let node_limit = request.limit.unwrap_or(64);
    if depth_limit > 32 || !(1..=256).contains(&node_limit) {
        return Err("query depth must be 0–32 and limit must be 1–256".to_owned());
    }
    let root = select(graph, &request.symbol)?;
    let incoming = !matches!(direction, Direction::Callees);
    let mut output = Output {
        symbols: Vec::new(),
        calls: Vec::new(),
        truncated: false,
        warning: "Syntactic graph only: ambiguous edges are possible calls; unresolved, dynamic, or omitted calls may hide additional paths. Inspect analysis issues and watcher health.",
    };
    let mut bytes = 48 * 1024 - 512;
    let mut work = 20_000usize;
    let mut queue = VecDeque::from([(root, 0)]);
    let mut seen = BTreeSet::new();
    let mut edges = BTreeSet::new();
    seen.insert(root);
    let symbol = &graph.symbols[root].symbol;
    let symbol = Symbol {
        id: root.to_string(),
        file: root.file.clone(),
        location: symbol.location.clone(),
        depth: 0,
    };
    if !fits(&symbol, &mut bytes) {
        output.truncated = true;
        return Ok(output);
    }
    output.symbols.push(symbol);
    'walk: while let Some((current, depth)) = queue.pop_front() {
        // Incoming adjacency already includes ambiguous candidates, but never guesses unresolved calls.
        let callers: Box<dyn Iterator<Item = &model::SymbolKey>> = if incoming {
            Box::new(graph.incoming.get(current).into_iter().flatten())
        } else {
            Box::new(std::iter::once(current))
        };
        for caller in callers {
            let mut stack = vec![graph.symbols[caller].steps.iter()];
            while let Some(iter) = stack.last_mut() {
                let Some(step) = iter.next() else {
                    stack.pop();
                    continue;
                };
                if work == 0 {
                    output.truncated = true;
                    break 'walk;
                }
                work -= 1;
                let call = match step {
                    model::ResolvedStep::Branch { children, .. } => {
                        stack.push(children.iter());
                        continue;
                    }
                    model::ResolvedStep::Call(call) => call,
                };
                let (resolution, candidates, target): (&str, &[model::SymbolKey], String) =
                    match &call.resolution {
                        model::Resolution::Resolved(key) => {
                            ("resolved", std::slice::from_ref(key), key.to_string())
                        }
                        model::Resolution::Ambiguous { target, candidates } => {
                            ("ambiguous", candidates, target.to_string())
                        }
                        model::Resolution::Unresolved(target) => {
                            ("unresolved", &[], target.to_string())
                        }
                        model::Resolution::Uncertain(target) => {
                            ("uncertain", &[], target.to_string())
                        }
                    };
                if incoming {
                    if candidates.len() > work {
                        output.truncated = true;
                        break 'walk;
                    }
                    work -= candidates.len();
                    if !candidates.contains(current) {
                        continue;
                    }
                }
                if depth == depth_limit {
                    output.truncated = true;
                    continue 'walk;
                }
                let targets: Box<dyn Iterator<Item = Option<&model::SymbolKey>>> = if incoming {
                    Box::new(std::iter::once(Some(current)))
                } else if candidates.is_empty() {
                    Box::new(std::iter::once(None))
                } else {
                    Box::new(candidates.iter().map(Some))
                };
                for callee in targets {
                    if work == 0 || output.calls.len() >= 1024 {
                        output.truncated = true;
                        break 'walk;
                    }
                    work -= 1;
                    let next = if incoming { Some(caller) } else { callee };
                    if let Some(next) = next
                        && !seen.contains(next)
                    {
                        let node = &graph.symbols[next].symbol;
                        let symbol = Symbol {
                            id: next.to_string(),
                            file: next.file.clone(),
                            location: node.location.clone(),
                            depth: depth + 1,
                        };
                        if seen.len() >= node_limit || !fits(&symbol, &mut bytes) {
                            output.truncated = true;
                            break 'walk;
                        }
                        seen.insert(next);
                        queue.push_back((next, depth + 1));
                        output.symbols.push(symbol);
                    }
                    if !edges.insert((
                        caller,
                        callee,
                        call.location.start_line,
                        call.location.start_column,
                    )) {
                        continue;
                    }
                    let edge = Call {
                        caller: caller.to_string(),
                        callee: callee.map(ToString::to_string),
                        target: target.clone(),
                        resolution,
                        location: call.location.clone(),
                    };
                    if !fits(&edge, &mut bytes) {
                        output.truncated = true;
                        break 'walk;
                    }
                    output.calls.push(edge);
                }
            }
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Repository;

    #[test]
    fn directions_cycles_and_unresolved_evidence() {
        let mut repo = Repository::new().unwrap();
        repo.change(
            "lib.rs",
            b"pub fn root() { mid(); } fn mid() { leaf(); missing(); } fn leaf() { mid(); }",
        )
        .unwrap();
        let snapshot = repo.snapshot();
        let request = |symbol: &str, depth| Request {
            symbol: symbol.to_owned(),
            depth: Some(depth),
            limit: None,
        };
        let incoming = run(&snapshot.graph, request("leaf", 8), Direction::Impact).unwrap();
        assert_eq!(incoming.symbols.len(), 3);
        assert_eq!(incoming.calls.len(), 3);
        assert!(!incoming.truncated);
        let direct = run(&snapshot.graph, request("leaf", 1), Direction::Callers).unwrap();
        assert_eq!(direct.symbols.len(), 2);
        assert!(direct.truncated);
        let outgoing = run(
            &snapshot.graph,
            request("rust:lib.rs::mid", 8),
            Direction::Callees,
        )
        .unwrap();
        assert_eq!(outgoing.symbols.len(), 2);
        assert!(
            outgoing
                .calls
                .iter()
                .any(|call| call.resolution == "unresolved" && call.callee.is_none())
        );
        assert!(!outgoing.truncated);
    }

    #[test]
    fn ambiguous_edges_remain_possible_in_both_directions() {
        let mut repo = Repository::new().unwrap();
        repo.change("a.ts", b"export function task() {}").unwrap();
        repo.change("b.ts", b"export function task() {}").unwrap();
        repo.change("main.ts", b"import {task as choose} from './a'; import {task as choose} from './b'; export function run() { choose(); }").unwrap();
        let snapshot = repo.snapshot();
        let outgoing = run(
            &snapshot.graph,
            Request {
                symbol: "run".to_owned(),
                depth: None,
                limit: None,
            },
            Direction::Callees,
        )
        .unwrap();
        assert_eq!(outgoing.calls.len(), 2);
        assert!(
            outgoing
                .calls
                .iter()
                .all(|call| call.resolution == "ambiguous")
        );
        let incoming = run(
            &snapshot.graph,
            Request {
                symbol: "ts:a.ts::task".to_owned(),
                depth: None,
                limit: None,
            },
            Direction::Callers,
        )
        .unwrap();
        assert_eq!(incoming.calls.len(), 1);
        assert_eq!(incoming.calls[0].resolution, "ambiguous");
    }

    #[test]
    fn qualified_impl_names_and_encoded_byte_limits() {
        let mut repo = Repository::new().unwrap();
        repo.change(
            "lib.rs",
            b"mod ns { pub struct Thing; } impl ns::Thing { fn run() {} }",
        )
        .unwrap();
        let snapshot = repo.snapshot();
        assert!(select(&snapshot.graph, "rust:lib.rs::ns::Thing::run").is_ok());
        let calls = (0..2000)
            .map(|i| format!("missing{i}();"))
            .collect::<String>();
        repo.change("wide.rs", format!("fn wide() {{ {calls} }}").as_bytes())
            .unwrap();
        let output = run(
            &repo.snapshot().graph,
            Request {
                symbol: "wide".to_owned(),
                depth: None,
                limit: None,
            },
            Direction::Callees,
        )
        .unwrap();
        assert!(output.truncated);
        assert!(!output.calls.is_empty());
        assert!(serde_json::to_vec(&output).unwrap().len() <= 48 * 1024);
    }

    #[test]
    fn selectors_and_budgets_are_explicit() {
        let mut repo = Repository::new().unwrap();
        repo.change("a.rs", b"pub fn same() { missing(); }")
            .unwrap();
        repo.change("b.rs", b"pub fn same() {}").unwrap();
        let snapshot = repo.snapshot();
        let mut request = Request {
            symbol: "same".to_owned(),
            depth: None,
            limit: None,
        };
        assert!(
            run(&snapshot.graph, request, Direction::Callees)
                .err()
                .unwrap()
                .contains("ambiguous")
        );
        request = Request {
            symbol: "rust:a.rs::same".to_owned(),
            depth: Some(0),
            limit: Some(1),
        };
        let output = run(&snapshot.graph, request, Direction::Callees).unwrap();
        assert!(output.truncated);
        assert_eq!(output.symbols.len(), 1);
        assert!(output.calls.is_empty());
    }
}
