# Repository service: Milestone 3, stage 1

Experimental protocol v1. Unix only. This is the first service stage, **not completion of
Milestone 3** or the service-backed Pi integration.

## Run

```sh
cargo build
# Foreground owner; stop with Ctrl-C. Usually request starts it instead.
target/debug/stackscout serve /path/to/workspace

# Connect or start an owner. Concurrent startup losers reconnect without scanning.
target/debug/stackscout request . '{"version":1,"op":"status"}'
# Wait for the initial scan and previously queued updates without rescanning.
target/debug/stackscout request . '{"version":1,"op":"sync","paths":[]}'
target/debug/stackscout request . '{"version":1,"op":"checkpoint","name":"session-1"}'
# After editing these files:
target/debug/stackscout request . '{"version":1,"op":"sync","paths":["src/lib.rs"]}'
target/debug/stackscout request . '{"version":1,"op":"diff","name":"session-1"}'
```

`request` prints a JSON response. Operation failures appear in `error`; callers must check
that field even when the CLI exits successfully. Transport/startup failures exit unsuccessfully.
The Rust `service::request` API connects only; subprocess startup belongs to the CLI.
The current Pi extension still runs directory diffs and does not connect to this service.

## Ownership and transport

The canonical root owns `.stackscout/owner.lock` and `.stackscout/service.sock`. The owner holds
an OS lock for its lifetime. Never unlink the lock file while a service may be running.
A startup loser cannot start indexing. An owner recovering after a crash removes only a stale
socket, under the lock; it refuses to replace a regular file or symlink endpoint. The service
directory and socket use user-only permissions. This assumes a trusted, user-controlled local
workspace, not an adversarial shared filesystem.

Use one UTF-8 JSON request followed by a newline per Unix socket connection. One JSON response
followed by a newline closes the connection. Requests require `version: 1` and reject unknown
operations/fields. Requests are limited to 64 KiB, input reads/writes have five-second timeouts,
and the bundled client waits up to 120 seconds for a response. It does not replay a request
after a transport failure: a disconnected mutation may still complete. Reconnect and inspect
its generation instead of assuming cancellation.

The owner stays alive when clients disconnect. This stage has no shutdown request or idle
expiry; terminate the foreground owner, or the PID at the start of `instance`. A restart gets
a new instance identity and loses all in-memory checkpoints. Clients must compare **instance
and generation**, not the generation number alone. Missing checkpoints are errors, never an
instruction to silently recreate a session-start baseline.

Unix socket path-length limits apply, including the canonical root prefix. Long workspace
paths and non-Unix transports need a later endpoint/fallback design.

## Requests

All examples include `"version":1`:

| `op` | Other fields | Meaning |
| --- | --- | --- |
| `status` | none | Read readiness, generation, progress, and diagnostics without waiting for updates. |
| `sync` | `paths: string[]` | Read and update only named source files; missing/ignored files become deletions. An empty list is a queue barrier, not a filesystem sync. |
| `rescan` | none | Reconcile the whole worktree, including deletions and ignore changes. Reuse cached analysis for unchanged hashes. |
| `checkpoint` | `name: string` | Pin the current published generation. Repeating a name returns the original checkpoint, including across reconnects. |
| `diff` | `name: string`, optional `diagnostics: bool` | Compare that checkpoint against the generation captured when this request begins. |
| `forget` | `name: string` | Release a checkpoint. Unknown names are errors. |

Sync paths use the core's normalized repository-relative source-path rules. No absolute paths,
`..`, backslashes, unsupported extensions, or declaration-only TypeScript paths. Duplicate
paths are deduplicated. Include both old and new paths for a rename; their changes publish
atomically. Targeted discovery shares the directory scanner's ignore, hidden-file, and
non-following symlink policy. It traverses requested ancestors and may read ignore configuration,
but does not read unrelated source contents. Change ignore rules with a full `rescan`.

Checkpoints require a completed initial index. Names contain 1–256 bytes; at most 64 names are
retained. Checkpoints share immutable generations with readers, rather than cloning a snapshot
for each name. A checkpoint during an update pins the previous published generation; the
response's `checkpoint_generation` records which one was captured.

## Response contract

Each response includes:

- `version`, canonical `root`, and service `instance`.
- `generation`: null until the initial scan publishes, then monotonically increasing within
  that instance. One successful changed batch advances once; no-op batches do not advance.
- `index_complete`: whether a complete initial index has been published. This **does not** mean
  the worktree is fresh or that analysis is compiler-complete.
- `watching`: always **false** in this stage. Bash/external changes are invisible until notified
  with `sync(paths)` or reconciled with `rescan`.
- `files`, `analysis_issue_count`, and up to 32 `analysis_issues` from the queried generation.
  Syntax failures are cached diagnostics, not fatal batch failures; affected paths are omitted
  from both sides of a diff as in the directory API.
- `progress`: null when idle, otherwise `files_read` and `total_files` (null while discovering).
  This measures source reading, not parsing progress; parsing/resolution may still be running
  after all files have been read.
- `last_update_error`: the last failed update, retained until a successful full rescan. Queries
  may still use the prior complete generation; do not interpret it as fresh worktree evidence.
- `checkpoint_generation`, `diff`, and `error` when applicable; otherwise null.
- `truncated`: true when the rendered diff contains graph-depth/node truncation markers or
  its text exceeds 48 KiB. Byte-limited text ends with an explicit marker and is cut only at
  a UTF-8 boundary. This is not yet structured graph pagination or drill-down.

An initial scan failure publishes nothing. A partial sync cannot establish an initial index;
repair the source/error and use `rescan`. Later fatal I/O, path, UTF-8, or graph errors leave
both the worker repository and published generation unchanged. Publication swaps a complete
immutable snapshot under a short lock. Diff computation runs after releasing that lock.

## Boundaries and remaining work

One background writer serializes scans and updates. Four connection workers permit two readers
alongside a waiting update request; eight update slots bound queueing. Multiple slow clients
or enough simultaneous blocking mutations can occupy all connection workers, so this is not
a fairness or latency guarantee. Updates are not filesystem-wide atomic snapshots: a rescan
can read files at different moments. Explicit multi-file notification after writes gives an
atomic graph publication, not a cross-process file-write transaction.

Initial loading now uses one atomic batch and one graph rebuild. Updates still rebuild global
resolution/edges; staging clones the file map and publication clones the snapshot. Scans also
hold source bytes for the batch. Peak memory can be substantial on large repositories, and
retained checkpoints keep old generations alive. Measure before adding structural sharing.
Diff text is bounded after rendering, so graph expansion/allocation costs are not eliminated.

Remaining Milestone 3 stages:

1. Filesystem watching, debounce/deduplication, ignore-policy changes, and overflow/loss recovery.
2. Bounded parallel file indexing and stronger scan/progress reporting.
3. `callers`, `callees`, and `impact` queries, plus endpoint/platform fallback and service hardening.

Milestone 4 then connects Pi, acknowledges edit/write generations, and supplies bounded graph
queries with structured locations/completeness. No Pi lifecycle changes are part of this stage.
