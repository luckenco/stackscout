pub fn run() {
    repo.change("lib.rs", b"./worker", r#"a.b::c"#, 1.25, '.').unwrap();
    repo.state.change("lib.rs").result.finish();
    "lib.rs".len();
    (1.25_f64).abs();
    workers["./worker"].run();
}
