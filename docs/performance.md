# Performance and usefulness

Measurements are workload-specific, not compiler-completeness or token-savings guarantees.

## Indexed resolution

Compared `21a45112` with `d6b978a6`: pass-local lookup indexes only, identical output on all
cases. Apple M5 Max/macOS arm64, Rust 1.95.0, Node 24.19.0, release builds/same lockfile.
[Raw samples](benchmarks/resolution.json) retain the full corpus and timings.

Generated Rust/TypeScript chains: 10 functions/file, 100–10,000 functions, one changed leaf.
Dense functions occupy three lines; padded ones add 27 declarations. Warm filesystem caches,
one warmup/five alternating samples, uncontrolled background load. At 10,000 dense functions:

| Median ms | Rust before → indexed | TypeScript before → indexed |
| --- | --- | --- |
| CLI directory comparison | 12,010.46 → 162.66 | 508.96 → 163.99 |
| In-memory resolution | 6,173.344 → 5.787 | 173.849 → 2.810 |
| Update+diff, 1,000 padded functions | 64.545 → 2.261 | 3.735 → 2.158 |

Real StackScout `src/` comparison: 71.97 → 52.32 ms (two warmups/seven alternating samples),
identical 22,019-byte output. Tiny TS repeat: 3.81 → 3.79 ms (25 samples); small differences
are noisy. Single CLI peak-RSS samples at 10,000 functions: Rust 68.5 → 72.0 MiB,
TS 69.1 → 70.4 MiB, not retained-service memory.

```sh
cargo test --release resolve::bench::scaling -- --ignored --nocapture
```

This regenerates the in-memory corpus; run the same module on the baseline for comparison.
CLI scratch runners were temporary. No cold-disk/Pi/workflow claim; import scans, global edge
rebuilds, and snapshot cloning remain costs.

## Service smoke measurement

One macOS arm64 release run, 1,000 files/empty Rust functions, four indexing threads, watching
disabled. Sixteen checkpoints with successive edits raised sampled owner RSS 13.8 → 38.4 MiB.
Mean explicit-update round trip: 6.7 ms including CLI startup. Forget did not immediately reduce
RSS (possibly allocator retention). Not peak memory, p95, or a representative-repository result.
