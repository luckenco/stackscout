# stackscout

Experimental Rust/TypeScript static call graphs and semantic diffs for coding agents.
Trace changed functions to affected callers with call order, branch context, source ranges,
and explicit uncertainty. Graphs guide source reads and tests; missing edges prove nothing.
Inspired by [call-stack workflows](https://x.com/dillon_mulroy/status/2069852516309762411?s=20)
and [fff's resident index](https://github.com/dmtrKovalenko/fff).

## Directory diff

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
in that context, not necessarily new/deleted implementations. Unchanged callee bodies with
no reachable changes become `[unchanged body omitted]`; changed/ambiguous descendants and
truncation stay visible. Labels escape control characters without changing literal identity.
Failed-analysis files are omitted from both sides; `--diagnostics` includes unchanged failures.

## Pi

Build first, then start a **new Pi session** in this repository. The repo-local extension uses
`target/debug/stackscout` (override with `STACKSCOUT_BIN`) to connect/start a shared owner.
Stop old owners before upgrading: running processes do not reload the binary.

- `call_graph({action: "callers" | "callees" | "impact", symbol, depth?, limit?, refresh?})`:
  bounded paths and locations. Select by unique name or `language:file::qualified::name`.
- `call_graph_diff({refresh?, diagnostics?})`: aggregate workspace changes since the immutable
  session-start checkpoint, not authorship. Drill down rather than rereading a truncated diff.
- `call_graph_review({files?: string[], symbols?: string[], refresh?})`: before/after evidence
  relevant to 1–16 explicit file/symbol selectors since that scope's last acknowledgement.
- `call_graph_ack({review, token})`: acknowledge the inspected review name/token, advancing only
  that scope to that result's generation. Later writes and the session baseline remain visible.

Use `refresh: true` after Bash/external writes when freshness matters. Successful `edit`/`write`
results await sync; failed sync warns and subsequent queries retry reconciliation. Watching is
only eventually consistent. Startup rescans reuse unchanged analysis.

Capture intent, identity, and failures persist across reload/resume. Lost checkpoints/restarted
owners make baselines unavailable, never silently recaptured; current-graph queries still work.
Cancellation stops client requests, not owners or saved-write acknowledgement. Checkpoints remain
for resume until explicit release/restart, with a 64-slot cap (two per rolling review).
Read [service contracts](docs/service.md) when integrating clients or releasing checkpoints.

Output is capped at 2,000 lines / 50 KiB. Pi overflow files cannot recover service-side omissions.
Inspect uncertainty/truncation and source before acknowledging; acknowledgement is not a
correctness certificate. Separate services/workspaces are safer for overlapping writers.

## Repository service

Linux/macOS clients share a watched index, immutable generations, and named checkpoints.
`request` connects or starts an owner; `serve <root>` runs one in the foreground.

```sh
target/debug/stackscout request . '{"version":1,"op":"sync","paths":[]}'
target/debug/stackscout request . '{"version":1,"op":"checkpoint","name":"session"}'
# After editing src/lib.rs:
target/debug/stackscout request . '{"version":1,"op":"sync","paths":["src/lib.rs"]}'
target/debug/stackscout request . '{"version":1,"op":"diff","name":"session"}'
```

Read the [protocol](docs/service.md) before integration: JSON `error` matters even on CLI
success, `sync([])` is not a watcher flush, and restarts lose checkpoints.

## Rust API

```rust
let mut repo = stackscout::Repository::new()?;
repo.change("lib.rs", b"pub fn run() {}")?;
repo.checkpoint("session");
repo.change("lib.rs", b"pub fn run() { audit(); } fn audit() {}")?;
println!("{}", repo.diff("session")?);
```

`change` takes relative paths/bytes; `delete` removes files. Neither reads the filesystem.
Hashes cache unchanged analysis/failures. Updates rebuild global edges; API checkpoints clone
snapshots, while the service batches rebuilds and shares generations. No persistent cache.

## Resolution limits

Tree-sitter parses `.rs`, `.ts`, `.tsx`, `.mts`, `.cts`; declaration-only TypeScript is skipped.
Rust explicit imports/groups/aliases and `crate`/`self`/`super` assume conventional Rust 2018+
file layouts, not Cargo's module graph. Globs, re-export chains, `#[path]`, type-driven receivers,
callbacks/closures, macros, dynamic dispatch, reflection, and generated/external code may be
unresolved or omitted. Hidden directories (including the extension itself) are excluded.
Test and production configurations mix. VCS history, more languages, non-Unix transport,
and automatic writer coordination are outside the MVP.

## Checks

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
bun test tests/pi_extension.test.ts tests/pi_live.test.ts
```

Live tests use real Node/Pi SDK processes with scripted model responses and watching disabled.
They require Node and Pi on PATH (or `STACKSCOUT_PI_PACKAGE`); missing Pi explicitly skips them.
Bundled Pi installs also need matching `@earendil-works/pi-ai` beside Pi. Service changes require
Linux and macOS checks; use an init process in Docker. Unit lifecycle tests stub the Pi API.
For manual measurements, see [performance](docs/performance.md).

[MIT License](LICENSE)
