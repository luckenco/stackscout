use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::sync::Arc;

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
fn rust_unsupported_edits_signal_without_inventing_call_paths() {
    for (category, before, after) in [
        (
            "const",
            "const LIMIT: usize = 1;",
            "const LIMIT: usize = 2;",
        ),
        (
            "struct",
            "struct Config { size: usize }",
            "struct Config { size: u64 }",
        ),
        (
            "macro",
            "macro_rules! value { () => { 1 } }",
            "macro_rules! value { () => { 2 } }",
        ),
        ("struct", "struct Config(usize);", "struct Config(u64);"),
        (
            "const",
            "struct Config; impl Config { const LIMIT: usize = 1; }",
            "struct Config; impl Config { const LIMIT: usize = 2; }",
        ),
        (
            "const",
            "fn run() { const LIMIT: usize = 1; }",
            "fn run() { const LIMIT: usize = 2; }",
        ),
    ] {
        let mut repo = Repository::new().unwrap();
        repo.change("src/lib.rs", before.as_bytes()).unwrap();
        repo.checkpoint("before");
        repo.change("src/lib.rs", after.as_bytes()).unwrap();
        let output = repo.diff("before").unwrap();
        assert!(output.starts_with(&format!("! unsupported (rust {category}): src/lib.rs\n")));
        if before.starts_with("fn run") {
            assert!(output.contains("~ rust:src/lib.rs::run"));
        } else {
            assert_eq!(output.lines().count(), 1);
        }
        repo.checkpoint("after");
        assert_eq!(repo.diff("after").unwrap(), "");
    }

    let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
    fs::write(
        dirs[0].path().join("lib.rs"),
        "const LIMIT: usize = 1; fn run() {}",
    )
    .unwrap();
    fs::write(
        dirs[1].path().join("lib.rs"),
        "const LIMIT: usize = 2; fn run() { work(); }",
    )
    .unwrap();
    let mut repo = Repository::new().unwrap();
    repo.change("lib.rs", &fs::read(dirs[0].path().join("lib.rs")).unwrap())
        .unwrap();
    repo.checkpoint("baseline");
    repo.change("lib.rs", &fs::read(dirs[1].path().join("lib.rs")).unwrap())
        .unwrap();
    let forward = repo.diff("baseline").unwrap();
    assert!(forward.contains("! unsupported (rust const): lib.rs"));
    assert!(forward.contains("work"));
    assert_eq!(
        forward,
        crate::diff_dirs(dirs[0].path(), dirs[1].path()).unwrap()
    );
    assert!(
        crate::diff_dirs(dirs[1].path(), dirs[0].path())
            .unwrap()
            .contains("! unsupported (rust const): lib.rs")
    );
    repo.change("lib.rs", b"const LIMIT: usize = 1; fn run() {}")
        .unwrap();
    assert_eq!(repo.diff("baseline").unwrap(), "");

    for (before, after) in [
        (
            "struct Config { size: usize }",
            "struct Config { /* note */ size : usize }",
        ),
        (
            "macro_rules! value { () => { 1 } }",
            "macro_rules! value { ( ) => { /* note */ 1 } }",
        ),
    ] {
        let mut repo = Repository::new().unwrap();
        repo.change("lib.rs", before.as_bytes()).unwrap();
        repo.checkpoint("before");
        repo.change("lib.rs", after.as_bytes()).unwrap();
        assert_eq!(repo.diff("before").unwrap(), "");
    }
    let mut repo = Repository::new().unwrap();
    repo.change("lib.rs", b"const LIMIT: usize = 1; // before")
        .unwrap();
    repo.checkpoint("before");
    repo.change("lib.rs", b"/* after */ const LIMIT : usize = 1 ;")
        .unwrap();
    assert_eq!(repo.diff("before").unwrap(), "");
    repo.change("lib.rs", b"const LIMIT: usize = 2; fn broken(")
        .unwrap();
    assert!(
        repo.diff("before")
            .unwrap()
            .contains("! incomplete (introduced; omitted from both): lib.rs")
    );
    assert!(!repo.diff("before").unwrap().contains("! unsupported"));
}

#[test]
fn rust_unsupported_signals_are_ordered_and_bounded() {
    let mut repo = Repository::new().unwrap();
    let baseline = (0..600)
        .map(|index| {
            (
                format!("{index:04}_{}.rs", "a".repeat(100)),
                Some(b"const LIMIT: usize = 1;".to_vec()),
            )
        })
        .collect();
    repo.update(&baseline).unwrap();
    repo.checkpoint("before");
    let changes = (0..600)
        .map(|index| {
            (
                format!("{index:04}_{}.rs", "a".repeat(100)),
                Some(b"const LIMIT: usize = 2;".to_vec()),
            )
        })
        .collect();
    repo.update(&changes).unwrap();
    let output = repo.diff("before").unwrap();
    assert!(output.starts_with(&format!(
        "! unsupported (rust const): 0000_{}.rs\n",
        "a".repeat(100)
    )));
    assert!(output.ends_with(crate::render::TRUNCATED));
    assert!(output.len() <= 48 * 1024 + crate::render::TRUNCATED.len());
}

#[test]
fn failure_filtering_reuses_graphs_unless_a_valid_artifact_is_removed() {
    let mut repo = Repository::new().unwrap();
    repo.change("bad.ts", b"export function work() {}").unwrap();
    repo.change(
        "api.ts",
        b"import { work } from './bad'; export function run() { work(); }",
    )
    .unwrap();
    let before = repo.snapshot();
    repo.change("bad.ts", b"export function work(").unwrap();
    let after = repo.snapshot();
    let omitted = std::collections::BTreeSet::from(["bad.ts".to_owned(), "missing.ts".to_owned()]);
    let filtered = before.without(&omitted).unwrap();
    assert!(!Arc::ptr_eq(&before.graph, &filtered));
    assert!(Arc::ptr_eq(&after.graph, &after.without(&omitted).unwrap()));
    assert!(
        crate::diff::compare(&filtered, &after.graph)
            .roots
            .is_empty()
    );
    assert!(
        before
            .compare(&after, false, false)
            .unwrap()
            .contains("bad.ts")
    );
    assert!(
        after
            .compare(&after, true, false)
            .unwrap()
            .contains("bad.ts")
    );

    repo.change(
        "api.ts",
        b"import { work } from './bad'; export function run() { work(); extra(); }",
    )
    .unwrap();
    let next = repo.snapshot();
    assert!(Arc::ptr_eq(&next.graph, &next.without(&omitted).unwrap()));
    assert!(
        after
            .compare(&next, false, false)
            .unwrap()
            .contains("extra")
    );
    assert!(
        after
            .compare(&next, true, false)
            .unwrap()
            .contains("bad.ts")
    );
    #[cfg(unix)]
    {
        let scope = crate::review::Scope {
            files: vec!["api.ts".to_owned()],
            symbols: vec![],
        };
        let review = before.review(&after, &scope).unwrap();
        assert!(review.witnesses.is_empty());
        assert_eq!(review.analysis_issues.len(), 1);
        let review = after.review(&next, &scope).unwrap();
        assert!(!review.witnesses.is_empty());
        assert_eq!(review.analysis_issues.len(), 2);
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
            .compare(&repo.snapshot(), false, false)
            .unwrap()
            .contains("unknown")
    );
    assert_eq!(published.compare(&published, false, false).unwrap(), "");
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
    Arc::make_mut(
        Arc::make_mut(&mut repo.current.files)
            .get_mut("view.tsx")
            .unwrap(),
    )
    .key
    .version = 0;
    assert!(repo.change("view.tsx", source).unwrap());
    Arc::make_mut(
        Arc::make_mut(&mut repo.current.files)
            .get_mut("view.tsx")
            .unwrap(),
    )
    .key
    .syntax = model::Syntax::TypeScript;
    assert!(repo.change("view.tsx", source).unwrap());
    assert_eq!(repo.analyzers.parsed, ["view.tsx", "view.tsx"]);
}

#[test]
fn snapshots_share_unchanged_artifacts_and_release_replaced_generations() {
    let mut repo = Repository::new().unwrap();
    repo.change("lib.rs", b"pub fn run() {}").unwrap();
    repo.change("other.rs", b"fn other() {}").unwrap();
    repo.checkpoint("saved");
    let before = repo.snapshot();
    assert!(Arc::ptr_eq(&before.files, &repo.checkpoints["saved"].files));
    assert!(Arc::ptr_eq(&before.graph, &repo.checkpoints["saved"].graph));
    let graph = Arc::downgrade(&before.graph);
    repo.update(&BTreeMap::from([(
        "lib.rs".to_owned(),
        Some(b"pub fn run() { audit(); }".to_vec()),
    )]))
    .unwrap();
    assert!(Arc::ptr_eq(
        &before.files["other.rs"],
        &repo.current.files["other.rs"]
    ));
    assert!(!Arc::ptr_eq(
        &before.files["lib.rs"],
        &repo.current.files["lib.rs"]
    ));
    assert!(!Arc::ptr_eq(&before.graph, &repo.current.graph));
    assert!(repo.diff("saved").unwrap().contains("audit"));
    repo.checkpoint("saved");
    drop(before);
    assert!(graph.upgrade().is_none());
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
fn cargo_binary_does_not_call_same_package_library_prelude_shadow() {
    let root = tempfile::tempdir().unwrap();
    for (name, lib) in [
        ("before", "pub fn drop<T>(_: T) {}\n"),
        ("after", "pub fn drop<T>(_: T) { audit(); } fn audit() {}\n"),
    ] {
        let dir = root.path().join(name);
        fs::create_dir_all(dir.join("src")).unwrap();
        fs::write(
            dir.join("Cargo.toml"),
            "[package]\nname = \"ownership_repro\"\nversion = \"0.1.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(dir.join("src/lib.rs"), lib).unwrap();
        fs::write(dir.join("src/main.rs"), "fn main() { drop(1); }\n").unwrap();
    }
    // `drop` in the binary comes from the standard prelude, not the distinct library target.
    let output = crate::diff_dirs(root.path().join("before"), root.path().join("after")).unwrap();
    assert!(output.contains("rust:src/lib.rs::drop"));
    let mut analyzers = crate::analyze::Analyzers::new().unwrap();
    let directory = analyzers.directory(&root.path().join("before")).unwrap();
    let graph = crate::build(&directory.artifacts).unwrap();
    let lib = graph
        .symbols
        .keys()
        .find(|key| key.file == "src/lib.rs")
        .unwrap();
    assert!(
        !graph.incoming.contains_key(lib),
        "binary call must not point into the library"
    );
}

#[test]
fn rust_roots_do_not_cross_resolve_and_unknown_siblings_stay_local() {
    let mut repo = Repository::new().unwrap();
    repo.change(
        "src/lib.rs",
        b"pub fn run() { std::mem::drop(1); }
",
    )
    .unwrap();
    repo.change(
        "src/main.rs",
        b"fn main() { drop(1); } mod std { pub mod mem { pub fn drop<T>(_: T) {} } }
",
    )
    .unwrap();
    repo.change(
        "src/foo.rs",
        b"pub fn drop<T>(_: T) {} pub fn local() { drop(1); }
",
    )
    .unwrap();
    let graph = &repo.current.graph;
    let main_drop = graph
        .symbols
        .keys()
        .find(|key| key.file == "src/main.rs" && key.name() == "std::mem::drop")
        .unwrap();
    let sibling_drop = graph
        .symbols
        .keys()
        .find(|key| key.file == "src/foo.rs" && key.name() == "drop")
        .unwrap();
    let sibling_local = graph
        .symbols
        .keys()
        .find(|key| key.file == "src/foo.rs" && key.name() == "local")
        .unwrap();
    assert!(!graph.incoming.contains_key(main_drop));
    assert_eq!(
        graph.incoming[sibling_drop].iter().collect::<Vec<_>>(),
        vec![sibling_local]
    );
}

#[test]
fn rust_declared_modules_rebuild_ownership_when_root_changes() {
    let mut repo = Repository::new().unwrap();
    repo.change("src/lib.rs", b"mod tasks; pub fn run() { tasks::work(); }")
        .unwrap();
    repo.change("src/main.rs", b"fn main() { tasks::work(); }")
        .unwrap();
    repo.change("src/tasks.rs", b"pub fn work() {}").unwrap();
    let work = repo
        .current
        .graph
        .symbols
        .keys()
        .find(|key| key.file == "src/tasks.rs")
        .unwrap()
        .clone();
    let lib = repo
        .current
        .graph
        .symbols
        .keys()
        .find(|key| key.file == "src/lib.rs")
        .unwrap()
        .clone();
    assert_eq!(
        repo.current.graph.incoming[&work]
            .iter()
            .collect::<Vec<_>>(),
        vec![&lib]
    );
    repo.change("src/lib.rs", b"pub fn run() { tasks::work(); }")
        .unwrap();
    assert!(!repo.current.graph.incoming.contains_key(&work));
    repo.change("src/main.rs", b"mod tasks; fn main() { tasks::work(); }")
        .unwrap();
    let main = repo
        .current
        .graph
        .symbols
        .keys()
        .find(|key| key.file == "src/main.rs")
        .unwrap();
    assert_eq!(
        repo.current.graph.incoming[&work]
            .iter()
            .collect::<Vec<_>>(),
        vec![main]
    );
}

#[test]
fn rust_flat_bin_root_resolves_declared_child_module() {
    let mut repo = Repository::new().unwrap();
    repo.change("src/lib.rs", b"pub fn work() {}").unwrap();
    repo.change(
        "src/bin/tool.rs",
        b"mod helper; fn main() { helper::work(); }",
    )
    .unwrap();
    repo.change("src/bin/tool/helper.rs", b"pub fn work() {}")
        .unwrap();
    let work = repo
        .current
        .graph
        .symbols
        .keys()
        .find(|key| key.file == "src/bin/tool/helper.rs")
        .unwrap();
    let main = repo
        .current
        .graph
        .symbols
        .keys()
        .find(|key| key.file == "src/bin/tool.rs")
        .unwrap();
    assert_eq!(
        repo.current.graph.incoming[work].iter().collect::<Vec<_>>(),
        vec![main]
    );
}

#[test]
fn rust_single_root_keeps_module_resolution() {
    let mut repo = Repository::new().unwrap();
    repo.change(
        "src/lib.rs",
        b"mod foo; pub fn run() { foo::work(); }
",
    )
    .unwrap();
    repo.change(
        "src/foo.rs",
        b"pub fn work() {}
",
    )
    .unwrap();
    let graph = &repo.current.graph;
    let run = graph
        .symbols
        .keys()
        .find(|key| key.file == "src/lib.rs")
        .unwrap();
    let work = graph
        .symbols
        .keys()
        .find(|key| key.file == "src/foo.rs")
        .unwrap();
    assert!(graph.incoming[work].contains(run));
}

#[test]
fn rust_root_changes_rebuild_resolution_from_cached_files() {
    let mut repo = Repository::new().unwrap();
    repo.change(
        "src/lib.rs",
        b"pub fn drop<T>(_: T) {}
",
    )
    .unwrap();
    repo.change(
        "src/foo.rs",
        b"pub fn run() { crate::drop(1); }
",
    )
    .unwrap();
    let lib = repo
        .current
        .graph
        .symbols
        .keys()
        .find(|key| key.file == "src/lib.rs")
        .unwrap()
        .clone();
    assert!(repo.current.graph.incoming.contains_key(&lib));
    repo.change(
        "src/main.rs",
        b"fn main() {}
",
    )
    .unwrap();
    assert!(!repo.current.graph.incoming.contains_key(&lib));
    repo.delete("src/main.rs").unwrap();
    assert!(repo.current.graph.incoming.contains_key(&lib));
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
        "line\nbreak.rs",
        "return\rcarriage.ts",
        "escape\u{1b}[2J.rs",
        "tab\tname.rs",
        "delete\u{7f}.rs",
        "control\u{85}.tsx",
    ] {
        for error in [
            repo.change(path, original).unwrap_err(),
            repo.delete(path).unwrap_err(),
        ] {
            assert!(
                !error.to_string().chars().any(char::is_control),
                "{error:?}"
            );
        }
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
