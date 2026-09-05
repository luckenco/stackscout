# stackscout

An experimental static call-graph index and semantic diff tool for coding agents,
inspired by [Dillon Mulroy's call-stack workflow](https://x.com/dillon_mulroy/status/2069852516309762411?s=20)
and [fff's resident repository index](https://github.com/dmtrKovalenko/fff).

StackScout traces changed functions back to affected callers, preserving call order,
branch context, and uncertainty. The goal is to guide agents to relevant source ranges
without repeatedly reading the whole repository—not to replace source reads or tests.

## Directory diff

Rust and TypeScript (`.rs`, `.ts`, `.tsx`, `.mts`, `.cts`) are parsed with Tree-sitter;
declaration-only TypeScript files are skipped.

```sh
cargo build
target/debug/stackscout diff-dir <before-directory> <after-directory>
```

Adding `audit()` inside `save()` can produce:

```text
  ts:api.ts::create
~   ts:store.ts::save
+     ts:store.ts::audit
      ts:store.ts::insert
```

`~` marks local syntax, export, or resolved-call changes. `+`/`-` mark paths added/removed
in that context, not necessarily new/deleted implementations. Unchanged callee bodies
with no reachable changes in either graph become `[unchanged body omitted]`. Changed
and ambiguous descendants remain expanded; subtrees containing `[truncated]` stay visible.
Control characters in labels are escaped onto one line without changing literal identity
or source ranges. Compact rendering reduces output, not graph-building work.

Files with analysis failures are omitted from both sides. Issue transitions report incomplete
analysis; unchanged failures are hidden unless `diff-dir --diagnostics` is requested.

## Pi

Run `cargo build`, then start a **new Pi session** in this repository. The repo-local extension
uses `target/debug/stackscout` (`STACKSCOUT_BIN` overrides the executable) to connect/start a
shared service. Queries do not run Cargo or reparse unchanged files. When upgrading the binary,
stop any old owner before starting a new session: running owners do not reload executable code.

- `call_graph({action: "callers" | "callees" | "impact", symbol, depth?, limit?, refresh?})`
  finds bounded call paths and source ranges. Use unique names or `language:file::qualified::name`.
- `call_graph_diff({refresh?, diagnostics?})` compares against the immutable session-start checkpoint.
  Changes are aggregate workspace changes, not one agent's authorship. Drill down with `call_graph`.
- `call_graph_review({files?: string[], symbols?: string[], refresh?})` reviews changes relevant
  to an explicit scope since its last acknowledgement (initially session start). Supply 1–16
  repository-relative files and/or symbol selectors. Before/after witness paths retain removed
  edges, dependencies, callers, source ranges, and uncertainty; unrelated changes are omitted.
- `call_graph_ack({review, token})` explicitly acknowledges the review name/token returned above.
  It advances **only that scope to that result's generation**, never later unseen writes or the
  session baseline. Inspect warnings and any omitted evidence before acknowledging; this is not
  a completeness or correctness certificate. A newer review supersedes older unacknowledged tokens.

For example, review `{files: ["src/query.rs"], refresh: true}`, inspect its paths, then acknowledge
its returned review name/token. Another scope has independent review history. Reload/resume retains
reviews in the same owner. `forget_review` releases a review explicitly; its next use reports a reset
back to the original session checkpoint, never a late worktree capture. See [scoped review](docs/service.md#scoped-review).

Startup reconciles saved files before capture; rescans read source bytes but reuse unchanged analysis.
Successful `edit`/`write` results await path sync. Failed sync adds a warning without misreporting the
file write as failed; the next graph query retries a full reconciliation. Syncs and rescans share
one queue so older completions cannot clear newer failures; graph traversal stays concurrent.
Bash/external writes remain
eventually consistent: use `refresh: true` when freshness matters, even with a healthy watcher.

Checkpoint identity and capture failures survive reload/resume in session metadata. Transient
reload/resume scan or transport failures retain the original identity for retry. Restarted owners,
forgotten checkpoints, and failed/interrupted capture make the baseline unavailable, never silently
recaptured. Current-graph queries remain usable. Upgrading from the directory adapter requires a new
session. Cancellation stops only client requests, not the owner or saved-write acknowledgement;
disconnected service mutations may still finish. Shutdown retains checkpoints for resume, up to the
service's 64-slot retention limit (each review reserves two slots); use `request` with `forget` and the checkpoint name from tool details
to release one explicitly. No automatic eviction or owner shutdown.

Results include generations, locations, analysis issues, watcher health, uncertainty, and truncation.
Output is capped at 2,000 lines / 50 KiB. Pi display overflow is saved to a temporary file, but that
file cannot recover service-side omissions. Hidden-directory discovery still excludes the extension
itself. Static results supplement source reads and tests; they do not prove behavioral independence.

## Repository service

Linux/macOS clients can share a watched index, immutable generations, named checkpoints,
and bounded `callers`, `callees`, and `impact` queries with source locations.

```sh
target/debug/stackscout request . '{"version":1,"op":"sync","paths":[]}'
target/debug/stackscout request . '{"version":1,"op":"checkpoint","name":"session"}'
# After editing src/lib.rs:
target/debug/stackscout request . '{"version":1,"op":"sync","paths":["src/lib.rs"]}'
target/debug/stackscout request . '{"version":1,"op":"diff","name":"session"}'
```

`request` connects or starts an owner; `serve <root>` runs one in the foreground.
Before integrating a client, read the [protocol and consistency contract](docs/service.md):
JSON `error` matters even on CLI success, watching is eventually consistent, and restarts
lose checkpoints. `sync([])` is not a watcher flush.

## Rust API

```rust
let mut repo = stackscout::Repository::new()?;
repo.change("lib.rs", b"pub fn run() {}")?;
repo.checkpoint("session");
repo.change("lib.rs", b"pub fn run() { audit(); } fn audit() {}")?;
println!("{}", repo.diff("session")?);
```

`change` takes relative paths/bytes; `delete` removes files. Neither reads the filesystem.
Content/parser hashes skip unchanged analysis, including syntax failures. Updates rebuild
global resolution/edges; public API checkpoints clone snapshots. The service batches graph
rebuilds and shares pinned generations. There is no persistent cache or public bulk-ingestion API.

## Resolution limits

Rust resolves explicit scoped `use` bindings, groups, aliases, and `crate`/`self`/`super`
paths for free/associated calls. Shadowing and ambiguity remain explicit. This assumes
Rust 2018+ conventional file layouts, not a Cargo module graph: globs, re-export chains,
`#[path]`, and inferred receiver types remain unsupported.

The graph is syntactic, not compiler-complete. Callback/closure bodies, macros, dynamic
dispatch, reflection, generated code, type-driven methods, and external calls may be omitted
or unresolved. Missing edges do not prove absence of impact. VCS revision lookup, non-Unix
transport, additional languages, and automatic agent coordination are not implemented.

## Checks

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
bun test tests/pi_extension.test.ts tests/pi_live.test.ts
```

Unit lifecycle tests use a stub Pi API. Live tests launch isolated Node/Pi SDK processes with
scripted model responses but real extension loading, agent loops, tools, persistence, service
execution, and truncation. They require Node and Pi on PATH (or `STACKSCOUT_PI_PACKAGE` pointing
to its installed package directory); missing Pi skips live tests explicitly. No npm install is
needed for a development Pi install; standalone bundled installs also need the matching
`@earendil-works/pi-ai` package beside Pi for scripted responses. Live fixtures disable watching
to prove explicit sync rather than incidental watcher freshness. Integration checks passed on
macOS and Linux arm64 (Docker with an init process).
For resolver measurements and the manual benchmark, see [performance](docs/performance.md).

[MIT License](LICENSE)
