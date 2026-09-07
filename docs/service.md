# Repository service

Protocol v1, Linux/macOS. Build/start commands: [README](../README.md#repository-service).
Request/response types: [`protocol.rs`](../src/service/protocol.rs).

## Requests

Every request requires `"version":1`; unknown operations/fields are errors.
Check response `error` even when the CLI exits successfully.

| `op` | Fields | Effect |
| --- | --- | --- |
| `status` | — | Read generation, readiness, health, and diagnostics. |
| `sync` | `paths: string[]` | Read selected files; missing/ignored files become deletions. `[]` waits for initial indexing and queued explicit updates, not watcher delivery. |
| `rescan` | — | Reconcile sources and ignore rules; reuse unchanged analysis. |
| `checkpoint` | `name` | Pin current generation; repeating a name returns its original pin. |
| `diff` | `name`, optional `diagnostics: bool` | Compare checkpoint with the captured current generation. |
| `forget` | `name` | Release a checkpoint; unknown names are errors. |
| `callers` / `callees` / `impact` | `symbol`, optional `depth`, `limit` | Query incoming/outgoing paths; impact includes private callers. |
| `review` | `name`, `baseline`, `baseline_generation`, `scope: {files?: string[], symbols?: string[]}` | Compare a scope, initially seeded from the named checkpoint at its expected generation. |
| `acknowledge` | `name`, `token` | Advance a review to exactly its inspected generation. |
| `forget_review` | `name` | Release the review and its pending result. |

Paths are normalized, relative supported source files: no absolute paths, empty/`.`/`..` components,
backslashes, colons, or declaration files. Include both paths for renames; rescan for ignore changes.
Names are 1–256 bytes. Checkpoints require initial indexing. There are 64 retention slots:
one per checkpoint, two per review. Restarts lose all pins; missing baselines are never recreated.

## Consistency and responses

Queries pin immutable generations outside the publication lock. `generation` is null before initial
publication, increases on changed batches, and is meaningful only within an `instance`.
`root` is canonical; clients must validate root, instance, and protocol version.
`index_complete` means an initial index exists, not that watching is current or analysis complete.
Fatal I/O/UTF-8/size/graph errors preserve the old publication. Syntax failures publish diagnostics
and omit failed files from both comparison sides, including resolution, rather than inventing deletions.
`last_update_error` persists until a successful full rescan. `files_parsed` is an owner-wide counter,
including failed/rolled-back work, not a pinned-generation count. Watch health/progress remain advisory.
Four bounded diff payloads are cached FIFO by generation pair/diagnostics; health headers stay fresh.

Graph selectors accept unique names or `language:file::qualified::name`; unknown/ambiguous names fail.
`graph` reports symbols, calls, uncertainty, ranges, and truncation. Unresolved calls create no incoming
edges. Ranges are one-based, with UTF-8 byte columns; diff `-` ranges use the checkpoint, others the current side.

## Scoped review

Scopes contain 1–16 file/symbol selectors of 1–512 bytes, normalized and bound to the review name/seed.
Each side independently walks dependencies and callers; unrelated siblings are not inferred relevant.
Results contain witnesses, unresolved/ambiguous boundaries, missing selectors, diagnostics, and truncation.
A review pins its result without advancing the baseline. Acknowledgement advances only to that token's
pinned generation, even after newer writes. New reviews supersede pending tokens; stale tokens fail.
Repeating the last successful acknowledgement is a no-op. Forgetting a review resets its next use to
its original seed. Neither reviews nor acknowledgements change the session checkpoint.

## Ownership and transport

The owner holds `.stackscout/owner.lock`; never unlink a live lock. Directories must be real, owned,
mode 0700; locks/sockets owned, mode 0600. Unsafe paths are refused, not repaired. Workspace is trusted.
Socket: `.stackscout/service.sock`, or `/tmp/stackscout-<uid>/<blake3-of-canonical-root>.sock` for paths
at least 100 bytes long. Bind a reserved `.tmp` sibling, chmod, then atomically rename to publish.
Stale public/staging sockets are removed only under the lock; non-sockets, symlinks, and foreign owners fail.
One LF-terminated UTF-8 JSON request/reply per connection: 64-KiB requests, 1-MiB replies.
Incomplete requests have a 5-second deadline; writes have a 5-second timeout. Rust reads time out after
120 seconds; Pi enforces a 120-second absolute exchange deadline and rejects invalid UTF-8/incomplete frames.
Only pre-connect absence/refusal permits automatic startup/retry. Disconnected mutations may complete;
never automatically replay potentially sent requests. Owners have no idle expiry or shutdown operation.
Pi cancellation waits for socket closure. Auto-started owners inherit stderr: route it to a file/inherited
stream, not a captured pipe when awaiting EOF. Stop old owners before binary upgrades.

## Resource limits

- Configure before startup: `STACKSCOUT_INDEX_THREADS=1..8` (default capped at 4), `STACKSCOUT_WATCH=0` to disable watching.
- Watching: 100-ms debounce, 4,096 pending paths; overflow/errors require reconciliation. Retry after 1 second, reconcile every 30 seconds.
- Sources: 8 MiB/file. Changed files reparse; batches rebuild global resolution/edges. Generations share immutable artifacts but still cost memory.
- Ingress: 32 accepts/batch, 64 incomplete frames (oldest evicted). Four query workers/8 queued queries; one writer/8 queued updates. Full queues reject.
- Graphs: depth 0–32 (default 1, impact 16), 1–256 symbols (default 64), 1,024 calls, 20,000 steps/candidates, 48 KiB JSON.
- Reviews: 32 roots/side, two traversals/root, depth 16/64 symbols; 64 witnesses, 32 boundaries, 32 KiB evidence. Inspect omissions before acknowledging.
- Diagnostics: 32 issues, 2 KiB each plus markers. Diffs: depth 32/10,000 expanded nodes per root, 48 KiB text plus marker; byte caps apply after rendering, not allocation.

Run [README checks](../README.md#checks) on both OSes. Optional startup stress:
`cargo test --release --test service startup_stress -- --ignored --nocapture`.
