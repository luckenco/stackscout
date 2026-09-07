#![cfg(unix)]
// Opt-in end-to-end measurements, not timing assertions in the normal test suite.
use std::fs;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

struct Owner(Option<Child>);

fn binary() -> std::ffi::OsString {
    std::env::var_os("STACKSCOUT_MEASURE_BIN")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_stackscout").into())
}

impl Owner {
    fn start(root: &Path) -> Self {
        Self(Some(
            Command::new(binary())
                .arg("serve")
                .arg(root)
                .env("STACKSCOUT_WATCH", "0")
                .env("STACKSCOUT_INDEX_THREADS", "4")
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        ))
    }

    #[expect(
        clippy::zombie_processes,
        reason = "wait4 reaps the child and measures its peak RSS"
    )]
    fn stop(&mut self) -> u64 {
        let mut child = self.0.take().unwrap();
        child.kill().unwrap();
        let mut status = 0;
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // This child belongs exclusively to this benchmark; wait4 captures its lifetime peak RSS.
        let pid = unsafe { libc::wait4(child.id() as i32, &mut status, 0, usage.as_mut_ptr()) };
        assert_eq!(pid, child.id() as i32);
        let peak = unsafe { usage.assume_init() }.ru_maxrss as u64;
        if cfg!(target_os = "macos") {
            peak
        } else {
            peak * 1024
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn request(root: &Path, operation: Value) -> Value {
    let mut operation = operation;
    operation["version"] = json!(1);
    let text = stackscout::service::request(root, &operation.to_string()).unwrap();
    let result: Value = serde_json::from_str(&text).unwrap();
    assert!(result["error"].is_null(), "{result}");
    result
}

fn ready(root: &Path) -> Vec<f64> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut reads = Vec::new();
    loop {
        let start = Instant::now();
        if let Ok(text) = stackscout::service::request(root, r#"{"version":1,"op":"status"}"#) {
            let result: Value = serde_json::from_str(&text).unwrap();
            assert!(result["last_update_error"].is_null(), "{result}");
            if result["index_complete"] == true {
                break;
            }
            assert!(result["generation"].is_null());
            reads.push(start.elapsed().as_secs_f64() * 1000.0);
        }
        assert!(Instant::now() < deadline, "owner never became ready");
        std::thread::sleep(Duration::from_millis(1));
    }
    reads
}

fn summary(samples: Vec<f64>) -> Value {
    if samples.is_empty() {
        return json!({"samples": [], "p50": null, "p95": null});
    }
    let mut sorted = samples.clone();
    sorted.sort_by(f64::total_cmp);
    json!({"p50": sorted[(sorted.len() - 1) / 2],
        "p95": sorted[(sorted.len() * 95).div_ceil(100) - 1], "samples": samples})
}

fn measure(count: usize, mut operation: impl FnMut(usize)) -> Value {
    let mut samples = Vec::new();
    for index in 0..count {
        let start = Instant::now();
        operation(index);
        samples.push(start.elapsed().as_secs_f64() * 1000.0);
    }
    summary(samples)
}

fn copy_sources(source: &Path, target: &Path, hash: &mut blake3::Hasher) -> (usize, usize) {
    fs::create_dir_all(target).unwrap();
    let mut paths = fs::read_dir(source)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    paths.sort();
    let (mut files, mut bytes) = (0, 0);
    for path in paths {
        hash.update(path.file_name().unwrap().as_encoded_bytes());
        if path.is_dir() {
            let counts = copy_sources(&path, &target.join(path.file_name().unwrap()), hash);
            files += counts.0;
            bytes += counts.1;
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            let content = fs::read(&path).unwrap();
            hash.update(&content);
            fs::write(target.join(path.file_name().unwrap()), &content).unwrap();
            files += 1;
            bytes += content.len();
        }
    }
    (files, bytes)
}

fn rss(owner: &Owner) -> u64 {
    let output = Command::new("ps")
        .args([
            "-o",
            "rss=",
            "-p",
            &owner.0.as_ref().unwrap().id().to_string(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .unwrap()
        .trim()
        .parse::<u64>()
        .unwrap()
        * 1024
}

// Generated independently of the self-hosted corpus: same input for any measured binary.
fn diff_costs(root: &Path) -> Vec<Value> {
    let mut output = Vec::new();
    for name in [
        "rust-dense",
        "ts-dense",
        "diamond",
        "failure-persistent",
        "failure-introduced",
    ] {
        let workspace = root.join(name);
        fs::create_dir(&workspace).unwrap();
        let mut sources = Vec::new();
        if name == "diamond" {
            let mut source = String::from("export function run() { a0(); b0(); }\n");
            for level in 0..10 {
                let mut calls = format!("a{}(); b{}();", level + 1, level + 1);
                if level == 9 {
                    calls = "before_call();".to_owned();
                }
                source.push_str(&format!(
                    "function a{level}() {{ {calls} }}\nfunction b{level}() {{ {calls} }}\n"
                ));
            }
            sources.push(("api.ts".to_owned(), source));
        } else {
            let (keyword, public, extension) = match name {
                "rust-dense" => ("fn", "pub ", "rs"),
                _ => ("function", "export ", "ts"),
            };
            for file in 0..1000 {
                let mut source = String::new();
                for node in 0..10 {
                    let visibility = if node == 0 { public } else { "" };
                    let mut target = format!("f{file}_{}", node + 1);
                    if node == 9 {
                        target = "before_call".to_owned();
                    }
                    source.push_str(&format!(
                        "{visibility}{keyword} f{file}_{node}() {{\n  {target}();\n}}\n"
                    ));
                }
                sources.push((format!("f{file}.{extension}"), source));
            }
        }
        if name.starts_with("failure-") {
            let mut source = "export function broken() {}";
            if name == "failure-persistent" {
                source = "export function broken(";
            }
            sources.push(("broken.ts".to_owned(), source.to_owned()));
        }
        let mut hash = blake3::Hasher::new();
        for (path, source) in &sources {
            fs::write(workspace.join(path), source).unwrap();
            hash.update(path.as_bytes());
            hash.update(source.as_bytes());
        }
        let mut index = 0;
        if name == "failure-introduced" {
            index = sources.len() - 1;
        }
        let (path, before) = &sources[index];
        let mut after = before.replace("before_call", "after_call");
        if name == "failure-introduced" {
            after = "export function broken(".to_owned();
        }
        let mut owner = Owner::start(&workspace);
        ready(&workspace);
        request(&workspace, json!({"op": "checkpoint", "name": "before"}));
        fs::write(workspace.join(path), &after).unwrap();
        let state = request(&workspace, json!({"op": "sync", "paths": [path]}));
        assert_eq!(state["files_parsed"], sources.len() + 1);
        request(&workspace, json!({"op": "checkpoint", "name": "same"}));
        let diff = json!({"op": "diff", "name": "before"});
        let evidence = request(&workspace, diff.clone());
        let mut expected = "after_call";
        if name == "failure-introduced" {
            expected = "broken.ts";
        }
        assert!(evidence["diff"].as_str().unwrap().contains(expected));
        for _ in 0..5 {
            request(&workspace, diff.clone());
        }
        let changed = measure(30, |_| {
            request(&workspace, diff.clone());
        });
        let unchanged = measure(30, |_| {
            assert_eq!(
                request(&workspace, json!({"op": "diff", "name": "same"}))["diff"],
                ""
            );
        });
        let review = json!({"op": "review", "name": "measure", "baseline": "before",
            "baseline_generation": 1, "scope": {"files": [path]}});
        let review_evidence = request(&workspace, review.clone());
        for _ in 0..5 {
            request(&workspace, review.clone());
        }
        let reviews = measure(30, |_| {
            request(&workspace, review.clone());
        });
        assert_eq!(
            request(&workspace, json!({"op": "status"}))["files_parsed"],
            state["files_parsed"]
        );
        let mut updates = Vec::new();
        let mut fresh = Vec::new();
        for index in 0..30 {
            let start = Instant::now();
            let source = if index % 2 == 0 { before } else { &after };
            fs::write(workspace.join(path), source).unwrap();
            let result = request(&workspace, json!({"op": "sync", "paths": [path]}));
            assert_eq!(result["generation"], index + 3);
            assert_eq!(result["files_parsed"], sources.len() + index + 2);
            updates.push(start.elapsed().as_secs_f64() * 1000.0);
            if index % 2 == 1 {
                // A new generation number forces a cache miss despite identical source text.
                let start = Instant::now();
                let result = request(&workspace, diff.clone());
                fresh.push(start.elapsed().as_secs_f64() * 1000.0);
                assert_eq!(result["diff"], evidence["diff"]);
                assert_eq!(result["truncated"], evidence["truncated"]);
            }
        }
        let resident = rss(&owner);
        let peak = owner.stop();
        output.push(json!({"name": name, "files": sources.len(), "source_blake3": hash.finalize().to_hex().to_string(),
            "diff_blake3": blake3::hash(evidence["diff"].as_str().unwrap().as_bytes()).to_hex().to_string(),
            "review_blake3": blake3::hash(&serde_json::to_vec(&review_evidence["review"]).unwrap()).to_hex().to_string(),
            "review_ms": reviews, "truncated": evidence["truncated"], "diff_ms": changed, "same_generation_ms": unchanged,
            "fresh_diff_ms": summary(fresh), "write_sync_ms": summary(updates), "rss_bytes": resident, "peak_rss_bytes": peak}));
    }
    output
}

#[test]
#[ignore = "manual release measurement; see docs/performance.md"]
fn service_costs() {
    if cfg!(debug_assertions) {
        panic!("use cargo test --release");
    }
    let output = std::env::var("STACKSCOUT_MEASURE_OUT")
        .expect("set STACKSCOUT_MEASURE_OUT to a JSON output path");
    let directory = tempfile::Builder::new()
        .prefix("ss-perf-")
        .tempdir()
        .unwrap();
    let root = directory.path().canonicalize().unwrap();
    let mut hash = blake3::Hasher::new();
    let (files, bytes) = copy_sources(Path::new("src"), &root.join("src"), &mut hash);
    let mut cold = Vec::new();
    let mut peaks = Vec::new();
    let mut indexing_reads = Vec::new();
    for _ in 0..10 {
        let start = Instant::now();
        let mut owner = Owner::start(&root);
        indexing_reads.extend(ready(&root));
        cold.push(start.elapsed().as_secs_f64() * 1000.0);
        let status = request(&root, json!({"op": "status"}));
        assert_eq!(status["files"], files);
        assert_eq!(status["files_parsed"], files);
        peaks.push(owner.stop() as f64);
    }
    let mut owner = Owner::start(&root);
    ready(&root);
    let graph = json!({"op": "callees", "symbol": "rust:src/lib.rs::diff_dirs", "depth": 2});
    for _ in 0..5 {
        request(&root, graph.clone());
    }
    let evidence = request(&root, graph.clone());
    let warm = measure(100, |_| {
        request(&root, graph.clone());
    });
    let connection = measure(100, |_| {
        request(&root, json!({"op": "status"}));
    });
    let second_client = measure(30, |_| {
        let output = Command::new(binary())
            .arg("request")
            .arg(&root)
            .arg(r#"{"version":1,"op":"status"}"#)
            .output()
            .unwrap();
        assert!(output.status.success());
        let response: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert!(response["error"].is_null());
        assert_eq!(response["files_parsed"], files);
    });
    let original = fs::read_to_string(root.join("src/lib.rs")).unwrap();
    let mut updates = Vec::new();
    for index in 0..100 {
        fs::write(
            root.join("src/lib.rs"),
            format!("{original}\nfn measurement() {{ marker{index}(); }}\n"),
        )
        .unwrap();
        let start = Instant::now();
        let response = request(&root, json!({"op": "sync", "paths": ["src/lib.rs"]}));
        updates.push(start.elapsed().as_secs_f64() * 1000.0);
        assert_eq!(response["generation"], index + 2);
        assert_eq!(response["files_parsed"], files + index + 1);
    }
    let mut retention = vec![json!({"pins": 0, "rss_bytes": rss(&owner)})];
    for index in 0..32 {
        request(
            &root,
            json!({"op": "checkpoint", "name": format!("pin{index}")}),
        );
        fs::write(
            root.join("src/lib.rs"),
            format!("{original}\nfn retained() {{ marker{index}(); }}\n"),
        )
        .unwrap();
        request(&root, json!({"op": "sync", "paths": ["src/lib.rs"]}));
        if [0, 7, 15, 31].contains(&index) {
            retention.push(json!({"pins": index + 1, "rss_bytes": rss(&owner)}));
        }
    }
    for index in 0..32 {
        request(
            &root,
            json!({"op": "forget", "name": format!("pin{index}")}),
        );
    }
    retention.push(json!({"pins": 0, "after_forget": true, "rss_bytes": rss(&owner)}));
    let lifetime_peak = owner.stop();

    let mut owner = Owner::start(&root);
    ready(&root);
    let seed = request(&root, json!({"op": "checkpoint", "name": "session"}));
    let mut rolling = vec![json!({"reviews": 0, "rss_bytes": rss(&owner)})];
    for index in 0..32 {
        fs::write(
            root.join("src/lib.rs"),
            format!("{original}\nfn rolling() {{ marker{index}(); }}\n"),
        )
        .unwrap();
        request(&root, json!({"op": "sync", "paths": ["src/lib.rs"]}));
        let review = request(
            &root,
            json!({"op": "review", "name": "scope", "baseline": "session",
            "baseline_generation": seed["checkpoint_generation"], "scope": {"files": ["src/lib.rs"]}}),
        );
        request(
            &root,
            json!({"op": "acknowledge", "name": "scope", "token": review["review_token"]}),
        );
        if [0, 7, 15, 31].contains(&index) {
            rolling.push(json!({"reviews": index + 1, "rss_bytes": rss(&owner)}));
        }
    }
    let rolling_peak = owner.stop();

    // Fixed total graph size, varying affected fan-in. Keeps repository size separate from impact size.
    let mut scaling = Vec::new();
    for affected in [1, 16, 128] {
        let workspace = root.join(format!("fan{affected}"));
        fs::create_dir(&workspace).unwrap();
        let mut content = String::from("function leaf() {}\nfunction other() {}\n");
        for index in 0..256 {
            let target = if index < affected { "leaf" } else { "other" };
            content.push_str(&format!("export function f{index}() {{ {target}(); }}\n"));
        }
        fs::write(workspace.join("api.ts"), &content).unwrap();
        let mut service = Owner::start(&workspace);
        ready(&workspace);
        request(&workspace, json!({"op": "checkpoint", "name": "before"}));
        fs::write(
            workspace.join("api.ts"),
            content.replace("leaf() {}", "leaf() { changed(); }"),
        )
        .unwrap();
        request(&workspace, json!({"op": "sync", "paths": ["api.ts"]}));
        let impact = json!({"op": "impact", "symbol": "leaf", "depth": 2, "limit": 256});
        let output = request(&workspace, impact.clone());
        assert_eq!(
            output["graph"]["symbols"].as_array().unwrap().len(),
            affected + 1
        );
        assert_eq!(output["truncated"], false);
        let diff = request(&workspace, json!({"op": "diff", "name": "before"}));
        scaling.push(json!({"affected_callers": affected, "total_symbols": 258,
            "impact_ms": measure(100, |_| { request(&workspace, impact.clone()); }),
            "diff_ms": measure(100, |_| { request(&workspace, json!({"op": "diff", "name": "before"})); }),
            "diff_bytes": diff["diff"].as_str().unwrap().len(), "diff_truncated": diff["truncated"]}));
        service.stop();
    }
    let diff_workloads = diff_costs(&root);
    let report = json!({"schema": 1, "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
        "binary_blake3": blake3::hash(&fs::read(binary()).unwrap()).to_hex().to_string(),
        "diff_workloads": diff_workloads,
        "rustc": String::from_utf8(Command::new("rustc").arg("--version").output().unwrap().stdout).unwrap().trim(),
        "workload": {"name": "StackScout src (including inline tests)", "files": files, "bytes": bytes,
            "blake3": hash.finalize().to_hex().to_string(), "threads": 4, "watching": false},
        "cold_index_ms": summary(cold), "cold_peak_rss_bytes": summary(peaks),
        "status_during_initial_index_ms": summary(indexing_reads), "warm_graph_ms": warm,
        "warm_query": {"request": graph, "symbols": evidence["graph"]["symbols"].as_array().unwrap().len(),
            "calls": evidence["graph"]["calls"].as_array().unwrap().len(), "truncated": evidence["truncated"],
            "analysis_issue_count": evidence["analysis_issue_count"]},
        "connection_status_ms": connection, "second_cli_client_ms": second_client,
        "one_file_sync_ms": summary(updates), "retention": retention,
        "lifetime_peak_rss_bytes": lifetime_peak, "rolling_reviews": rolling,
        "rolling_peak_rss_bytes": rolling_peak, "fan_in": scaling});
    fs::write(
        output,
        serde_json::to_string_pretty(&report).unwrap() + "\n",
    )
    .unwrap();
}
