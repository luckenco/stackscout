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

The repo-local extension exposes `call_graph_diff` against a session-start source copy.
Completed snapshots survive reload/resume; capture failures are retained in session metadata
and require a new session. Tool execution never captures a late baseline. Cancellation
stops its Cargo command, not baseline capture.

Each call reparses both directories. Output is capped at 2,000 lines / 50 KiB, with full
truncated output saved to a temporary file. Hidden-directory discovery excludes the extension
itself. Pi is **not yet connected to the repository service**.

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
bun test tests/pi_extension.test.ts
```

Bun lifecycle tests use temporary files and a stub Pi API; no npm install is needed.
They do not exercise Pi's loader, Cargo execution, or real truncation.
For resolver measurements and the manual benchmark, see [performance](docs/performance.md).

[MIT License](LICENSE)
