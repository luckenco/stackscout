# Indexed resolution measurements

Baseline: `21a45112` (repository service already present), compared with the pass-local
`BTreeMap` indexes added in this change. No runtime dependencies, parallelism, parser,
resolution-policy, or service changes. Measurements on an Apple M5 Max, Darwin arm64,
Rust 1.95.0, Node 24.19.0, release builds with the same lockfile.

## Method

- Generated directory pairs: ten functions per
  file, a ten-function direct-call chain, one public/exported root per file, and one
  unresolved leaf. Only the first file's leaf changes from `beforeCall` to `afterCall`.
- Dense cases have three lines per function. Padded cases add 27 local declarations per
  function: 100 files, 1,000 functions, 30,000 lines.
- CLI: fresh processes, warm filesystem caches, one output-check warmup per binary,
  then five samples alternating binary order. Output was byte-identical on all eight
  cases. Timings include discovery, reading, parsing both trees, resolution, diff,
  rendering, and process startup. They are not cold-disk measurements.
- In-memory: one warmup and five samples, before and after measured separately.
  Resolution includes index construction, symbol cloning, and index destruction over
  already-extracted artifacts for **one** directory; returned-program destruction is
  outside the timer. Update + diff is timed together after batched initial ingestion
  and checkpoint creation, with each reset outside the timer.
- Background load was not controlled. Small absolute differences are noisy. These
  synthetic workloads do not establish a general speed advantage over other tools.

[Raw samples](benchmarks/resolution.json). This run's binaries, CLI runner, and logs
are in `/tmp/stackscout-resolution-bench`. Those local scratch paths are not durable
repository artifacts. The checked-in benchmark below regenerates the in-memory corpus.

## Full CLI medians (milliseconds)

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

The noisy tiny-TypeScript result was repeated with two warmups and 25 alternating
samples: **3.81 ms before, 3.79 ms indexed**. On StackScout's own `src` directory
(parent sources versus updated sources), seven alternating samples after two warmups
measured **71.97 ms before, 52.32 ms indexed**, with identical 22,019-byte output and
no incomplete-analysis diagnostics. This is one small real repository, not a broad corpus.

## In-memory medians (milliseconds)

| Operation | Language | Functions | Before | Indexed |
| --- | --- | ---: | ---: | ---: |
| Resolve dense artifacts | TypeScript | 1,000 | 1.615 | 0.452 |
| Resolve dense artifacts | TypeScript | 10,000 | 173.849 | 2.810 |
| Resolve dense artifacts | Rust | 1,000 | 61.840 | 0.518 |
| Resolve dense artifacts | Rust | 10,000 | 6,173.344 | 5.787 |
| One-file update + diff, padded | TypeScript | 1,000 | 3.735 | 2.158 |
| One-file update + diff, padded | Rust | 1,000 | 64.545 | 2.261 |

Single `/usr/bin/time -l` observations on the dense 10,000-function CLI case showed
peak RSS rising from 68.5 to 72.0 MiB for Rust and 69.1 to 70.4 MiB for TypeScript.
These are whole-process peaks, not allocation counts or retained-service memory.

## Repeatable resolver benchmark

```sh
cargo test --release resolve::bench::scaling -- --ignored --nocapture
```

The ignored benchmark uses only existing dependencies, prints raw millisecond samples,
and runs separately from ordinary tests. To reproduce the baseline, run the same
`src/resolve/bench.rs` module against the baseline resolver. Do not compare debug builds.

The improvement removes full-symbol scans from name lookup. Imports and export bindings
still have per-call scans, updates still rebuild global resolution and graph edges, and
checkpoints still clone snapshots. Indexes exist only during resolution; this change
adds no persistent cache or invalidation scheme. Reprofile before adding parallelism,
other collections, or more indexes.
