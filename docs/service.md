# Repository service: Milestone 3

Experimental protocol v1 for Linux and macOS. One resident index, filesystem reconciliation,
bounded parallel indexing, immutable generations, and graph queries. Windows is out of scope.
The bundled Pi extension still uses directory diffs; service-backed Pi integration is Milestone 4.

## Run

```sh
cargo build
# Foreground owner; stop with Ctrl-C. Usually request starts it instead.
target/debug/stackscout serve /path/to/workspace

# Connect or start an owner. Concurrent startup losers reconnect without indexing.
target/debug/stackscout request . '{"version":1,"op":"status"}'
target/debug/stackscout request . '{"version":1,"op":"sync","paths":[]}'
target/debug/stackscout request . '{"version":1,"op":"checkpoint","name":"session-1"}'
# Acknowledge these writes without waiting for the debouncer:
target/debug/stackscout request . '{"version":1,"op":"sync","paths":["src/lib.rs"]}'
target/debug/stackscout request . '{"version":1,"op":"diff","name":"session-1"}'
target/debug/stackscout request . '{"version":1,"op":"callers","symbol":"rust:src/lib.rs::diff_dirs"}'
target/debug/stackscout request . '{"version":1,"op":"impact","symbol":"rust:src/lib.rs::diff_dirs","depth":8,"limit":64}'
```

`request` prints JSON. Operation failures appear in `error`; check that field even when the CLI
exits successfully. Transport/startup failures exit unsuccessfully. The Rust `service::request`
API connects only; subprocess startup belongs to the CLI.

Owner configuration (set before starting the owner; reconnecting does not reconfigure it):

- `STACKSCOUT_INDEX_THREADS=1..8`: private Rayon pool size; default available CPUs, capped at 4.
- `STACKSCOUT_WATCH=0`: disable watching and periodic reconciliation for explicit-update workflows.

## Ownership and transport

The canonical root owns `.stackscout/owner.lock`. The OS lock is held for the owner's lifetime,
including when using a fallback socket. Never unlink it while an owner may be running. Startup
losers return before scanning or starting a watcher. A restart removes a stale socket only under
the lock and refuses to replace non-socket or unowned endpoints.

Normally the endpoint is `.stackscout/service.sock`. If its path is 100 bytes or longer, use
`/tmp/stackscout-<effective-uid>/<blake3-of-canonical-root>.sock` instead. Both clients and owners
choose identically, including through root aliases. Service directories must be real, owned,
mode-0700 directories; locks and sockets must be owned and mode 0600. Lock opens reject symlinks.
Existing unsafe permissions are errors, not silently repaired. This assumes a trusted,
user-controlled workspace, not an adversarial shared filesystem. Fallback sockets may remain
after termination and are recovered on the next start for that root.

Send one UTF-8 JSON request followed by a newline per Unix socket connection. One JSON response
followed by a newline closes it. Requests require `version: 1` and reject unknown operations and
fields. Requests are limited to 64 KiB; incomplete frames have a five-second absolute deadline.
Replies have a five-second write deadline; the bundled client waits up to 120 seconds for a reply
and rejects incomplete or oversized (over 1 MiB) responses.

The client never replays a mutation after a transport failure: a disconnected mutation may still
complete. Reconnect and inspect **instance and generation** rather than assuming cancellation.
The owner survives client disconnects. There is no shutdown operation or idle expiry; terminate
the foreground owner or the PID at the start of `instance`. Restarts change instance identity and
lose in-memory checkpoints. Missing checkpoints are errors, never silently recreated baselines.

## Requests

All requests include `"version":1`:

| `op` | Other fields | Meaning |
| --- | --- | --- |
| `status` | none | Read readiness, generation, progress, watcher health, and diagnostics. |
| `sync` | `paths: string[]` | Re-read only named source files; missing/ignored files become deletions. Empty paths wait for the initial scan and previously queued explicit updates; they do not force watcher delivery or a scan. |
| `rescan` | none | Reconcile the worktree, including deletions and ignore changes; reuse unchanged analysis. |
| `checkpoint` | `name: string` | Pin the current published generation. Repeating a name returns the original checkpoint. |
| `diff` | `name: string`, optional `diagnostics: bool` | Compare the checkpoint against the generation captured when query execution begins. |
| `forget` | `name: string` | Release a checkpoint. Unknown names are errors. |
| `callers` | `symbol: string`, optional `depth`, `limit` | Follow incoming edges; default depth 1. |
| `callees` | same | Follow outgoing calls; default depth 1. |
| `impact` | same | Follow incoming edges transitively; default depth 16, including private callers. |

Sync paths must be normalized repository-relative source paths: no absolute paths, `..`, empty
components, backslashes, colons, unsupported extensions, or declaration-only TypeScript files.
Duplicates are deduplicated. Include old and new source paths for an explicitly acknowledged
rename; one batch publishes atomically. Targeted discovery shares scanning's ignore, hidden-file,
and non-following symlink policy. It visits requested ancestors and may read ignore configuration,
but not unrelated source contents. Use `rescan` for directory or ignore-configuration changes.

Checkpoints require a completed initial index. Names contain 1–256 bytes; at most 64 names may be
retained. They share immutable generations, not a snapshot clone per name. During indexing, a
checkpoint pins the previous complete generation; `checkpoint_generation` identifies it.

### Graph-query contract

Selectors accept a unique short/qualified name or a file-qualified ID such as
`rust:src/lib.rs::module::function` or `ts:src/api.ts::Class::method`. Unknown or ambiguous selectors
are errors, not guessed matches. Selection inspects at most 10,000 symbols; file-qualified IDs
restrict the search to that file. Scope components may themselves contain `::` (Rust impl types).

Each query pins one generation and traverses outside the publication lock. `graph` contains:

- `symbols`: ID, file, source range, and shortest discovered depth; the selected symbol is depth 0.
- `calls`: caller, optional callee, target label, source range, and `resolution` (`resolved`,
  `ambiguous`, `unresolved`, or `uncertain`). Ambiguous candidates are separate possible edges and
  are traversed in either direction. Unresolved/uncertain outgoing calls have null callees;
  incoming queries cannot infer their destinations.
- `truncated` and a warning explaining syntactic/analysis limits. These are not compiler-complete
  results; an empty incoming result cannot prove absence of impact. Source ranges use one-based
  lines and columns (columns count UTF-8 bytes), matching the extractor.

`depth` is 0–32. `limit` is 1–256 symbols, including the selected symbol; default 64. Traversal
also stops at 1,024 retained calls, a 20,000-step/candidate work budget, or 48 KiB of encoded graph
JSON. Depth cutoffs report truncation when further calls exist. Byte limits account for JSON
escaping before retaining items. Cycles terminate, results are deterministic, and all limits
return structured partial results with explicit truncation, not malformed JSON.

## Watching and publication

`notify-debouncer-full` uses the platform-recommended backend, a 100 ms debounce interval,
non-following symlink configuration, and `NoCache`: invalidation does not require a second
file-ID tree cache. Events are hints, never authoritative edits. Watching starts before the
initial scan; events arriving during indexing remain pending for subsequent reconciliation.

The callback deduplicates up to 4,096 source paths without waiting for indexing. Directory,
removal, ignore-configuration, and other non-source events conservatively request a full scan.
Hidden paths are filtered except discovery control files. Lost-event flags, watcher errors, and
our own pending-path overflow retain a recovery-needed signal even if the path buffer is cleared.
A loss arriving during recovery requires another scan. Failed automatic updates retry after one
second. With watching enabled, a 30-second reconciliation also catches external/global ignore
changes and silent loss; failed watchers are restarted on that interval. The debounce library
and OS have their own buffering/resource limits; our path limit is not a total watcher-memory cap.

Per-file reading and analysis use a private Rayon pool with job-local analyzers. Source bytes are
released after each job rather than retained for the entire batch. Each source is limited to
8 MiB; oversized files fail the whole batch explicitly. At most one such source per active worker
is being read/analyzed (allocator and parser overhead is additional). Cached content/parser hashes
skip unchanged analysis, including syntax failures. Results and errors are assembled in sorted
path order; resolution runs once before atomically publishing a complete generation.

A failed initial scan publishes nothing; repair the error and rescan (or wait for automatic
recovery). A partial sync cannot establish an initial index. Later fatal I/O, UTF-8, path, size,
or graph errors leave the repository and published snapshot unchanged. Syntax failures instead
publish cached diagnostics; affected paths are omitted from both sides of diffs.

## Response contract

Each response includes:

- `version`, canonical `root`, and service `instance`.
- `generation`: null until the first successful scan, then monotonically increasing within that
  instance. Changed batches advance once; no-op batches do not advance.
- `index_complete`: an initial complete index exists, **not** compiler completeness or freshness.
- `watching`: a watcher is installed without a reported backend error. `recovery_needed` reports
  known reconciliation debt; `watcher_error` reports backend/loss/overflow problems. Successful
  scans clear recovery debt only if no newer recovery signal arrived; watcher startup failures
  remain visible until a watcher is installed and reconciliation succeeds.
- `files`, `analysis_issue_count`, and up to 32 `analysis_issues` from the queried generation.
  Diagnostic strings are capped at 2 KiB plus an explicit diagnostic-truncation marker.
- `progress`: null when idle, otherwise `phase` (`discovering`, `indexing`, `resolving`, or
  `publishing`), `files_read`, `files_analyzed`, and `total_files` (null during discovery).
  Analysis progress includes cache hits, not just parser invocations.
- `last_update_error`: the last failed update, retained until a successful full rescan.
- `checkpoint_generation`, `diff`, `graph`, and `error` when applicable; otherwise null.
- `truncated`: graph-query truncation, or depth/node/48-KiB text limits in rendered diffs. Diff
  byte truncation occurs at a UTF-8 boundary and ends with an explicit marker.

Even `watching: true` and `recovery_needed: false` do not prove worktree freshness: notifications
can still be in flight or debouncing. Explicit path sync acknowledges those writes; `sync([])`
is only an explicit-update queue barrier. Scans read files at different moments; atomic graph
publication is not a filesystem-wide file-write transaction.

## Resource boundaries and verification

Nonblocking ingress holds at most 64 incomplete frames and evicts the oldest on overload. Four
query workers and eight queued queries are separate from the single writer and its eight queued
updates. Full queues reject requests explicitly. Waiting mutations and slow frame senders do not
occupy query workers. Reply writes have deadlines; slow mutation readers can delay the writer,
not consume read workers. Sustained overload can still reject/evict clients; there is no latency
or fairness guarantee against arbitrary load. Ingress polls at a two-millisecond interval.

Updates still rebuild global resolution/edges. Staging clones the file map and publication clones
the snapshot; prepared artifacts and retained checkpoints can consume substantial memory. Measure
real workloads before adding structural sharing. Diff text is bounded after rendering, so its
expansion/allocation costs are not eliminated. No async runtime, RPC framework, graph-library
migration, or concurrent-map/publication crate is needed. `libc` supplies only Unix owner-ID and
no-follow/nonblocking lock-open primitives; transport remains `std::os::unix::net`.

A local release-build smoke measurement on macOS arm64 used 1,000 files (one empty Rust function
per file), four indexing threads, and watching disabled. Sixteen distinct checkpoints, each
followed by a one-file edit, raised sampled owner RSS from 13.8 MiB to 38.4 MiB. Mean explicit
update round-trip time was 6.7 ms including CLI process startup. Forgetting checkpoints did not
immediately reduce RSS (allocator retention is possible). This is one synthetic run, not a peak
memory, p95 latency, or real-repository performance claim.

Behavioral tests cover serial/parallel replay of every fixture in both directions, atomic failures,
watcher saves/renames/deletions/ignore changes, injected overflow and recovery epochs, shared startup
and crash recovery, long paths, concurrent reads under slow-frame/mutation pressure, graph-query
ambiguity/cycles/limits, protocol errors, and checkpoint retention. Run the README checks on both
Linux and macOS. Pi lifecycle/generation acknowledgements remain Milestone 4.
