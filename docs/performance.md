# Performance measurements

## Indexed resolution

Compared `21a45112` with `d6b978a6` (pass-local `BTreeMap` lookups). No parser,
resolution-policy, service, parallelism, or runtime-dependency changes in that comparison.
Apple M5 Max, Darwin arm64, Rust 1.95.0, Node 24.19.0; release builds, same lockfile.
[Raw samples](benchmarks/resolution.json).

### Method

- Generated directory pairs: 10 functions/file, direct-call chains, one public/exported root
  and unresolved leaf per file. Only the first leaf changes. Dense functions occupy 3 lines;
  padded functions add 27 declarations (1,000 functions / 30,000 lines).
- CLI: fresh processes, warm filesystem caches, one output-check warmup, five alternating
  samples. Includes both trees' discovery, parsing, resolution, diff/render, and startup.
  Output matched byte-for-byte on all eight cases.
- In-memory: one warmup/five samples per version. Resolve includes index construction,
  symbol cloning, and index destruction for one pre-extracted tree, excluding result destruction.
  Update+diff follows batched ingestion/checkpoint creation; reset is outside the timer.
- Background load was uncontrolled. These are not cold-disk, Pi, or general tool-comparison
  results. Small differences are noisy.

### CLI medians (ms)

| Language | Functions | Lines | Before | Indexed |
| --- | ---: | ---: | ---: | ---: |
| TypeScript | 100 | 300 | 4.83 | 5.10 |
| TypeScript | 1,000 | 3,000 | 21.39 | 18.15 |
| TypeScript | 10,000 | 30,000 | 508.96 | 163.99 |
| TypeScript | 1,000 | 30,000 | 177.62 | 174.15 |
| Rust | 100 | 300 | 5.23 | 3.81 |
| Rust | 1,000 | 3,000 | 138.45 | 17.84 |
| Rust | 10,000 | 30,000 | 12,010.46 | 162.66 |
| Rust | 1,000 | 30,000 | 253.67 | 133.87 |

Tiny TypeScript, repeated with 2 warmups/25 alternating samples: **3.81 → 3.79 ms**.
StackScout `src/` (parent versus updated sources), 2 warmups/7 alternating samples:
**71.97 → 52.32 ms**, identical 22,019-byte output, no analysis issues. One real repository.

### In-memory medians (ms)

| Operation | Language | Functions | Before | Indexed |
| --- | --- | ---: | ---: | ---: |
| Resolve dense artifacts | TypeScript | 1,000 | 1.615 | 0.452 |
| Resolve dense artifacts | TypeScript | 10,000 | 173.849 | 2.810 |
| Resolve dense artifacts | Rust | 1,000 | 61.840 | 0.518 |
| Resolve dense artifacts | Rust | 10,000 | 6,173.344 | 5.787 |
| One-file update + diff, padded | TypeScript | 1,000 | 3.735 | 2.158 |
| One-file update + diff, padded | Rust | 1,000 | 64.545 | 2.261 |

Single `/usr/bin/time -l` runs at 10,000 dense functions showed whole-process peak RSS
**68.5 → 72.0 MiB** (Rust), **69.1 → 70.4 MiB** (TypeScript), not retained-service memory.

### Reproduce

```sh
cargo test --release resolve::bench::scaling -- --ignored --nocapture
```

This regenerates the in-memory corpus and prints raw samples. Run the same benchmark module
against the baseline resolver for comparison; use release builds. Historical binaries/logs:
`/tmp/stackscout-resolution-bench`.
Those scratch artifacts are not durable. Lookup indexes are pass-local; import scans, global
edge rebuilds, and snapshot cloning remain costs. Reprofile before further optimization.

## Service smoke measurement

One macOS arm64 release run: 1,000 files, one empty Rust function/file, 4 indexing threads,
watching disabled. Sixteen distinct checkpoints, each followed by a one-file edit, raised
sampled owner RSS **13.8 → 38.4 MiB**. Mean explicit update round trip was **6.7 ms**, including
CLI startup. Forgetting checkpoints did not immediately reduce RSS (possible allocator retention).
This is not peak memory, p95 latency, a real-repository result, or evidence of Pi token savings.
