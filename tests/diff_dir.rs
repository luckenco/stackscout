use std::fs;
use std::path::PathBuf;

fn fixture(name: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("test/fixtures")
        .join(name);
    let actual = stackscout::diff_dirs(root.join("before"), root.join("after")).unwrap();
    let expected = fs::read_to_string(root.join("expected.txt")).unwrap();
    assert_eq!(actual, expected);
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
fn reordered_calls_are_semantic_changes() {
    fixture("reordered");
}

#[test]
fn changed_callee_affects_unchanged_exported_caller() {
    let output = fixture("impact");
    assert!(output.starts_with("  ts:api.ts::create\n"));
}

#[test]
fn unresolved_and_ambiguous_calls_are_explicit() {
    let output = fixture("resolution");
    assert!(output.contains("[unresolved: missing]"));
    assert!(output.contains("[ambiguous: choose"));
}

#[test]
fn ambiguous_calls_show_which_possible_callee_changed() {
    fixture("ambiguous-impact");
}

#[test]
fn resolution_changes_when_defining_files_change() {
    fixture("resolution-transition");
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
