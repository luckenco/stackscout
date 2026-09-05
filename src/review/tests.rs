use super::*;
use crate::Repository;

#[test]
fn dependency_changes_omit_unrelated_edits_and_keep_removed_edges() {
    let mut repo = Repository::new().unwrap();
    repo.change(
        "api.ts",
        b"import { work } from './dep'; export function run() { work(); }",
    )
    .unwrap();
    repo.change("dep.ts", b"export function work() { old(); }")
        .unwrap();
    repo.change("other.ts", b"export function unrelated() {}")
        .unwrap();
    let before = repo.snapshot();
    repo.change("dep.ts", b"export function work() { next(); }")
        .unwrap();
    repo.change("other.ts", b"export function unrelated() { noise(); }")
        .unwrap();
    let scope = Scope {
        files: vec!["api.ts".into()],
        ..Scope::default()
    };
    let output = before.review(&repo.snapshot(), &scope).unwrap();
    assert!(!output.truncated);
    assert_eq!(output.witnesses.len(), 2);
    for witness in &output.witnesses {
        assert_eq!(witness.relation, "dependency");
        assert_eq!(witness.symbol.id, "ts:dep.ts::work");
        assert_eq!(witness.path.len(), 1);
        assert_eq!(witness.path[0].caller, "ts:api.ts::run");
    }
    let text = serde_json::to_string(&output).unwrap();
    assert!(text.contains("old"));
    assert!(text.contains("next"));
    assert!(!text.contains("unrelated"));
    assert!(!text.contains("noise"));
    assert_eq!(
        text,
        serde_json::to_string(&before.review(&repo.snapshot(), &scope).unwrap()).unwrap()
    );

    repo.delete("dep.ts").unwrap();
    let deleted = before.review(&repo.snapshot(), &scope).unwrap();
    assert!(
        deleted
            .witnesses
            .iter()
            .any(|w| w.side == "before" && w.change == "removed" && w.path.len() == 1)
    );
    assert!(
        deleted
            .boundaries
            .iter()
            .any(|b| b.side == "after" && b.call.resolution == "unresolved")
    );
    // Removing a call to an unchanged implementation is still visible in local before evidence.
    repo.change("dep.ts", b"export function work() { old(); }")
        .unwrap();
    repo.change(
        "api.ts",
        b"import { work } from './dep'; export function run() {}",
    )
    .unwrap();
    let removed = before.review(&repo.snapshot(), &scope).unwrap();
    assert!(
        removed
            .witnesses
            .iter()
            .any(|w| w.side == "before" && w.relation == "local" && w.calls.len() == 1)
    );
    assert!(
        removed
            .witnesses
            .iter()
            .any(|w| w.side == "after" && w.relation == "local" && w.calls.is_empty())
    );
}

#[test]
fn incoming_paths_show_changed_and_affected_callers_and_handle_cycles() {
    let mut repo = Repository::new().unwrap();
    repo.change(
        "lib.rs",
        b"fn root() { mid(); } fn mid() { leaf(); } fn leaf() { mid(); }",
    )
    .unwrap();
    let before = repo.snapshot();
    repo.change(
        "lib.rs",
        b"fn root() { mid(); } fn mid() { leaf(); } fn leaf() { mid(); new(); }",
    )
    .unwrap();
    let scope = Scope {
        symbols: vec!["leaf".into()],
        ..Scope::default()
    };
    let output = before.review(&repo.snapshot(), &scope).unwrap();
    assert!(!output.truncated);
    let witness = output
        .witnesses
        .iter()
        .find(|w| w.side == "after" && w.symbol.id.ends_with("::root"))
        .unwrap();
    assert_eq!(witness.relation, "affected_caller");
    assert_eq!(witness.path.len(), 2);
    assert_eq!(witness.path[0].caller, "rust:lib.rs::root");
    assert_eq!(witness.path[1].callee.as_deref(), Some("rust:lib.rs::leaf"));
    let scope = Scope {
        symbols: vec!["mid".into()],
        ..Scope::default()
    };
    let output = before.review(&repo.snapshot(), &scope).unwrap();
    assert!(
        output
            .witnesses
            .iter()
            .any(|w| w.relation == "caller" && w.symbol.id.ends_with("::leaf"))
    );
}

#[test]
fn deleted_scope_and_ambiguous_boundaries_remain_explicit() {
    let mut repo = Repository::new().unwrap();
    repo.change("a.ts", b"export function task() {}").unwrap();
    repo.change("b.ts", b"export function task() {}").unwrap();
    repo.change("api.ts", b"import {task as choose} from './a'; import {task as choose} from './b'; export function run() { choose(); }").unwrap();
    let before = repo.snapshot();
    repo.change("a.ts", b"export function task() { changed(); }")
        .unwrap();
    let scope = Scope {
        symbols: vec!["run".into(), "task".into(), "absent".into()],
        ..Scope::default()
    };
    let output = before.review(&repo.snapshot(), &scope).unwrap();
    assert!(
        output
            .witnesses
            .iter()
            .any(|w| w.path.iter().any(|call| call.resolution == "ambiguous"))
    );
    assert!(
        output
            .missing
            .iter()
            .any(|m| m.selector == "task" && m.reason.contains("ambiguous"))
    );
    assert!(output.missing.iter().any(|m| m.selector == "absent"));
    repo.delete("api.ts").unwrap();
    let output = before
        .review(
            &repo.snapshot(),
            &Scope {
                files: vec!["api.ts".into()],
                ..Scope::default()
            },
        )
        .unwrap();
    assert!(
        output
            .witnesses
            .iter()
            .any(|w| w.side == "before" && w.change == "removed")
    );
    assert!(
        output
            .missing
            .iter()
            .any(|m| m.side == "after" && m.selector == "api.ts")
    );
}

#[test]
fn failed_analysis_is_not_reported_as_deletion_and_is_visible_on_both_sides() {
    let mut repo = Repository::new().unwrap();
    repo.change("lib.rs", b"fn run() {}").unwrap();
    let before = repo.snapshot();
    repo.change("lib.rs", b"fn run( {").unwrap();
    let after = repo.snapshot();
    let scope = Scope {
        files: vec!["lib.rs".into()],
        ..Scope::default()
    };
    for (a, b) in [(&before, &after), (&after, &before)] {
        let output = a.review(b, &scope).unwrap();
        assert!(output.witnesses.is_empty());
        assert!(!output.analysis_issues.is_empty());
        assert_eq!(output.missing.len(), 2);
    }
}

#[test]
fn boundary_noise_does_not_prevent_after_witnesses() {
    let mut repo = Repository::new().unwrap();
    let calls = (0..40)
        .map(|i| format!("unknown{i}();"))
        .collect::<String>();
    repo.change("lib.rs", format!("fn run() {{ {calls} }}").as_bytes())
        .unwrap();
    let before = repo.snapshot();
    repo.change(
        "lib.rs",
        format!("fn run() {{ {calls} added(); }}").as_bytes(),
    )
    .unwrap();
    let scope = Scope {
        symbols: vec!["run".into()],
        ..Scope::default()
    };
    let output = before.review(&repo.snapshot(), &scope).unwrap();
    assert!(output.truncated);
    assert_eq!(output.boundaries.len(), 32);
    assert!(output.witnesses.iter().any(|w| w.side == "before"));
    assert!(output.witnesses.iter().any(|w| w.side == "after"));
}

#[test]
fn selector_search_exhaustion_marks_the_review_truncated() {
    let mut repo = Repository::new().unwrap();
    let source = (0..10_001)
        .map(|i| format!("fn f{i}() {{}}"))
        .collect::<String>();
    repo.change("lib.rs", source.as_bytes()).unwrap();
    let snapshot = repo.snapshot();
    let scope = Scope {
        symbols: vec!["absent".into()],
        ..Scope::default()
    };
    let output = snapshot.review(&snapshot, &scope).unwrap();
    assert_eq!(output.missing.len(), 2);
    assert!(
        output
            .missing
            .iter()
            .all(|item| item.reason.contains("selector search limit"))
    );
    assert!(output.truncated);
}

#[test]
fn scope_and_evidence_budgets_are_explicit() {
    assert!(Scope::default().normalize().is_err());
    assert!(
        Scope {
            files: vec!["../lib.rs".into()],
            ..Scope::default()
        }
        .normalize()
        .is_err()
    );
    assert!(
        Scope {
            symbols: vec!["a".repeat(513)],
            ..Scope::default()
        }
        .normalize()
        .is_err()
    );
    let mut scope = Scope {
        files: vec!["lib.rs".into(), "lib.rs".into()],
        symbols: vec![],
    };
    scope.normalize().unwrap();
    assert_eq!(scope.files.len(), 1);
    let mut repo = Repository::new().unwrap();
    repo.change("lib.rs", b"fn run() {}").unwrap();
    let before = repo.snapshot();
    let calls = (0..2000)
        .map(|i| format!("unknown{i}();"))
        .collect::<String>();
    repo.change("lib.rs", format!("fn run() {{ {calls} }}").as_bytes())
        .unwrap();
    let output = before.review(&repo.snapshot(), &scope).unwrap();
    assert!(output.truncated);
    assert!(serde_json::to_vec(&output).unwrap().len() <= 32 * 1024);
    let wide = (0..100)
        .map(|i| format!("fn f{i}() {{}}"))
        .collect::<String>();
    repo.change("lib.rs", wide.as_bytes()).unwrap();
    let output = before.review(&repo.snapshot(), &scope).unwrap();
    assert!(output.truncated);
    assert!(output.witnesses.len() <= 64);
}
