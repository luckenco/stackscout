use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn fixture_root(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test/fixtures")
        .join(name)
}

fn fixture(name: &str) -> String {
    let root = fixture_root(name);
    let actual = stackscout::diff_dirs(root.join("before"), root.join("after")).unwrap();
    assert_output(&root.join("expected.txt"), actual)
}

fn diagnostic_fixture(name: &str) -> String {
    let root = fixture_root(name);
    let actual =
        stackscout::diff_dirs_with_diagnostics(root.join("before"), root.join("after")).unwrap();
    assert_output(&root.join("expected-diagnostics.txt"), actual)
}

fn assert_output(expected: &Path, actual: String) -> String {
    assert_eq!(actual, fs::read_to_string(expected).unwrap());
    actual
}

#[test]
fn typescript_adds_and_removes_calls() {
    fixture("typescript-basic");
}

#[test]
fn rust_adds_and_removes_calls() {
    fixture("rust-basic");
}

#[test]
fn duplicate_names_keep_file_qualified_identity() {
    let output = fixture("duplicate-names");
    assert!(output.contains("ts:a.ts::work"));
    assert!(!output.contains("ts:b.ts::work"));
}

#[test]
fn recursive_calls_stop_at_a_cycle() {
    let output = fixture("recursion");
    assert!(output.contains("[cycle]"));
}

#[test]
fn independent_changed_cycles_each_have_a_root() {
    let output = fixture("independent-root-cycles");
    assert_eq!(output.matches("[cycle]").count(), 2);
}

#[test]
fn independent_exported_cycles_each_show_the_changed_callee() {
    let output = fixture("exported-root-cycles");
    assert_eq!(output.matches("rust:lib.rs::changed").count(), 2);
}

#[test]
fn reordered_calls_are_semantic_changes() {
    fixture("reordered");
}

#[test]
fn changed_callee_affects_unchanged_exported_caller() {
    let output = fixture("impact");
    assert!(output.starts_with("  ts:api.ts::create\n"));
}

#[test]
fn ambiguous_calls_are_explicit() {
    let output = fixture("resolution");
    assert!(output.contains("[ambiguous: choose"));
}

#[test]
fn unresolved_changes_are_shown_without_unchanged_unresolved_noise() {
    let output = fixture("unresolved-calls");
    assert!(!output.contains("stableMissing"));
}

#[test]
fn unchanged_bodies_are_omitted_but_changed_paths_remain() {
    fixture("compact-bodies");
}

#[test]
fn compact_bodies_preserve_ambiguous_changed_paths_and_cycles() {
    fixture("compact-ambiguity");
}

#[test]
fn compact_bodies_preserve_truncation_in_both_directions() {
    let chain = (0..40)
        .map(|i| format!("fn f{i}() {{ f{}(); }}\n", i + 1))
        .collect::<String>();
    let before = format!("{chain}pub fn run() {{}}");
    let after = format!("{chain}pub fn run() {{ f0(); }}");
    for (before, after) in [(&before, &after), (&after, &before)] {
        let mut repo = stackscout::Repository::new().unwrap();
        repo.change("lib.rs", before.as_bytes()).unwrap();
        repo.checkpoint("before");
        repo.change("lib.rs", after.as_bytes()).unwrap();
        let output = repo.diff("before").unwrap();
        assert!(output.contains("[truncated]"), "{output}");
    }
}

#[test]
fn multiline_labels_are_escaped_without_collapsing_call_identity() {
    fixture("compact-labels");
}

#[test]
fn rust_call_literals_are_preserved() {
    fixture("rust-call-literals");
}

#[test]
fn typescript_call_literals_are_preserved() {
    fixture("typescript-call-literals");
}

#[test]
fn distinct_call_literals_remain_distinct_changes() {
    fixture("call-literal-transition");
}

#[test]
fn ambiguous_calls_show_which_possible_callee_changed() {
    fixture("ambiguous-impact");
}

#[test]
fn every_affected_root_shows_its_changed_path() {
    fixture("explanatory-root");
}

#[test]
fn deep_changes_fall_back_to_the_changed_symbol() {
    fixture("deep-change");
}

#[test]
fn resolution_changes_when_defining_files_change() {
    fixture("resolution-transition");
}

#[test]
fn rust_use_imports_resolve_modules_types_aliases_and_functions() {
    fixture("rust-use-imports");
}

#[test]
fn rust_use_changes_update_unchanged_callers() {
    fixture("rust-use-transition");
}

#[test]
fn rust_use_bindings_respect_lexical_scope_and_shadowing() {
    fixture("rust-use-scope");
}

#[test]
fn rust_use_bindings_preserve_ambiguity_and_unknown_alternatives() {
    fixture("rust-use-ambiguity");
}

#[test]
fn rust_module_lookup_collisions_preserve_all_candidates() {
    fixture("rust-module-collision");
}

#[test]
fn rust_calls_resolve_inside_inline_modules() {
    fixture("rust-inline-module");
}

#[test]
fn rust_calls_resolve_inside_file_modules() {
    fixture("rust-file-module");
}

#[test]
fn typescript_bindings_shadow_file_functions() {
    fixture("typescript-shadowing");
}

#[test]
fn typescript_block_bindings_do_not_shadow_outer_calls() {
    fixture("typescript-block-scope");
}

#[test]
fn typescript_var_bindings_apply_to_the_whole_function() {
    fixture("typescript-function-scope");
}

#[test]
fn typescript_object_methods_do_not_collide_with_class_methods() {
    fixture("typescript-object-methods");
}

#[test]
fn typescript_accessors_do_not_collide_with_class_methods() {
    fixture("typescript-accessors");
}

#[test]
fn typescript_static_and_instance_methods_have_distinct_identities() {
    fixture("typescript-static-methods");
}

#[test]
fn typescript_imports_do_not_resolve_private_symbols() {
    let output = fixture("typescript-private-import");
    assert!(!output.contains("ts:main.ts::run"));
}

#[test]
fn rust_trait_impls_have_distinct_symbol_identities() {
    fixture("rust-trait-impls");
}

#[test]
fn typescript_export_changes_are_semantic() {
    fixture("typescript-export");
}

#[test]
fn typescript_export_aliases_resolve_to_local_symbols() {
    fixture("typescript-export-alias");
}

#[test]
fn typescript_type_star_export_does_not_omit_the_file() {
    fixture("typescript-type-star-export");
}

#[test]
fn typescript_default_imports_propagate_changes() {
    fixture("typescript-default-import");
}

#[test]
fn typescript_callable_forms_propagate_changes() {
    fixture("typescript-callables");
}

#[test]
fn typescript_module_extensions_are_discovered() {
    fixture("module-extensions");
}

#[test]
fn typescript_branches_preserve_call_context() {
    fixture("typescript-branches");
}

#[test]
fn rust_branches_preserve_call_context() {
    fixture("rust-branches");
}

#[test]
fn closures_do_not_contribute_calls_to_their_parent() {
    let output = fixture("closure-ownership");
    assert!(!output.contains("hidden"));
}

#[test]
fn malformed_files_are_reported_and_omitted_from_both_sides() {
    let output = fixture("malformed");
    assert!(!output.contains("ts:broken.ts::"));
    assert!(!output.contains("bad-export.ts"));
}

#[test]
#[cfg(unix)]
fn control_characters_in_source_filenames_cannot_forge_output() {
    for name in [
        "bad\n+ forged.rs",
        "bad\r.ts",
        "bad\u{1b}[2J.rs",
        "bad\t.rs",
    ] {
        let before = tempfile::tempdir().unwrap();
        let after = tempfile::tempdir().unwrap();
        fs::write(after.path().join(name), "fn broken(").unwrap();
        for diagnostics in [false, true] {
            let error = if diagnostics {
                stackscout::diff_dirs_with_diagnostics(before.path(), after.path())
            } else {
                stackscout::diff_dirs(before.path(), after.path())
            }
            .unwrap_err()
            .to_string();
            assert!(error.contains("normalized repository-relative source path"));
            assert!(!error.chars().any(char::is_control), "{error:?}");
        }
        let output = Command::new(env!("CARGO_BIN_EXE_stackscout"))
            .arg("diff-dir")
            .arg(before.path())
            .arg(after.path())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert_eq!(error.lines().count(), 1, "{error:?}");
        assert!(!error.trim_end().chars().any(char::is_control), "{error:?}");
    }
}

#[test]
fn unchanged_diagnostics_are_hidden_by_default() {
    assert_eq!(fixture("unchanged-incomplete"), "");
}

#[test]
fn diagnostics_are_detailed_only_when_requested() {
    fixture("diagnostics");
    diagnostic_fixture("diagnostics");
    diagnostic_fixture("unchanged-incomplete");
}

#[test]
fn diagnostics_flag_enables_detailed_output() {
    let root = fixture_root("unchanged-incomplete");
    let output = Command::new(env!("CARGO_BIN_EXE_stackscout"))
        .args(["diff-dir", "--diagnostics"])
        .arg(root.join("before"))
        .arg(root.join("after"))
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        fs::read_to_string(root.join("expected-diagnostics.txt")).unwrap()
    );
}

#[test]
fn nested_private_changes_have_one_explanatory_root() {
    let output = fixture("nested-private-change");
    assert_eq!(output.matches("ts:main.ts::usage").count(), 1);
}

#[test]
fn stackscout_can_analyze_itself() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    assert_eq!(stackscout::diff_dirs(&root, &root).unwrap(), "");
}

#[test]
fn output_is_deterministic() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test/fixtures")
        .join("impact");
    let first = stackscout::diff_dirs(root.join("before"), root.join("after")).unwrap();
    for _ in 0..10 {
        assert_eq!(
            stackscout::diff_dirs(root.join("before"), root.join("after")).unwrap(),
            first
        );
    }
}
