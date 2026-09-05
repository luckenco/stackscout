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

## Service evaluation

2026-09-05, Rust 1.95 release, four index threads, watching disabled for timings. Small real
Rust workload: StackScout `src/` at `8fe00554`, 24 files / 214,397 bytes including inline tests.
[macOS samples](benchmarks/service-macos.json) and [Linux samples](benchmarks/service-linux.json)
include a source hash, query shape, counts, and every timing. Host: Apple M5 Max, macOS 26.6.2;
Linux arm64: `rust:1.95` under OrbStack 7.0.14, 18 vCPUs / ~16 GiB. Background load was uncontrolled.

```sh
STACKSCOUT_MEASURE_OUT=/tmp/service.json cargo test --release --test performance -- --ignored --nocapture
cargo test --test service periodic_reconciliation -- --ignored --nocapture
```

Cold means a fresh owner/cache, **not cold disk**: source copy precedes timing. Ten cold samples
include process launch and readiness polling; `wait4` records per-owner kernel peak RSS. Warm
queries use six untimed queries/100 samples, each with a fresh socket and full JSON response. The depth-2
`diff_dirs` query returns seven symbols/17 calls and explicitly truncates deeper paths. Updates
append a small synthetic callable to `src/lib.rs`, exclude file-write time, include sync round trip,
and assert one new generation/parse per edit.
Second-client samples (30) include CLI startup and assert no extra parsing. p95 is nearest-rank;
with ten cold samples it is the maximum, not a stable tail estimate.

| Metric | macOS | Linux |
| --- | ---: | ---: |
| Cold index p50 / p95, ms | 15.74 / 351.60 | 14.57 / 21.72 |
| Cold kernel peak RSS, maximum MiB | 19.14 | 14.24 |
| Warm graph p95, ms | 2.56 | 3.46 |
| Connect + status p95, ms | 2.54 | 3.31 |
| One-file sync p95, ms | 5.12 | 8.90 |
| Second CLI client p95, ms | 7.44 | 3.40 |
| Status during initial indexing p95, ms | 2.31 | 3.14 |
| RSS with 32 distinct checkpoint generations, MiB | 118.00 | 117.52 |
| RSS after 32 rolling review/ack cycles, MiB | 26.48 | 23.46 |

Both warm targets (<10 ms queries, <50 ms updates) pass **on this small workload**. A fixed
258-symbol TS graph with 1/16/128 affected callers had impact p95 2.55–2.57 ms on macOS,
3.45–6.01 ms on Linux; diff p95 2.57–2.60 / 3.66–6.71 ms. None of those diffs truncated.
These round trips do not isolate traversal cost or establish large-repository scaling.

Named pins raised RSS roughly 3 MiB per distinct generation; forgetting did not immediately
return memory to the OS. Rolling reviews stayed far smaller. Sampled `ps` RSS and kernel
high-water counters are separate accounting measures, not exact retained-heap sizes.
A first macOS launch dominated cold p95 (also observed at 379 ms in the earlier probe); cause
is unassigned. Next-run cold budgets for this corpus: p95 <100 ms and kernel peak RSS <32 MiB.
The latency budget is **not met on macOS**; investigate launch cost before adding a parse cache.

Linux/macOS checks exercise startup races/crash recovery, unrelated-source read avoidance,
failed-batch rollback, and two readers observing complete generations. The new parent-ignore
probe catches unwatched discovery drift after 30.08 / 30.10 seconds. Callback overflow and loss
during recovery are deterministically injected in the existing watcher unit test; kernel-buffer
overflow is not forced. Timing/retention runners are opt-in, not flaky CI timing assertions.

## Workflow evaluation

[Method and reproduction](evaluation.md). macOS, Pi 0.85.1, Node 24.19.0,
`openai-codex/gpt-6-astra`, low thinking; real model responses in every agent.
[Initial batch](benchmarks/workflow-initial.json): 12 attempts, 11 completed, one provider
WebSocket failure retained (resume ran only unattempted trials). All completed patches passed
seven external cases and source/patch review, with zero false/missed alerts or unexpected edits.
The five complete pairs used **67–120% more total model tokens with graph tools**. The complete
relevant pair reported detection after 14.25 seconds (control) / 15.75 seconds (graph).

Traces showed unconditional adapter guidance causing extra session diffs, ceremonial acknowledgements,
and repeated reads. Changed guidance to choose a concrete scoped/session-wide question and acknowledge
only for a later review. [Follow-up](benchmarks/workflow-guided.json): three pairs, control first,
all six completed with the same correctness/alert results. Counts below sum reviewer + writer tokens;
reads count the reviewer's post-preparation `read` calls, not Bash inspection.

| Scenario | Model tokens, control → graph | Follow-up reads, control → graph |
| --- | ---: | ---: |
| Single agent | 21,026 → 31,041 | 3 → 1 |
| Relevant + unrelated writer edits | 21,844 → 39,198 | 3 → 3 |
| Unrelated-only writer edit | 27,326 → 37,728 | 5 → 2 |

The revised workflow used 1–2 reviewer graph calls and zero writer graph calls (initially 3–5
and one, respectively), but still **38–79% more tokens**. Relevant-change detection was
10.45 / 15.55 seconds; wall time improved in only one of three pairs. Shared second clients
kept `files_parsed` at 11, establishing reuse, not token savings.

**Usefulness gate remains unproven.** Fewer repeated reads are promising, but there is no token
or detection benefit on this small explicit-import task. This synthetic pilot and small self-hosted
service corpus do not establish representative large Rust/TS workflow performance. Evaluate selective
tool exposure/context cost next; require stronger matched tasks before expanding product scope.

[Compressed traces](benchmarks/workflow-events.jsonl.gz) retain 591 tool/text/usage events across
cohorts; SDK thinking/signatures and auth journals are excluded. Summaries retain every final patch
and settings; the initial adapter is `8fe00554`, while the follow-up records its adapter hash.
A separate [preflight](benchmarks/workflow-preflight.json) retains two earlier attempts: one complete
control, then a runner startup-pipe timeout. Fixed stderr routing before the primary batch; this
incomplete preflight pair is excluded from paired deltas, not hidden as a successful trial.
