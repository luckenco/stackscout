# Repository service

Experimental protocol v1, Linux/macOS only. One canonical-root owner maintains a watched
index and immutable generations. The [repo-local Pi extension](../README.md#pi) shares this service.

## Run

```sh
cargo build
target/debug/stackscout serve /path/to/workspace  # foreground; stop with Ctrl-C
# request connects or starts an owner:
target/debug/stackscout request . '{"version":1,"op":"status"}'
target/debug/stackscout request . '{"version":1,"op":"sync","paths":[]}'
target/debug/stackscout request . '{"version":1,"op":"checkpoint","name":"session"}'
# After editing src/lib.rs:
target/debug/stackscout request . '{"version":1,"op":"sync","paths":["src/lib.rs"]}'
target/debug/stackscout request . '{"version":1,"op":"diff","name":"session"}'
target/debug/stackscout request . '{"version":1,"op":"impact","symbol":"rust:src/lib.rs::diff_dirs","depth":8,"limit":64}'
```

Check JSON `error` even when the CLI exits successfully; transport/startup failures exit
unsuccessfully. Rust `service::request` connects only; the CLI owns subprocess startup.

Set owner configuration before startup; reconnecting does not reconfigure it:

- `STACKSCOUT_INDEX_THREADS=1..8`: private Rayon pool; default CPU count capped at 4.
- `STACKSCOUT_WATCH=0`: disable watching and periodic reconciliation.

## Requests

Every request requires `"version":1`; unknown operations/fields are rejected.

| `op` | Fields | Meaning |
| --- | --- | --- |
| `status` | — | Read readiness, generation, progress, health, and diagnostics. |
| `sync` | `paths: string[]` | Re-read named files; missing/ignored files become deletions. `[]` waits for initial indexing and queued explicit updates, **not** watcher delivery or a scan. |
| `rescan` | — | Reconcile all files, deletions, and ignore changes; reuse unchanged analysis. |
| `checkpoint` | `name` | Pin current publication; repeating a name returns the original checkpoint. |
| `diff` | `name`, optional `diagnostics: bool` | Compare checkpoint to generation captured at query execution. |
| `forget` | `name` | Release checkpoint; unknown names are errors. |
| `callers` / `callees` | `symbol`, optional `depth`, `limit` | Incoming/outgoing graph traversal; default depth 1. |
| `impact` | same | Incoming traversal including private callers; default depth 16. |
| `review` | `name`, `baseline`, `baseline_generation`, `scope: {files?: string[], symbols?: string[]}` | Relevant changes since this scope's last acknowledgement; initially seeds from the named checkpoint at the expected generation. |
| `acknowledge` | `name`, `token` | Advance a review to the exact generation pinned by that result. |
| `forget_review` | `name` | Release a rolling review and its pending result; unknown names are errors. |

Sync paths are normalized, repository-relative supported source paths: no absolute paths,
`..`, empty components, backslashes, colons, or TypeScript declaration files. Paths are
deduplicated; include old and new paths for renames. Targeted sync shares scan ignore/hidden/
non-following-symlink rules and visits ancestors/ignore configuration, not unrelated source
contents. Use `rescan` for directory or ignore changes.

Checkpoints require a completed initial index; during updates they pin the previous complete
generation. Names are 1–256 bytes. The retention budget is 64 slots: one per named checkpoint,
two reserved per rolling review (acknowledged and pending generations). Names share immutable generations.
Restarts lose checkpoints; missing names are errors, never recreated baselines.

## Consistency and responses

Queries pin one generation and traverse outside the publication lock. Responses include:

| Fields | Contract |
| --- | --- |
| `version`, `root`, `instance` | Protocol version, canonical root, owner identity; instance changes on restart. |
| `generation` | Null before initial publication; changed batches advance once, no-ops retain it. Monotonic only within an instance. |
| `index_complete` | Initial index exists; **not** compiler completeness or worktree freshness. |
| `watching`, `recovery_needed`, `watcher_error` | Watcher installed without reported backend error, reconciliation debt, and backend/loss/overflow error. |
| `files`, `analysis_issue_count`, `analysis_issues` | Queried-generation diagnostics: up to 32 issues, each capped at 2 KiB plus truncation marker. |
| `files_parsed` | Cumulative cache-miss file analyses completed by this owner, including syntax failures and work later rolled back. Cache hits do not increment it. Operational counter, not pinned-generation data; resets on restart. |
| `progress` | Null when idle; otherwise phase (`discovering`, `indexing`, `resolving`, `publishing`), `files_read`, `files_analyzed`, `total_files` (null during discovery). Analysis counts include cache hits. |
| `last_update_error` | Retained until a successful full rescan. |
| `checkpoint_generation`, `diff`, `graph`, `error` | Operation-specific results; otherwise null. |
| `truncated` | Graph/review limits or diff depth/node/48-KiB limit reached. Diff byte cuts preserve UTF-8 and append a marker. |
| `review`, `review_name`, `review_token`, `review_initialized` | Scoped evidence, review identity, acknowledgement token, and whether seeded from the original checkpoint on this request; otherwise null/false. |

Service diff text includes file-qualified ranges. `-` lines use checkpoint locations; other
lines use the queried generation. Roots show definition ranges; descendants show call/branch
sites (including the caller's file for cross-file calls), not callee definitions. Deleted-file
locations remain available from the checkpoint. Directory/API diffs keep their compact format.

Watching is eventually consistent even when health is green. Await explicit path sync to
acknowledge known writes; protocol v1 has no minimum-generation query parameter. Scans read
files at different times: atomic publication is not a filesystem-wide write transaction.

Failed initial scans publish nothing; partial sync cannot establish readiness. Later fatal
I/O, UTF-8, path, size, or graph errors preserve the previous repository/publication. Syntax
failures instead publish cached diagnostics and omit affected files from both diff sides.

## Graph-query contract

Selectors accept unique short/qualified names or IDs such as `rust:src/lib.rs::module::fn`
and `ts:src/api.ts::Class::method`. Unknown/ambiguous selectors are errors. Selection inspects
at most 10,000 symbols; file-qualified IDs restrict it to that file. Rust impl scope components
may themselves contain `::`.

`graph` contains:

- `symbols`: ID, file, source range, shortest discovered depth (selected symbol = 0).
- `calls`: caller, optional callee, target, range, and `resolution` (`resolved`, `ambiguous`,
  `unresolved`, `uncertain`). Ambiguous candidates are separate possible edges in either
  direction; unresolved/uncertain outgoing calls have null callees and no inferred incoming edge.
- `truncated` and a syntactic-analysis warning. Empty incoming results do not prove no impact.

Ranges use one-based lines/columns; columns count UTF-8 bytes. Results are deterministic and
cycle-safe. Limits return structured partial results: depth 0–32, symbols 1–256 (default 64,
including root), 1,024 calls, 20,000 steps/candidates, and 48 KiB encoded graph JSON. Depth
cutoffs report truncation when further calls exist; byte budgeting includes JSON escaping.

## Scoped review

```sh
# Use checkpoint_generation from the checkpoint response, not a guessed generation.
target/debug/stackscout request . '{"version":1,"op":"review","name":"api-review","baseline":"session","baseline_generation":1,"scope":{"files":["src/query.rs"]}}'
# After inspecting the returned result, substitute its review_token:
target/debug/stackscout request . '{"version":1,"op":"acknowledge","name":"api-review","token":"<returned-token>"}'
target/debug/stackscout request . '{"version":1,"op":"forget_review","name":"api-review"}'
```

A review name binds permanently to its normalized scope and original checkpoint identity/generation.
Scope accepts 1–16 selectors of 1–512 bytes; files use sync's normalized relative source paths, not
folders/globs. Files select their indexed callables; symbols use graph-query selectors. Lists are
sorted/deduplicated. Different scopes need separate names: acknowledging one cannot hide another's
unreviewed changes. Pi derives names from the session checkpoint and scope; no task registry or
ownership inference is involved. Reviews are session-wide, not conversation-branch-local.

For each side independently, walk outgoing dependencies and incoming callers. Report local syntax,
visibility, or resolved-call changes reachable in either direction, plus affected callers when a
scoped callable changes. No mixed-direction inference through a shared dependency: unrelated siblings
are not automatically relevant. `review` contains:

- `witnesses`: side (`before`/`after`), relation (`local`, `dependency`, `caller`, `affected_caller`),
  change (`added`, `removed`, `modified`, `affected`), scoped symbol ID, definition/range, one shortest
  call path, and direct call evidence available in that traversal. This is not a complete syntax patch.
- `boundaries`: unresolved/uncertain calls and ambiguous candidate edges encountered by the walks.
- `missing`: selectors missing or ambiguous on each side, including deleted scoped files/symbols.
- `analysis_issues`: diagnostics from both snapshots. Failed paths are omitted from **both** graphs,
  including resolution, rather than reported as deletions/additions.
- `truncated`: any selection, traversal, evidence, or diagnostic-count cap reached.

Witness/path/call ranges belong to their labeled side; a call's caller ID identifies its file.
Output is deterministic and cycle-safe. At most 32 selected roots per side, two traversals per root,
16 hops and 64 symbols per traversal, with the existing 1,024-call/20,000-step/48-KiB query budgets.
Retained evidence is bounded while constructed: 64 witnesses, 32 boundaries, 32 KiB encoded evidence
(including escaping), plus at most 32 diagnostics of 2 KiB each. Narrow scopes when truncated.
Repeated roots may repeat evidence. Traversals reuse the bounded query implementation, not full diff
trees; selection/local-shape comparisons and rebuilding graphs around analysis failures remain costs.
No new latency or token-savings claim is established by these limits.

A successful review returns an instance-qualified token and pins the exact `generation` it compared.
It does **not** advance the review checkpoint. `acknowledge` advances to that pinned generation even
if newer writes have already published; the acknowledgement response's `checkpoint_generation` is
what was inspected, while `generation` may be newer. Later changes remain visible. A newer review
supersedes the previous pending token, including queries still computing; stale tokens are errors.
Retrying the most recently successful acknowledgement is a no-op. A result must finish computing
before its token can be acknowledged. Disconnect/cancellation can leave a pending result or completed
acknowledgement; mutations are not replayed automatically. Retry the same acknowledgement token if
its reply was lost, or inspect a new review if superseded.

Acknowledgement is explicit acceptance of the inspected **bounded, advisory** result, including its
reported uncertainty/omissions; it is not proof that every relevant path was analyzed. Inspect source
and omitted evidence first. The immutable session checkpoint never changes. Rolling results replace
pins rather than accumulating names. Forget releases both review pins; requesting that review again
explicitly reports `review_initialized: true` and seeds from the original checkpoint, never current
worktree state. Missing/replaced seed checkpoints fail. Restarts lose reviews/tokens along with all
checkpoints; Pi refuses to recreate its lost session baseline. Owners do not automatically evict.

## Ownership and transport

The owner holds `.stackscout/owner.lock` for its lifetime. **Never unlink a live owner's lock.**
Startup losers exit before scanning/watching; the CLI reconnects. Stale sockets are removed
only under the lock; non-socket/unowned endpoints are refused.

Use `.stackscout/service.sock`, or, when its path is at least 100 bytes,
`/tmp/stackscout-<effective-uid>/<blake3-of-canonical-root>.sock`. Root aliases choose the same
endpoint. Directories must be real, owned, mode 0700; locks/sockets owned, mode 0600. Lock
opens reject symlinks; unsafe permissions fail rather than being repaired. This assumes a
trusted workspace, not an adversarial shared filesystem. Stale fallback sockets recover on restart.

One newline-terminated UTF-8 JSON request and response per connection. Requests: 64 KiB,
5-second absolute incomplete-frame deadline. Replies: 5-second write deadline; the bundled
client waits up to 120 seconds and rejects incomplete or >1-MiB responses.

Disconnected mutations may still complete and are never automatically replayed. Reconnect
and inspect instance/generation rather than assuming cancellation. Owners outlive clients;
there is no shutdown operation or idle expiry. Stop the foreground owner or PID prefix of `instance`.

## Indexing, recovery, and resource limits

- Watching starts before scanning: `notify-debouncer-full`, recommended backend, 100-ms debounce,
  no symlink following, `NoCache`. Events during indexing remain pending.
- The callback deduplicates up to 4,096 paths without waiting for indexing. Directory/removal/
  ignore/non-source events request full scans; hidden paths are filtered except discovery controls.
- Lost events, errors, and buffer overflow retain recovery debt; loss during recovery requires
  another scan. Successful scans clear only older debt. Watcher startup failures remain visible
  until installation and reconciliation succeed. Failed automatic updates retry after 1 second;
  30-second reconciliation catches silent loss/global ignore changes and restarts failed watchers.
  OS/debouncer buffers are additional to our path cap.
- Job-local analyzers read at most one 8-MiB source per worker, releasing bytes per job; parser/
  allocator overhead is additional. Oversized files fail the batch. Content/parser hashes reuse
  unchanged analysis, including failures. Sorted results resolve once and publish atomically.
- Ingress polls every 2 ms, retains 64 incomplete frames, evicts oldest on overload. Four query
  workers/8 queued queries are separate from one writer/8 queued updates. Full queues reject.
  Slow senders/waiting mutations do not occupy query workers; slow mutation readers can delay
  the writer. Sustained overload has no fairness/latency guarantee.
- Updates rebuild global edges; staging/publication clone maps/snapshots. Retained checkpoints
  cost memory. Diff bytes are capped **after rendering**, so expansion/allocation remain unbounded
  by that byte cap. See [measurements](performance.md#service-smoke-measurement) before optimizing.

Run the [README checks](../README.md#checks) on Linux and macOS. Coverage includes serial/parallel
fixture replay, atomic failures, watcher reconciliation/recovery, startup/crash/long-path ownership,
read pressure, graph ambiguity/cycles/budgets, protocol limits, and checkpoint retention.
