//! Manual benchmark: cargo test --release resolve::bench::scaling -- --ignored --nocapture

use std::collections::BTreeMap;
use std::hint::black_box;
use std::time::Instant;

use crate::{Repository, analyze};

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
