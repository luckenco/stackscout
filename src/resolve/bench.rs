//! Manual benchmark: cargo test --release resolve::bench::scaling -- --ignored --nocapture

use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::Instant;

use crate::model::{CallTarget, Resolution, ResolvedStep};
use crate::{Repository, analyze, graph};

#[test]
fn colliding_typescript_aliases_keep_bounded_ambiguity_witnesses() {
    let cases = [
        ("{ work as alias }", "export function work() {}", "alias()"),
        ("alias", "export default function work() {}", "alias()"),
        ("* as alias", "export function work() {}", "alias.work()"),
        (
            "{ Worker as alias }",
            "export class Worker { static work() {} constructor() {} }",
            "alias.work()",
        ),
        (
            "alias",
            "export default class Worker { static work() {} constructor() {} }",
            "alias.work()",
        ),
        (
            "{ Worker as alias }",
            "export class Worker { constructor() {} }",
            "new alias()",
        ),
        (
            "alias",
            "export default class Worker { constructor() {} }",
            "new alias()",
        ),
        (
            "* as alias",
            "export class Worker { constructor() {} }",
            "new alias.Worker()",
        ),
    ];
    let mut analyzers = analyze::Analyzers::new().unwrap();
    for (binding, dependency, call) in cases {
        for n in [3, super::TYPESCRIPT_WORK_LIMIT * 2] {
            let mut artifacts = Vec::new();
            let mut source = String::new();
            for i in 0..n {
                source.push_str(&format!("import {binding} from './dep_{i}.ts';\n"));
                artifacts.push(analyzers.file(&format!("dep_{i}.ts"), dependency).unwrap());
            }
            // Distinct callers prevent graph incoming sets from hiding repeated
            // candidate allocations by deduplicating the same caller.
            for i in 0..n {
                source.push_str(&format!("export function caller_{i}() {{ {call}; }}\n"));
            }
            artifacts.push(analyzers.file("caller.ts", &source).unwrap());
            let program = super::program(&artifacts);
            artifacts.reverse();
            assert_eq!(program, super::program(&artifacts));
            let mut retained = 0;
            for symbol in program
                .symbols
                .iter()
                .filter(|s| s.symbol.key.file == "caller.ts")
            {
                let ResolvedStep::Call(call) = &symbol.steps[0] else {
                    panic!("expected call")
                };
                let candidates = match &call.resolution {
                    Resolution::Ambiguous { candidates, .. } if n == 3 => candidates,
                    Resolution::Limited { candidates, .. } if n > 3 => candidates,
                    other => panic!("wrong resolution: {other:?}"),
                };
                assert!(candidates.len() >= 2);
                assert!(candidates.windows(2).all(|pair| pair[0] < pair[1]));
                if n == 3 {
                    assert_eq!(candidates.len(), n);
                } else {
                    assert!(candidates.len() < n);
                }
                retained += candidates.len();
            }
            assert!(retained <= n * super::TYPESCRIPT_WORK_LIMIT);
            let graph = graph::build(program).unwrap();
            assert_eq!(
                graph
                    .incoming
                    .values()
                    .map(|callers| callers.len())
                    .sum::<usize>(),
                retained
            );
        }
    }
}

#[test]
fn unrelated_imports_and_exports_do_not_exhaust_matching_searches() {
    let mut analyzers = analyze::Analyzers::new().unwrap();
    let mut dependency = String::new();
    let mut source = String::new();
    for i in 0..super::TYPESCRIPT_WORK_LIMIT * 2 {
        dependency.push_str(&format!("export function other_{i}() {{}}\n"));
        source.push_str(&format!("import {{ other_{i} }} from './dep.ts';\n"));
    }
    dependency.push_str(
        "export function work() {} export class Worker { static work() {} constructor() {} }",
    );
    source.push_str("import { work as alias, Worker } from './dep.ts'; import * as ns from './dep.ts'; function caller() { alias(); Worker.work(); ns.work(); new Worker(); new ns.Worker(); missing(); }");
    let dep = analyzers.file("dep.ts", &dependency).unwrap();
    let caller = analyzers.file("caller.ts", &source).unwrap();
    let program = super::program([&caller, &dep]);
    let steps = &program.symbols[0].steps;
    assert_eq!(steps.len(), 6);
    for step in &steps[..5] {
        let ResolvedStep::Call(call) = step else {
            panic!("expected call")
        };
        assert!(
            matches!(call.resolution, Resolution::Resolved(_)),
            "{:?}",
            call.resolution
        );
    }
    let ResolvedStep::Call(call) = &steps[5] else {
        panic!("expected call")
    };
    assert_eq!(
        call.resolution,
        Resolution::Unresolved(CallTarget::Bare("missing".into()))
    );
    let graph = graph::build(program).unwrap();
    assert!(!graph.limited);
    assert_eq!(graph.incoming.len(), 3);
}

#[test]
fn exhausted_search_retains_zero_or_one_witness_and_flags_incoming_queries() {
    let mut analyzers = analyze::Analyzers::new().unwrap();
    let dep = analyzers
        .file("dep.ts", "export function work() {}")
        .unwrap();
    for known in [false, true] {
        let mut source = String::new();
        if known {
            source.push_str("import { work as alias } from './dep.ts';\n");
        }
        for _ in 0..super::TYPESCRIPT_WORK_LIMIT {
            source.push_str("import { work as alias } from './missing.ts';\n");
        }
        source.push_str("import { work as alias } from './dep.ts'; function caller() { alias(); }");
        let caller = analyzers.file("caller.ts", &source).unwrap();
        let program = super::program([&caller, &dep]);
        let ResolvedStep::Call(call) = &program.symbols[0].steps[0] else {
            panic!("expected call")
        };
        let Resolution::Limited { target, candidates } = &call.resolution else {
            panic!("expected limited")
        };
        assert_eq!(target, &CallTarget::Bare("alias".into()));
        assert_eq!(candidates.len(), usize::from(known));
        assert!(call.resolution.identity().starts_with("limited:"));
        let graph = graph::build(program).unwrap();
        assert!(graph.limited);
        assert_eq!(graph.incoming.len(), usize::from(known));
        for direction in [
            crate::query::Direction::Callees,
            crate::query::Direction::Callers,
            crate::query::Direction::Impact,
        ] {
            let outgoing = matches!(direction, crate::query::Direction::Callees);
            let output = crate::query::run(
                &graph,
                crate::query::Request {
                    symbol: if outgoing { "caller" } else { "work" }.into(),
                    depth: Some(8),
                    limit: None,
                },
                direction,
            )
            .unwrap();
            assert!(output.truncated);
            assert_eq!(output.calls.len(), usize::from(outgoing || known));
            for call in &output.calls {
                assert_eq!(call.resolution, "limited");
                assert_eq!(call.candidates.as_ref().unwrap().len(), usize::from(known));
                assert_eq!(call.callee.is_some(), known);
            }
            assert!(serde_json::to_vec(&output).unwrap().len() <= 48 * 1024);
        }
    }
}

#[test]
fn budget_counts_duplicate_candidate_visits_and_has_an_exact_boundary() {
    let mut analyzers = analyze::Analyzers::new().unwrap();
    let dep = analyzers
        .file("dep.ts", "export function work() {}")
        .unwrap();
    let target = CallTarget::Bare("work".into());
    for n in [
        super::TYPESCRIPT_WORK_LIMIT,
        super::TYPESCRIPT_WORK_LIMIT + 1,
    ] {
        let symbols = vec![dep.symbols[0].clone(); n];
        let index = super::Index::new(&symbols, Default::default(), Default::default());
        let mut candidates = super::Candidates::new();
        index.file(dep.language, &dep.path, &["work".into()], &mut candidates);
        assert_eq!(candidates.remaining, 0);
        assert_eq!(candidates.keys.len(), 1);
        assert_eq!(candidates.limited, n > super::TYPESCRIPT_WORK_LIMIT);
        if n == super::TYPESCRIPT_WORK_LIMIT {
            assert_eq!(
                candidates.finish(&target),
                Resolution::Resolved(dep.symbols[0].key.clone())
            );
        } else {
            assert_eq!(
                candidates.finish(&target),
                Resolution::Limited {
                    target: target.clone(),
                    candidates: vec![dep.symbols[0].key.clone()],
                }
            );
        }
    }
}

#[test]
#[ignore = "manual release-mode performance measurement"]
fn scaling() {
    for extension in ["ts", "rs"] {
        for (files, padding) in [(10, 0), (100, 0), (1000, 0), (100, 27)] {
            let mut sources = BTreeMap::new();
            let mut analyzers = analyze::Analyzers::new().unwrap();
            let mut artifacts = Vec::new();
            for file in 0..files {
                let path = format!("file_{file:04}.{extension}");
                let mut source = String::new();
                for function in 0..10 {
                    let declaration = match (extension, function) {
                        ("ts", 0) => "export function",
                        ("ts", _) => "function",
                        (_, 0) => "pub fn",
                        _ => "fn",
                    };
                    source.push_str(&format!("{declaration} f_{file}_{function}() {{\n"));
                    for local in 0..padding {
                        let binding = if extension == "ts" { "const" } else { "let" };
                        source.push_str(&format!("  {binding} v_{local} = {local};\n"));
                    }
                    let target = if function == 9 {
                        "beforeCall".to_owned()
                    } else {
                        format!("f_{file}_{}", function + 1)
                    };
                    source.push_str(&format!("  {target}();\n}}\n"));
                }
                artifacts.push(analyzers.file(&path, &source).unwrap());
                sources.insert(path, Some(source.into_bytes()));
            }

            // Parsing is outside the timer; index construction and symbol cloning are inside.
            // One warmup, five samples. Keep raw samples to expose measurement noise.
            let mut samples = Vec::new();
            for sample in 0..6 {
                let start = Instant::now();
                let program = black_box(super::program(black_box(&artifacts)));
                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                assert_eq!(program.symbols.len(), files * 10);
                if sample > 0 {
                    samples.push(elapsed);
                }
            }
            println!("{extension} files={files} padding={padding} resolve_ms={samples:?}");

            if padding == 0 {
                continue;
            }
            let mut repo = Repository::new().unwrap();
            repo.update(&sources).unwrap();
            repo.checkpoint("before");
            let (path, original) = sources.first_key_value().unwrap();
            let original = original.as_ref().unwrap();
            let changed = std::str::from_utf8(original)
                .unwrap()
                .replace("beforeCall", "afterCall");
            let mut samples = Vec::new();
            for sample in 0..6 {
                repo.change(path, original).unwrap();
                let start = Instant::now();
                assert!(repo.change(path, changed.as_bytes()).unwrap());
                let output = black_box(repo.diff("before").unwrap());
                let elapsed = start.elapsed().as_secs_f64() * 1000.0;
                assert!(output.contains("afterCall"));
                if sample > 0 {
                    samples.push(elapsed);
                }
            }
            println!("{extension} files={files} padding={padding} update_diff_ms={samples:?}");
        }
    }
}
