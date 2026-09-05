#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{
    Barrier,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

use serde_json::Value;

struct Workspace {
    _directory: tempfile::TempDir,
    root: PathBuf,
    child: Option<Child>,
    detached_pid: Option<u32>,
    watching: bool,
}

impl Workspace {
    fn new() -> Self {
        let directory = tempfile::Builder::new().prefix("ss-").tempdir().unwrap();
        Self {
            root: directory.path().canonicalize().unwrap(),
            _directory: directory,
            child: None,
            detached_pid: None,
            watching: false,
        }
    }

    fn start(&mut self) {
        self.child = Some(
            Command::new(env!("CARGO_BIN_EXE_stackscout"))
                .arg("serve")
                .arg(&self.root)
                .env("STACKSCOUT_WATCH", if self.watching { "1" } else { "0" })
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if stackscout::service::request(&self.root, r#"{"version":1,"op":"status"}"#).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "service did not start");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        } else if let Some(pid) = self.detached_pid {
            let _ = Command::new("kill")
                .arg("-TERM")
                .arg(pid.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
    }
}

fn request(root: &Path, json: &str) -> Value {
    serde_json::from_str(&stackscout::service::request(root, json).unwrap()).unwrap()
}

fn wait(root: &Path, predicate: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = request(root, r#"{"version":1,"op":"status"}"#);
        if predicate(&status) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "watcher did not converge: {status}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn watcher_reconciles_atomic_saves_directories_ignores_and_unchanged_hashes() {
    let mut workspace = Workspace::new();
    workspace.watching = true;
    let root = workspace.root.clone();
    fs::write(root.join("lib.rs"), "pub fn run() {}").unwrap();
    workspace.start();
    wait(&root, |status| {
        status["watching"] == true && status["index_complete"] == true
    });
    request(&root, r#"{"version":1,"op":"checkpoint","name":"before"}"#);
    fs::write(root.join(".save"), "pub fn run() { changed(); }").unwrap();
    fs::rename(root.join(".save"), root.join("lib.rs")).unwrap();
    wait(&root, |status| {
        status["generation"].as_u64().unwrap_or(0) >= 2
    });
    assert!(
        request(&root, r#"{"version":1,"op":"diff","name":"before"}"#)["diff"]
            .as_str()
            .unwrap()
            .contains("changed")
    );
    fs::create_dir(root.join("nested")).unwrap();
    fs::write(root.join("nested/worker.ts"), "export function work() {}").unwrap();
    wait(&root, |status| status["files"] == 2);
    fs::rename(root.join("nested"), root.join("renamed.rs")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let query = request(
            &root,
            r#"{"version":1,"op":"callees","symbol":"ts:renamed.rs/worker.ts::work"}"#,
        );
        if query["error"].is_null() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "directory rename did not converge: {query}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    fs::remove_dir_all(root.join("renamed.rs")).unwrap();
    wait(&root, |status| status["files"] == 1);
    fs::write(root.join(".ignore"), "lib.rs\n").unwrap();
    wait(&root, |status| status["files"] == 0);
    fs::write(root.join(".ignore"), "").unwrap();
    wait(&root, |status| {
        status["files"] == 1 && status["recovery_needed"] == false
    });
    let before = request(&root, r#"{"version":1,"op":"sync","paths":[]}"#);
    fs::write(root.join("lib.rs"), "pub fn run() { changed(); }").unwrap();
    std::thread::sleep(Duration::from_millis(600));
    let after = request(&root, r#"{"version":1,"op":"sync","paths":[]}"#);
    assert_eq!(after["generation"], before["generation"]);
    assert!(after["watcher_error"].is_null(), "{after}");
}

#[test]
fn graph_queries_report_locations_ambiguity_limits_and_generations() {
    let mut workspace = Workspace::new();
    fs::write(
        workspace.root.join("lib.rs"),
        "pub fn run() { work(); missing(); } fn work() {}",
    )
    .unwrap();
    fs::write(workspace.root.join("other.rs"), "fn work() {}").unwrap();
    workspace.start();
    request(&workspace.root, r#"{"version":1,"op":"sync","paths":[]}"#);
    let ambiguous = request(
        &workspace.root,
        r#"{"version":1,"op":"callees","symbol":"work"}"#,
    );
    assert!(ambiguous["error"].as_str().unwrap().contains("ambiguous"));
    let callers = request(
        &workspace.root,
        r#"{"version":1,"op":"callers","symbol":"rust:lib.rs::work"}"#,
    );
    assert!(callers["error"].is_null(), "{callers}");
    assert_eq!(callers["generation"], 1);
    assert_eq!(callers["graph"]["symbols"].as_array().unwrap().len(), 2);
    assert_eq!(callers["graph"]["calls"][0]["location"]["start_line"], 1);
    let callees = request(
        &workspace.root,
        r#"{"version":1,"op":"callees","symbol":"run"}"#,
    );
    assert!(
        callees["graph"]["calls"]
            .as_array()
            .unwrap()
            .iter()
            .any(|call| call["resolution"] == "unresolved")
    );
    let limited = request(
        &workspace.root,
        r#"{"version":1,"op":"impact","symbol":"rust:lib.rs::work","limit":1}"#,
    );
    assert_eq!(limited["truncated"], true);
    for json in [
        r#"{"version":1,"op":"callees","symbol":"run","limit":0}"#,
        r#"{"version":1,"op":"callees","symbol":"run","depth":33}"#,
        r#"{"version":1,"op":"callees","symbol":"run","extra":true}"#,
    ] {
        assert!(request(&workspace.root, json)["error"].is_string());
    }
}

#[test]
fn long_paths_share_ownership_and_recover_stale_fallback_sockets() {
    let mut workspace = Workspace::new();
    workspace.root = workspace.root.join("long-workspace-name-".repeat(8));
    fs::create_dir(&workspace.root).unwrap();
    fs::write(workspace.root.join("lib.rs"), "pub fn run() {}").unwrap();
    workspace.start();
    let first = request(&workspace.root, r#"{"version":1,"op":"sync","paths":[]}"#);
    assert_eq!(first["files"], 1);
    assert!(!workspace.root.join(".stackscout/service.sock").exists());
    let alias = workspace._directory.path().join("alias");
    std::os::unix::fs::symlink(&workspace.root, &alias).unwrap();
    assert_eq!(
        request(&alias, r#"{"version":1,"op":"status"}"#)["instance"],
        first["instance"]
    );
    let loser = Command::new(env!("CARGO_BIN_EXE_stackscout"))
        .arg("serve")
        .arg(&alias)
        .output()
        .unwrap();
    assert!(!loser.status.success());
    workspace.child.as_mut().unwrap().kill().unwrap();
    workspace.child.as_mut().unwrap().wait().unwrap();
    workspace.start();
    let restarted = request(&alias, r#"{"version":1,"op":"sync","paths":[]}"#);
    assert_ne!(restarted["instance"], first["instance"]);
    assert_eq!(restarted["files"], 1);
}

#[test]
fn slow_frames_and_waiting_updates_do_not_occupy_read_workers() {
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    let mut workspace = Workspace::new();
    workspace.start();
    request(&workspace.root, r#"{"version":1,"op":"sync","paths":[]}"#);
    let socket = workspace.root.join(".stackscout/service.sock");
    let mut clients = Vec::new();
    for _ in 0..16 {
        let mut stream = UnixStream::connect(&socket).unwrap();
        stream.write_all(b"{\"version\":").unwrap();
        clients.push(stream);
    }
    for index in 0..300 {
        fs::write(
            workspace.root.join(format!("f{index}.rs")),
            "pub fn added() {}",
        )
        .unwrap();
    }
    for _ in 0..8 {
        let mut stream = UnixStream::connect(&socket).unwrap();
        stream
            .write_all(b"{\"version\":1,\"op\":\"rescan\"}\n")
            .unwrap();
        clients.push(stream);
    }
    let start = Instant::now();
    let status = request(&workspace.root, r#"{"version":1,"op":"status"}"#);
    assert!(status["error"].is_null(), "{status}");
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "read waited for slow-frame deadline"
    );
}

#[test]
fn oversized_sources_preserve_the_previous_generation() {
    let mut workspace = Workspace::new();
    fs::write(workspace.root.join("lib.rs"), "pub fn run() {}").unwrap();
    workspace.start();
    request(&workspace.root, r#"{"version":1,"op":"sync","paths":[]}"#);
    fs::write(workspace.root.join("lib.rs"), "pub fn changed() {}").unwrap();
    fs::File::create(workspace.root.join("huge.rs"))
        .unwrap()
        .set_len(8 * 1024 * 1024 + 1)
        .unwrap();
    let response = request(&workspace.root, r#"{"version":1,"op":"rescan"}"#);
    assert_eq!(response["generation"], 1);
    assert!(
        response["error"]
            .as_str()
            .unwrap()
            .contains("exceeds 8 MiB")
    );
    assert!(
        request(
            &workspace.root,
            r#"{"version":1,"op":"callees","symbol":"run"}"#
        )["error"]
            .is_null()
    );
}

#[test]
fn explicit_updates_publish_once_and_checkpoints_survive_reconnects() {
    let mut workspace = Workspace::new();
    let root = workspace.root.clone();
    fs::write(root.join("lib.rs"), "pub fn run() {} fn audit() {}").unwrap();
    fs::write(root.join(".ignore"), "ignored.rs\n").unwrap();
    fs::write(root.join("ignored.rs"), "broken source {").unwrap();
    workspace.start();
    let initial = request(&root, r#"{"version":1,"op":"sync","paths":[]}"#);
    assert_eq!(initial["generation"], 1);
    assert_eq!(initial["files"], 1);
    assert_eq!(initial["index_complete"], true);
    assert_eq!(initial["watching"], false);
    assert_eq!(
        request(&root, r#"{"version":1,"op":"checkpoint","name":"session"}"#)["checkpoint_generation"],
        1
    );
    fs::write(
        root.join("lib.rs"),
        "pub fn run() { audit(); } fn audit() {}",
    )
    .unwrap();
    let updated = request(
        &root,
        r#"{"version":1,"op":"sync","paths":["lib.rs","lib.rs","ignored.rs"]}"#,
    );
    assert_eq!(updated["generation"], 2);
    assert!(updated["error"].is_null(), "{updated}");
    assert_eq!(updated["files"], 1);
    assert_eq!(
        request(&root, r#"{"version":1,"op":"checkpoint","name":"session"}"#)["checkpoint_generation"],
        1
    );
    let diff = request(&root, r#"{"version":1,"op":"diff","name":"session"}"#);
    assert_eq!(diff["generation"], 2);
    assert_eq!(diff["checkpoint_generation"], 1);
    assert!(diff["diff"].as_str().unwrap().contains("audit"));
    assert_eq!(
        request(&root, r#"{"version":1,"op":"rescan"}"#)["generation"],
        2
    );
    fs::rename(root.join("lib.rs"), root.join("renamed.rs")).unwrap();
    let renamed = request(
        &root,
        r#"{"version":1,"op":"sync","paths":["lib.rs","renamed.rs"]}"#,
    );
    assert_eq!(renamed["generation"], 3);
    assert_eq!(renamed["files"], 1);
    fs::write(root.join(".ignore"), "ignored.rs\nrenamed.rs\n").unwrap();
    let ignored = request(&root, r#"{"version":1,"op":"sync","paths":["renamed.rs"]}"#);
    assert_eq!(ignored["generation"], 4);
    assert_eq!(ignored["files"], 0);
    assert_eq!(
        request(&root, r#"{"version":1,"op":"rescan"}"#)["generation"],
        4
    );
    request(&root, r#"{"version":1,"op":"forget","name":"session"}"#);
    assert!(
        request(&root, r#"{"version":1,"op":"diff","name":"session"}"#)["error"]
            .as_str()
            .unwrap()
            .contains("unknown checkpoint")
    );
}

#[test]
fn startup_racers_share_one_owner_and_crash_recovery_does_not_restore_checkpoints() {
    let mut workspace = Workspace::new();
    fs::write(workspace.root.join("lib.rs"), "pub fn run() {}").unwrap();
    std::os::unix::fs::symlink(&workspace.root, workspace.root.join("alias")).unwrap();
    let mut clients = Vec::new();
    for root in [&workspace.root, &workspace.root.join("alias")] {
        clients.push(
            Command::new(env!("CARGO_BIN_EXE_stackscout"))
                .arg("request")
                .env("STACKSCOUT_WATCH", "0")
                .arg(root)
                .arg(r#"{"version":1,"op":"sync","paths":[]}"#)
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
    }
    let responses = clients
        .into_iter()
        .map(|client| {
            let output = client.wait_with_output().unwrap();
            assert!(output.status.success());
            serde_json::from_slice::<Value>(&output.stdout).unwrap()
        })
        .collect::<Vec<_>>();
    let pid = responses[0]["instance"]
        .as_str()
        .unwrap()
        .split('-')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    workspace.detached_pid = Some(pid);
    assert_eq!(responses[0]["instance"], responses[1]["instance"]);
    assert_eq!(responses[0]["root"], responses[1]["root"]);
    assert_eq!(responses[0]["generation"], 1);
    assert_eq!(responses[1]["generation"], 1);
    request(
        &workspace.root,
        r#"{"version":1,"op":"checkpoint","name":"session"}"#,
    );
    let loser = Command::new(env!("CARGO_BIN_EXE_stackscout"))
        .arg("serve")
        .arg(&workspace.root)
        .output()
        .unwrap();
    assert!(!loser.status.success());
    assert!(
        String::from_utf8(loser.stderr)
            .unwrap()
            .contains("already owned")
    );
    assert_eq!(
        request(&workspace.root, r#"{"version":1,"op":"status"}"#)["instance"],
        responses[0]["instance"]
    );
    Command::new("kill")
        .arg("-KILL")
        .arg(pid.to_string())
        .status()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while stackscout::service::request(&workspace.root, r#"{"version":1,"op":"status"}"#).is_ok() {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    workspace.detached_pid = None;
    workspace.start();
    let restarted = request(&workspace.root, r#"{"version":1,"op":"sync","paths":[]}"#);
    assert_ne!(restarted["instance"], responses[0]["instance"]);
    assert_eq!(restarted["generation"], 1);
    assert!(
        request(
            &workspace.root,
            r#"{"version":1,"op":"diff","name":"session"}"#
        )["error"]
            .is_string()
    );
}

#[test]
fn failed_scans_and_batches_never_publish_partial_state() {
    let mut workspace = Workspace::new();
    let root = workspace.root.clone();
    fs::write(root.join("a.rs"), "pub fn a() {}").unwrap();
    fs::write(root.join("z.rs"), [0xff]).unwrap();
    workspace.start();
    let failed = request(&root, r#"{"version":1,"op":"sync","paths":["a.rs"]}"#);
    assert_eq!(failed["index_complete"], false);
    assert!(failed["generation"].is_null());
    assert!(failed["error"].is_string());
    fs::write(root.join("z.rs"), "pub fn z() {}").unwrap();
    assert_eq!(
        request(&root, r#"{"version":1,"op":"rescan"}"#)["generation"],
        1
    );
    request(&root, r#"{"version":1,"op":"checkpoint","name":"before"}"#);
    fs::write(root.join("a.rs"), "pub fn a() { changed(); }").unwrap();
    fs::write(root.join("z.rs"), [0xff]).unwrap();
    let failed = request(
        &root,
        r#"{"version":1,"op":"sync","paths":["a.rs","z.rs"]}"#,
    );
    assert_eq!(failed["generation"], 1);
    assert!(failed["last_update_error"].is_string());
    let barrier = request(&root, r#"{"version":1,"op":"sync","paths":[]}"#);
    assert!(barrier["error"].is_null());
    assert!(barrier["last_update_error"].is_string());
    assert_eq!(
        request(&root, r#"{"version":1,"op":"diff","name":"before"}"#)["diff"],
        ""
    );
    fs::write(root.join("z.rs"), "pub fn broken() {").unwrap();
    let recovered = request(&root, r#"{"version":1,"op":"rescan"}"#);
    assert_eq!(recovered["generation"], 2);
    assert!(recovered["last_update_error"].is_null());
    assert_eq!(recovered["analysis_issue_count"], 1);
    assert!(
        request(&root, r#"{"version":1,"op":"diff","name":"before"}"#)["diff"]
            .as_str()
            .unwrap()
            .contains("incomplete")
    );
    let invalid = request(
        &root,
        r#"{"version":1,"op":"sync","paths":["../outside.rs"]}"#,
    );
    assert_eq!(invalid["generation"], 2);
    assert!(invalid["error"].is_string());
}

#[test]
fn two_readers_observe_only_complete_generations_during_rescan() {
    let mut workspace = Workspace::new();
    let root = workspace.root.clone();
    fs::write(root.join("lib.rs"), "pub fn run() {}").unwrap();
    workspace.start();
    request(&root, r#"{"version":1,"op":"sync","paths":[]}"#);
    request(&root, r#"{"version":1,"op":"checkpoint","name":"before"}"#);
    for index in 0..300 {
        fs::write(root.join(format!("f{index}.rs")), "pub fn added() {}").unwrap();
    }
    let start = Barrier::new(3);
    let done = AtomicBool::new(false);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            start.wait();
            let result = request(&root, r#"{"version":1,"op":"rescan"}"#);
            assert_eq!(result["generation"], 2);
            assert_eq!(result["files"], 301);
            done.store(true, Ordering::Release);
        });
        for op in [
            r#"{"version":1,"op":"status"}"#,
            r#"{"version":1,"op":"diff","name":"before"}"#,
        ] {
            let start = &start;
            let done = &done;
            let root = &root;
            scope.spawn(move || {
                start.wait();
                let deadline = Instant::now() + Duration::from_secs(10);
                loop {
                    assert!(Instant::now() < deadline, "update did not complete");
                    let result = request(root, op);
                    assert!(result["error"].is_null(), "{result}");
                    match result["generation"].as_u64().unwrap() {
                        1 => {
                            assert_eq!(result["files"], 1);
                            if result["diff"].is_string() {
                                assert_eq!(result["diff"], "");
                            }
                        }
                        2 => assert_eq!(result["files"], 301),
                        other => panic!("unexpected generation {other}"),
                    }
                    if done.load(Ordering::Acquire) {
                        break;
                    }
                }
            });
        }
    });
}

#[test]
fn targeted_sync_does_not_read_unrelated_sources_or_follow_symlinks() {
    let mut workspace = Workspace::new();
    let root = workspace.root.clone();
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn run() {}").unwrap();
    fs::write(root.join("other.rs"), "pub fn other() {}").unwrap();
    workspace.start();
    request(&root, r#"{"version":1,"op":"sync","paths":[]}"#);
    fs::write(root.join("other.rs"), [0xff]).unwrap();
    fs::write(root.join("src/lib.rs"), "pub fn run() { added(); }").unwrap();
    std::os::unix::fs::symlink(root.join("other.rs"), root.join("link.rs")).unwrap();
    let response = request(
        &root,
        r#"{"version":1,"op":"sync","paths":["src/lib.rs","link.rs"]}"#,
    );
    assert!(response["error"].is_null(), "{response}");
    assert_eq!(response["generation"], 2);
    assert_eq!(response["files"], 2);
    assert!(request(&root, r#"{"version":1,"op":"rescan"}"#)["error"].is_string());
}

#[test]
fn service_ownership_does_not_remove_non_socket_files() {
    let workspace = Workspace::new();
    fs::create_dir(workspace.root.join(".stackscout")).unwrap();
    let socket = workspace.root.join(".stackscout/service.sock");
    fs::write(&socket, "keep me").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_stackscout"))
        .arg("serve")
        .arg(&workspace.root)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(&socket).unwrap(), "keep me");
    assert!(
        stackscout::service::request(&workspace.root, r#"{"version":1,"op":"status"}"#).is_err()
    );
}

#[test]
fn protocol_errors_and_output_limits_are_explicit() {
    let mut workspace = Workspace::new();
    workspace.start();
    request(&workspace.root, r#"{"version":1,"op":"sync","paths":[]}"#);
    for json in [
        "not json",
        r#"{"version":2,"op":"status"}"#,
        r#"{"version":1,"op":"unknown"}"#,
        r#"{"version":1,"op":"status","surprise":true}"#,
    ] {
        assert!(request(&workspace.root, json)["error"].is_string());
    }
    request(
        &workspace.root,
        r#"{"version":1,"op":"checkpoint","name":"before"}"#,
    );
    let calls = (0..2000)
        .map(|i| format!("missing{i}();\n"))
        .collect::<String>();
    fs::write(
        workspace.root.join("lib.rs"),
        format!("pub fn run() {{ {calls} }}"),
    )
    .unwrap();
    request(
        &workspace.root,
        r#"{"version":1,"op":"sync","paths":["lib.rs"]}"#,
    );
    let diff = request(
        &workspace.root,
        r#"{"version":1,"op":"diff","name":"before"}"#,
    );
    assert_eq!(diff["truncated"], true);
    assert!(
        diff["diff"]
            .as_str()
            .unwrap()
            .ends_with("[truncated: diff exceeds 48 KiB]\n")
    );
    let chain = (0..40)
        .map(|i| format!("fn f{i}() {{ f{}(); }}\n", i + 1))
        .collect::<String>();
    fs::write(
        workspace.root.join("lib.rs"),
        format!("pub fn run() {{ f0(); }} {chain}"),
    )
    .unwrap();
    request(
        &workspace.root,
        r#"{"version":1,"op":"sync","paths":["lib.rs"]}"#,
    );
    let diff = request(
        &workspace.root,
        r#"{"version":1,"op":"diff","name":"before"}"#,
    );
    assert_eq!(diff["truncated"], true);
    assert!(diff["diff"].as_str().unwrap().contains("[truncated]"));
    assert!(!diff["diff"].as_str().unwrap().contains("exceeds 48 KiB"));
}

#[test]
fn frame_and_checkpoint_limits_are_enforced() {
    use std::io::{BufRead, BufReader, Write};
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;

    let mut workspace = Workspace::new();
    workspace.start();
    request(&workspace.root, r#"{"version":1,"op":"sync","paths":[]}"#);
    for bytes in [
        b"{\"version\":1,\"op\":\"status\"}".to_vec(),
        vec![b' '; 64 * 1024 + 1],
    ] {
        let mut stream =
            UnixStream::connect(workspace.root.join(".stackscout/service.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(&bytes).unwrap();
        stream.shutdown(Shutdown::Write).unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).unwrap();
        assert!(serde_json::from_str::<Value>(&reply).unwrap()["error"].is_string());
    }
    for index in 0..64 {
        let response = request(
            &workspace.root,
            &format!(r#"{{"version":1,"op":"checkpoint","name":"cp{index}"}}"#),
        );
        assert!(response["error"].is_null(), "{response}");
    }
    assert!(
        request(
            &workspace.root,
            r#"{"version":1,"op":"checkpoint","name":"overflow"}"#
        )["error"]
            .is_string()
    );
    assert_eq!(
        request(
            &workspace.root,
            r#"{"version":1,"op":"checkpoint","name":"cp0"}"#
        )["checkpoint_generation"],
        1
    );
    request(
        &workspace.root,
        r#"{"version":1,"op":"forget","name":"cp0"}"#,
    );
    assert!(
        request(
            &workspace.root,
            r#"{"version":1,"op":"checkpoint","name":"overflow"}"#
        )["error"]
            .is_null()
    );
}
