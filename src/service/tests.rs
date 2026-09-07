use super::*;

#[test]
fn diff_cache_bounds_payloads_without_pinning_graphs_or_caching_health() {
    let directory = tempfile::tempdir().unwrap();
    let shared = Shared {
        root: directory.path().to_owned(),
        instance: "test".to_owned(),
        state: Mutex::new(State::default()),
        watch: Arc::new(Mutex::new(watch::Pending::default())),
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build()
        .unwrap();
    let mut repo = Repository::new().unwrap();
    fs::write(shared.root.join("bad.ts"), "export function bad(").unwrap();
    let compare = |diagnostics| {
        query(
            &shared,
            Operation::Diff {
                name: "before".to_owned(),
                diagnostics,
            },
        )
    };
    let mut old_graph = None;
    for index in 0..=6 {
        fs::write(
            shared.root.join("api.rs"),
            format!("pub fn run() {{ call{index}(); }}"),
        )
        .unwrap();
        assert!(refresh(&shared, &mut repo, &pool, None).error.is_none());
        if index == 0 {
            query(
                &shared,
                Operation::Checkpoint {
                    name: "before".to_owned(),
                },
            );
            continue;
        }
        let output = compare(false);
        assert!(output.error.is_none());
        let text = output.diff.as_ref().unwrap();
        assert!(text.contains(&format!("call{index}")));
        if index == 1 {
            old_graph = Some(Arc::downgrade(&repo.snapshot().graph));
            fs::write(shared.root.join("api.rs"), [0xff]).unwrap();
            let failed = refresh(&shared, &mut repo, &pool, None);
            assert!(failed.last_update_error.is_some());
            let cached = compare(false);
            assert_eq!(cached.diff, output.diff);
            assert_eq!(cached.last_update_error, failed.last_update_error);
            assert_eq!(shared.state.lock().unwrap().diffs.len(), 1);
            let diagnostic = compare(true);
            assert_ne!(diagnostic.diff, output.diff);
            assert!(diagnostic.diff.unwrap().contains("bad.ts"));
            assert_eq!(shared.state.lock().unwrap().diffs.len(), 2);
        }
    }
    let state = shared.state.lock().unwrap();
    assert_eq!(state.diffs.len(), 4);
    assert_eq!(state.diffs.first().unwrap().key, (1, 4, false));
    assert!(
        state
            .diffs
            .iter()
            .all(|entry| entry.text.len() <= protocol::MAX_DIFF + 64)
    );
    drop(state);
    assert!(old_graph.unwrap().upgrade().is_none());
    query(
        &shared,
        Operation::Forget {
            name: "before".to_owned(),
        },
    );
    assert!(compare(false).error.is_some());
    query(
        &shared,
        Operation::Checkpoint {
            name: "same".to_owned(),
        },
    );
    assert_eq!(
        query(
            &shared,
            Operation::Diff {
                name: "same".to_owned(),
                diagnostics: false
            }
        )
        .diff
        .as_deref(),
        Some("")
    );
    assert_eq!(shared.state.lock().unwrap().diffs.len(), 4);
}

#[test]
fn parallel_scans_match_every_fixture_in_both_directions() {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test/fixtures");
    for threads in [1, 4] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        for entry in fs::read_dir(&fixtures).unwrap() {
            let fixture = entry.unwrap().path();
            for (before, after) in [("before", "after"), ("after", "before")] {
                let mut shared = Shared {
                    root: fixture.join(before),
                    instance: "test".to_owned(),
                    state: Mutex::new(State::default()),
                    watch: Arc::new(Mutex::new(watch::Pending::default())),
                };
                let mut repo = Repository::new().unwrap();
                let initial = refresh(&shared, &mut repo, &pool, None);
                assert!(initial.error.is_none(), "{:?}", initial.error);
                let baseline = repo.snapshot();
                shared.root = fixture.join(after);
                let updated = refresh(&shared, &mut repo, &pool, None);
                assert!(updated.error.is_none(), "{:?}", updated.error);
                assert_eq!(
                    baseline.compare(&repo.snapshot(), true, false).unwrap(),
                    crate::diff_dirs_with_diagnostics(fixture.join(before), fixture.join(after))
                        .unwrap(),
                    "{} with {threads} threads",
                    fixture.display()
                );
                let unchanged = refresh(&shared, &mut repo, &pool, None);
                assert_eq!(unchanged.generation, updated.generation);
                assert!(unchanged.error.is_none());
            }
        }
    }
}
