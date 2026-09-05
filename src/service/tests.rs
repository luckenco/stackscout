use super::*;

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
