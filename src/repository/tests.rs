use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use super::Repository;
use crate::{discover, model};

fn sources(root: &Path) -> BTreeMap<String, Vec<u8>> {
    discover::sources(root)
        .unwrap()
        .into_iter()
        .map(|path| {
            (
                discover::relative(root, &path).unwrap(),
                fs::read(path).unwrap(),
            )
        })
        .collect()
}

#[test]
fn fixture_updates_match_fresh_analysis_in_both_directions() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("test/fixtures");
    for entry in fs::read_dir(root).unwrap() {
        let fixture = entry.unwrap().path();
        let before = fixture.join("before");
        let after = fixture.join("after");
        for (before, after) in [(&before, &after), (&after, &before)] {
            let original = sources(before);
            let updated = sources(after);
            let mut repo = Repository::new().unwrap();
            // Ingest in the opposite order from fresh directory discovery.
            for (path, bytes) in original.iter().rev() {
                repo.change(path, bytes).unwrap();
            }
            repo.checkpoint("baseline");
            repo.analyzers.parsed.clear();
            for (path, bytes) in &updated {
                assert_eq!(
                    repo.change(path, bytes).unwrap(),
                    original.get(path) != Some(bytes)
                );
            }
            for path in original.keys().filter(|path| !updated.contains_key(*path)) {
                assert!(repo.delete(path).unwrap());
            }
            let parsed = updated
                .iter()
                .filter(|(path, bytes)| original.get(*path) != Some(*bytes))
                .map(|(path, _)| path.clone())
                .collect::<Vec<_>>();
            assert_eq!(repo.analyzers.parsed, parsed, "{}", fixture.display());
            assert_eq!(
                repo.diff("baseline").unwrap(),
                crate::diff_dirs(before, after).unwrap(),
                "{} -> {}",
                before.display(),
                after.display()
            );
            assert_eq!(
                repo.diff_with_diagnostics("baseline").unwrap(),
                crate::diff_dirs_with_diagnostics(before, after).unwrap(),
                "{} -> {} (diagnostics)",
                before.display(),
                after.display()
            );
            assert_eq!(repo.analyzers.parsed, parsed, "diff must not parse");
        }
    }
}

#[test]
fn batch_updates_match_fixtures_and_reuse_unchanged_analysis() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("test/fixtures");
    for entry in fs::read_dir(root).unwrap() {
        let fixture = entry.unwrap().path();
        let before = fixture.join("before");
        let after = fixture.join("after");
        for (before, after) in [(&before, &after), (&after, &before)] {
            let original = sources(before);
            let updated = sources(after);
            let mut repo = Repository::new().unwrap();
            repo.update(
                &original
                    .iter()
                    .map(|(p, b)| (p.clone(), Some(b.clone())))
                    .collect(),
            )
            .unwrap();
            repo.checkpoint("before");
            repo.analyzers.parsed.clear();
            let mut batch = original
                .keys()
                .map(|p| (p.clone(), None))
                .collect::<BTreeMap<_, _>>();
            batch.extend(updated.iter().map(|(p, b)| (p.clone(), Some(b.clone()))));
            repo.update(&batch).unwrap();
            let parsed = updated
                .iter()
                .filter(|(p, b)| original.get(*p) != Some(*b))
                .map(|(p, _)| p.clone())
                .collect::<Vec<_>>();
            assert_eq!(repo.analyzers.parsed, parsed);
            assert_eq!(
                repo.diff("before").unwrap(),
                crate::diff_dirs(before, after).unwrap()
            );
            assert_eq!(
                repo.diff_with_diagnostics("before").unwrap(),
                crate::diff_dirs_with_diagnostics(before, after).unwrap()
            );
            assert!(!repo.update(&batch).unwrap());
            assert_eq!(repo.analyzers.parsed, parsed);
        }
    }
}

#[test]
fn failed_batches_preserve_published_snapshots_and_all_files() {
    let mut repo = Repository::new().unwrap();
    repo.change("lib.rs", b"pub fn run() {}").unwrap();
    repo.checkpoint("before");
    let published = repo.snapshot();
    for invalid in [vec![0xff], b"fn duplicate() {} fn duplicate() {}".to_vec()] {
        let batch = BTreeMap::from([
            ("a.rs".to_owned(), Some(b"pub fn added() {}".to_vec())),
            ("lib.rs".to_owned(), None),
            ("z.rs".to_owned(), Some(invalid)),
        ]);
        assert!(repo.update(&batch).is_err());
        assert_eq!(repo.diff("before").unwrap(), "");
        assert_eq!(repo.snapshot().paths().collect::<Vec<_>>(), ["lib.rs"]);
    }
    repo.change("lib.rs", b"pub fn run() { unknown(); }")
        .unwrap();
    assert!(
        published
            .compare(&repo.snapshot(), false)
            .unwrap()
            .contains("unknown")
    );
    assert_eq!(published.compare(&published, false).unwrap(), "");
}

#[test]
fn updates_use_only_supplied_bytes_and_cache_analysis_failures() {
    let mut repo = Repository::new().unwrap();
    // These paths need not exist. Even initial ingestion only uses supplied bytes.
    repo.change("virtual/api.ts", b"export function run() {}")
        .unwrap();
    repo.change("virtual/worker.ts", b"export function work() {}")
        .unwrap();
    repo.checkpoint("before");
    repo.analyzers.parsed.clear();
    assert!(
        !repo
            .change("virtual/api.ts", b"export function run() {}")
            .unwrap()
    );
    assert!(
        repo.change("virtual/worker.ts", b"export function work() {")
            .unwrap()
    );
    assert!(
        !repo
            .change("virtual/worker.ts", b"export function work() {")
            .unwrap()
    );
    assert!(repo.diff("before").unwrap().contains("introduced"));
    repo.checkpoint("broken");
    assert_eq!(repo.diff("broken").unwrap(), "");
    assert!(
        repo.diff_with_diagnostics("broken")
            .unwrap()
            .contains("unchanged")
    );
    assert!(repo.delete("virtual/worker.ts").unwrap());
    assert!(!repo.delete("virtual/worker.ts").unwrap());
    assert!(repo.diff("broken").unwrap().contains("resolved"));
    assert_eq!(repo.analyzers.parsed, ["virtual/worker.ts"]);
    assert!(
        repo.change("virtual/worker.ts", b"export function work() {}")
            .unwrap()
    );
    assert_eq!(repo.diff("before").unwrap(), "");
    assert_eq!(
        repo.analyzers.parsed,
        ["virtual/worker.ts", "virtual/worker.ts"]
    );
}

#[test]
fn parser_version_and_mode_are_part_of_cache_identity() {
    let mut repo = Repository::new().unwrap();
    let source = b"const view = () => <div />;";
    repo.change("view.tsx", source).unwrap();
    repo.change("view.ts", source).unwrap();
    assert!(repo.current.files["view.tsx"].analysis.is_ok());
    assert!(repo.current.files["view.ts"].analysis.is_err());
    repo.analyzers.parsed.clear();
    assert!(!repo.change("view.tsx", source).unwrap());
    repo.current.files.get_mut("view.tsx").unwrap().key.version = 0;
    assert!(repo.change("view.tsx", source).unwrap());
    repo.current.files.get_mut("view.tsx").unwrap().key.syntax = model::Syntax::TypeScript;
    assert!(repo.change("view.tsx", source).unwrap());
    assert_eq!(repo.analyzers.parsed, ["view.tsx", "view.tsx"]);
}

#[test]
fn replacement_and_deletion_refresh_reverse_edges() {
    let mut repo = Repository::new().unwrap();
    let worker = b"export function work() {} export function other() {}";
    repo.change("worker.ts", worker).unwrap();
    repo.change(
        "api.ts",
        b"import {work, other} from './worker'; export function run() {work(); other();}",
    )
    .unwrap();
    let keys = repo
        .current
        .graph
        .symbols
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    let caller = keys.iter().find(|key| key.file == "api.ts").unwrap();
    let work = keys
        .iter()
        .find(|key| key.qualified_name == ["work"])
        .unwrap();
    let other = keys
        .iter()
        .find(|key| key.qualified_name == ["other"])
        .unwrap();
    assert!(repo.current.graph.incoming[work].contains(caller));
    assert!(repo.current.graph.incoming[other].contains(caller));
    repo.checkpoint("both");
    repo.change(
        "api.ts",
        b"import {work} from './worker'; export function run() {work();}",
    )
    .unwrap();
    assert!(repo.current.graph.incoming[work].contains(caller));
    assert!(!repo.current.graph.incoming.contains_key(other));
    assert!(repo.current.graph.symbols.contains_key(other));
    repo.delete("worker.ts").unwrap();
    assert!(repo.current.graph.incoming.is_empty());
    assert_eq!(repo.current.graph.symbols.len(), 1);
    assert!(repo.diff("both").unwrap().contains("ts:api.ts::run"));
    repo.change("worker.ts", worker).unwrap();
    assert!(repo.current.graph.incoming[work].contains(caller));
    assert!(!repo.current.graph.incoming.contains_key(other));
    assert!(repo.checkpoints["both"].graph.incoming[other].contains(caller));
}

#[test]
fn checkpoints_survive_edits_deletions_and_name_replacement() {
    let mut repo = Repository::new().unwrap();
    let original = b"pub fn run() {}";
    let updated = b"pub fn run() { audit(); } fn audit() {}";
    repo.change("lib.rs", original).unwrap();
    repo.checkpoint("start");
    repo.change("lib.rs", updated).unwrap();
    let change = repo.diff("start").unwrap();
    assert!(!change.is_empty());
    repo.checkpoint("later");
    repo.delete("lib.rs").unwrap();
    assert!(!repo.diff("later").unwrap().is_empty());
    repo.change("lib.rs", updated).unwrap();
    assert_eq!(repo.diff("start").unwrap(), change);
    assert_eq!(repo.diff("later").unwrap(), "");
    repo.checkpoint("start");
    assert_eq!(repo.diff("start").unwrap(), "");
    repo.change("lib.rs", original).unwrap();
    assert_eq!(repo.diff("start").unwrap(), repo.diff("later").unwrap());
    assert!(!repo.diff("later").unwrap().is_empty());
    assert_eq!(
        repo.diff("missing").unwrap_err().to_string(),
        "unknown checkpoint: missing"
    );
}

#[test]
fn invalid_inputs_and_graph_errors_leave_state_unchanged() {
    let mut repo = Repository::new().unwrap();
    let original = b"pub fn run() {}";
    repo.change("lib.rs", original).unwrap();
    repo.checkpoint("before");
    repo.analyzers.parsed.clear();
    for path in [
        "",
        "/lib.rs",
        "../lib.rs",
        "./lib.rs",
        "src//lib.rs",
        "src/../lib.rs",
        "src\\lib.rs",
        "C:/lib.rs",
        "lib.js",
        "lib.d.ts",
        "lib.d.mts",
        "lib.d.cts",
    ] {
        assert!(repo.change(path, original).is_err(), "{path}");
        assert!(repo.delete(path).is_err(), "{path}");
    }
    assert!(repo.change("lib.rs", &[0xff]).is_err());
    assert!(repo.analyzers.parsed.is_empty());
    let error = repo
        .change("lib.rs", b"pub fn run() {} pub fn run() {}")
        .unwrap_err();
    assert!(error.to_string().contains("duplicate symbol identity"));
    assert!(!repo.change("lib.rs", original).unwrap());
    assert_eq!(repo.diff("before").unwrap(), "");
    assert_eq!(repo.current.files.len(), 1);
}
