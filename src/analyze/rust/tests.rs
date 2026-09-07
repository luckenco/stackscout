use crate::{analyze, graph, model, render};

fn graph(files: &[(&str, &str)]) -> graph::Graph {
    let mut parser = analyze::Analyzers::new().unwrap();
    let artifacts = files
        .iter()
        .map(|(path, source)| parser.file(path, source).unwrap())
        .collect::<Vec<_>>();
    crate::build(&artifacts).unwrap()
}

fn calls(graph: &graph::Graph, name: &str) -> Vec<String> {
    let node = graph
        .symbols
        .values()
        .find(|node| node.symbol.key.name() == name)
        .unwrap();
    node.steps
        .iter()
        .map(|step| {
            let model::ResolvedStep::Call(call) = step else {
                panic!("expected a call")
            };
            call.resolution.identity()
        })
        .collect()
}

#[test]
fn cfg_alternatives_preserve_candidates_and_stable_identities() {
    let source = r#"
        #[cfg(unix)] fn work() { left(); }
        #[cfg(not(unix))] fn work() { right(); }
        fn left() {} fn right() {} pub fn run() { work(); }
    "#;
    let before = graph(&[("lib.rs", source)]);
    assert_eq!(
        calls(&before, "run"),
        ["!work:rust:lib.rs::#[cfg(not(unix))]::work,rust:lib.rs::#[cfg(unix)]::work"]
    );
    for (key, callers) in &before.incoming {
        if key.qualified_name == ["work"] {
            assert_eq!(callers.len(), 1);
            assert_eq!(callers.first().unwrap().name(), "run");
        }
    }
    let reordered = r#"
        #[cfg(not(unix))] fn work() { right(); }
        #[cfg( unix )] fn work() { left(); }
        fn left() {} fn right() {} pub fn run() { work(); }
    "#;
    let after = graph(&[("lib.rs", reordered)]);
    assert_eq!(render::render(&crate::diff::compare(&before, &after)), "");
    let changed = graph(&[(
        "lib.rs",
        &source.replace("{ left(); }", "{ left(); right(); }"),
    )]);
    let diff = render::render(&crate::diff::compare(&before, &changed));
    assert!(diff.contains("rust:lib.rs::run"), "{diff}");
    assert!(diff.contains("#[cfg(unix)]::work"), "{diff}");
}

#[test]
fn cfg_on_modules_impls_and_cfg_attr_is_inherited() {
    let graph = graph(&[(
        "lib.rs",
        r#"
        #[cfg(unix)] mod platform { pub fn run() {} }
        #[cfg(not(unix))] mod platform { pub fn run() {} }
        struct S;
        #[cfg_attr(feature = "a", cfg(test))] impl S { fn work() {} }
        #[cfg(not(test))] impl S { fn work() {} }
    "#,
    )]);
    let names = graph
        .symbols
        .keys()
        .map(model::SymbolKey::name)
        .collect::<Vec<_>>();
    assert_eq!(names.len(), 4);
    assert!(names.contains(&"#[cfg(unix)]::platform::run".to_owned()));
    assert!(names.contains(&"#[cfg(not(unix))]::platform::run".to_owned()));
    assert!(names.contains(&"#[cfg_attr(feature=\"a\",cfg(test))]::S::work".to_owned()));
}

#[test]
fn duplicates_are_not_arbitrarily_discarded() {
    let mut parser = analyze::Analyzers::new().unwrap();
    for source in [
        "fn run() {} fn run() {}",
        "#[cfg(test)] fn run() {} #[cfg(test)] fn run() {}",
    ] {
        let artifact = parser.file("lib.rs", source).unwrap();
        assert!(
            crate::build([&artifact])
                .unwrap_err()
                .to_string()
                .contains("duplicate symbol identity")
        );
    }
}

#[test]
fn normalizes_paths_raw_names_parentheses_and_qualified_impls() {
    let graph = graph(&[(
        "lib.rs",
        r#"
        mod inner { pub struct S; pub fn r#leaf() {} }
        impl inner :: S { pub fn r#work() {} }
        use crate :: inner :: r#leaf as r#alias;
        pub fn run() {
            inner :: leaf(); inner::r#leaf(); alias(); r#alias();
            inner :: S :: work(); (/* callee */ inner::leaf)();
        }
    "#,
    )]);
    assert_eq!(
        calls(&graph, "run"),
        [
            "rust:lib.rs::inner::leaf",
            "rust:lib.rs::inner::leaf",
            "rust:lib.rs::inner::leaf",
            "rust:lib.rs::inner::leaf",
            "rust:lib.rs::inner::S::work",
            "rust:lib.rs::inner::leaf",
        ]
    );
}

#[test]
fn raw_bindings_still_shadow_functions() {
    let graph = graph(&[(
        "lib.rs",
        "fn work() {} fn run(r#work: fn()) { (work)(); r#work(); }",
    )]);
    assert_eq!(calls(&graph, "run"), ["~work", "~work"]);
}

#[test]
fn generic_specializations_remain_distinct_and_formatting_is_not_identity() {
    let source = "struct S<T>(T); impl S<u8> { fn work() {} } impl S<u16> { fn work() {} } fn run() { S::<u8>::work(); S::<u16>::work(); }";
    let before = graph(&[("lib.rs", source)]);
    let after = graph(&[("lib.rs", &source.replace("S<u8>", "S < u8 >"))]);
    assert_eq!(render::render(&crate::diff::compare(&before, &after)), "");
    assert_eq!(
        calls(&after, "run"),
        ["rust:lib.rs::S<u8>::work", "rust:lib.rs::S<u16>::work"]
    );
}

#[test]
fn spelling_preserves_literals_in_const_generics() {
    let mut parser = analyze::Analyzers::new().unwrap();
    let a = parser
        .file(
            "lib.rs",
            "struct S<const N: usize>; impl S<{ r#\"a b\"#.len() }> { fn work() {} }",
        )
        .unwrap();
    let b = parser
        .file(
            "lib.rs",
            "struct S<const N: usize>; impl S<{ r#\"ab\"#.len() }> { fn work() {} }",
        )
        .unwrap();
    assert_ne!(a.symbols[0].key, b.symbols[0].key);
    assert!(a.symbols[0].key.name().contains("r#\"a b\"#"));
}

#[test]
fn qualified_ufcs_resolves_but_trait_self_does_not_guess_dispatch() {
    let graph = graph(&[(
        "lib.rs",
        r#"
        mod traits { pub trait T { fn leaf(&self); fn run(&self); } }
        struct S;
        impl S { fn leaf(&self) {} }
        impl traits::T for S {
            fn leaf(&self) {}
            fn run(&self) { self.leaf(); Self::leaf(self); }
        }
        fn run(s: &S) { <S as traits :: T>::leaf(s); }
    "#,
    )]);
    assert_eq!(
        calls(&graph, "run"),
        ["rust:lib.rs::<S as traits::T>::leaf"]
    );
    assert_eq!(
        calls(&graph, "<S as traits::T>::run"),
        ["~self.leaf", "~Self::leaf"]
    );
}

#[test]
fn self_calls_follow_explicit_imports_across_impl_files() {
    let graph = graph(&[
        (
            "src/lib.rs",
            "mod extra; pub struct S; impl S { pub fn leaf(&self) {} }",
        ),
        (
            "src/extra.rs",
            "use crate::S; impl S { pub fn run(&self) { self.leaf(); Self::leaf(self); } }",
        ),
    ]);
    assert_eq!(
        calls(&graph, "S::run"),
        ["rust:src/lib.rs::S::leaf", "rust:src/lib.rs::S::leaf"]
    );
}

#[test]
fn trait_defaults_have_owners_without_claiming_dispatch() {
    let graph = graph(&[(
        "lib.rs",
        "fn leaf() {} trait T { fn required(&self); fn defaulted(&self) { leaf(); self.required(); } } struct S; impl T for S { fn required(&self) {} } fn run(s: &S) { T::defaulted(s); }",
    )]);
    assert_eq!(
        calls(&graph, "<trait T>::defaulted"),
        ["rust:lib.rs::leaf", "~self.required"]
    );
    assert_eq!(calls(&graph, "run"), ["?T::defaulted"]);
}

#[test]
fn trait_type_parameters_shadow_module_types() {
    let graph = graph(&[(
        "lib.rs",
        "struct T; impl T { fn leaf() {} } trait A { fn leaf(); } trait Trait<T: A> { fn defaulted(&self) { T::leaf(); } }",
    )]);
    assert_eq!(calls(&graph, "<trait Trait>::defaulted"), ["~T::leaf"]);
}

#[test]
fn opaque_calls_and_deferred_bodies_are_visible_without_inventing_edges() {
    let graph = graph(&[(
        "lib.rs",
        r#"
        fn leaf() {} fn factory() -> fn() { leaf }
        fn run(fp: fn()) {
            (*&fp)(); factory()();
            let _closure = || leaf();
            let _future = async { leaf(); };
            fn nested() { leaf(); }
            println!("{:?}", leaf());
        }
    "#,
    )]);
    assert_eq!(
        calls(&graph, "run"),
        [
            "~[callee *&fp]",
            "~[callee factory()]",
            "rust:lib.rs::factory",
            "~[closure body omitted]",
            "~[async body deferred]",
            "~[nested function body omitted]",
            "~[macro println! unexpanded]",
        ]
    );
    let leaf = graph
        .symbols
        .keys()
        .find(|key| key.name() == "leaf")
        .unwrap();
    assert!(!graph.incoming.contains_key(leaf));
}

#[test]
fn opaque_diff_labels_do_not_claim_local_bindings() {
    let before = graph(&[("lib.rs", "fn run() {}")]);
    let after = graph(&[("lib.rs", "fn run() { println!(\"hello\"); }")]);
    let diff = render::render(&crate::diff::compare(&before, &after));
    assert!(
        diff.contains("[uncertain: [macro println! unexpanded]]"),
        "{diff}"
    );
    assert!(!diff.contains("locally bound"));
}

#[test]
fn safe_foreign_modifiers_do_not_omit_the_file_or_hide_other_errors() {
    let mut parser = analyze::Analyzers::new().unwrap();
    for visibility in ["", "pub "] {
        let source = format!(
            "unsafe extern \"C\" {{ {visibility}safe fn foreign(); {visibility}safe static VALUE: u8; }} fn run() {{ foreign(); }}"
        );
        assert_eq!(calls(&graph(&[("lib.rs", &source)]), "run"), ["?foreign"]);
    }
    for source in [
        "extern \"C\" { safe fn foreign(); }",
        "unsafe extern \"C\" { safe unsafe fn foreign(); }",
        "unsafe extern \"C\" { pub wrong fn foreign(); }",
        "unsafe extern \"C\" { safe fn foreign( ; } fn run() {}",
        "unsafe extern \"C\" { safe fn foreign(); } fn broken( {}",
    ] {
        assert!(parser.file("lib.rs", source).is_err(), "{source}");
    }
}
