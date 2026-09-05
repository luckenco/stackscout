# stackscout

stackscout is an experiment in giving coding agents an incremental view of how code is
changing.

It is heavily inspired by
[Dillon Mulroy's](https://x.com/dillon_mulroy/status/2069852516309762411?s=20) suggestion of asking
your coding agents for call stacks.

This works really well for me, since I've incorporated it in my workflow.

## Vision

[fff](https://github.com/dmtrKovalenko/fff) keeps a fast local repository index available to coding
agents. The idea here is to do something similar for call graphs and changes over time.

In a repo worked on by several coding agents, each agent is a different head of the same hydra.
StackScout could let one head see what another head changed, and which callers or entrypoints that
change affects, without re-reading and re-ingesting the whole file. A shared index could answer
questions such as:

- What call paths changed since this agent started?
- What is another agent changing in this repo right now?
- Did two agents change overlapping parts of the graph?

StackScout may help with this workflow, or simply provide a faster way to inspect call stacks.

## What it does today

StackScout provides a one-shot directory CLI, an in-memory Rust API, and an experimental Unix
repository service for Rust and TypeScript. It:

- parses `.rs`, `.ts`, `.tsx`, `.mts`, and `.cts` files with Tree-sitter;
- builds a static call graph for each directory;
- compares the graphs and traces changes back to affected public or exported callers;
- preserves call order and branch context; and
- reports unresolved or ambiguous calls instead of guessing.

```sh
stackscout diff-dir [--diagnostics] <before-directory> <after-directory>
```

The repo-local Pi extension snapshots supported source files at session start and exposes this diff
through `call_graph_diff`. Completed snapshots are retained for resumed sessions. If capture fails,
the tool reports the failure and requires a new session; it never lazily snapshots after edits.
The failure is retained in Pi session metadata across reload/resume. Cancelling a tool call only
cancels its Cargo command, not session baseline capture.

For example, adding an `audit` call inside `save` can produce:

```text
  ts:api.ts::create
~   ts:store.ts::save
+     ts:store.ts::audit
      ts:store.ts::insert
```

### Compact output

Added and removed paths retain their call edges, but callee bodies with no changed paths
reachable in either graph are replaced by `[unchanged body omitted]`. Changed descendants
—including ambiguous candidates—remain expanded under every explanatory root. Subtrees
containing `[truncated]` are not collapsed, so existing expansion limits stay visible.
A `+` or `-` marks a path added or removed from that context, not necessarily a new or deleted
callee implementation.

Labels containing control characters are quoted and escaped onto one line (`\n`, `\t`, etc.).
Literal contents, call identities, and source ranges are not normalized or shortened.
This reduces presentation noise, not graph-building work; long lines and large diffs can still
exceed the Pi tool's output limits.

## Experimental repository service

The Linux/macOS service shares one watched index and immutable generations across clients:

```sh
stackscout request . '{"version":1,"op":"sync","paths":[]}'
stackscout request . '{"version":1,"op":"checkpoint","name":"session"}'
# After editing src/lib.rs:
stackscout request . '{"version":1,"op":"sync","paths":["src/lib.rs"]}'
stackscout request . '{"version":1,"op":"diff","name":"session"}'
```

`request` connects or starts a background owner; `serve <root>` runs one in the foreground.
Check the response's `error`, `instance`, generation, completeness, and watcher-health fields.
Watching is eventually consistent; explicit path sync acknowledges writes without waiting for
notifications. The service also provides bounded `callers`, `callees`, and `impact` queries,
parallel indexing, and a private Unix socket fallback for long paths. The Pi extension is not
connected to this service. See [the protocol and resource limits](docs/service.md).

## In-memory Rust API

```rust
let mut repo = stackscout::Repository::new()?;
repo.change("lib.rs", b"pub fn run() {}")?;
repo.checkpoint("session");
repo.change("lib.rs", b"pub fn run() { audit(); } fn audit() {}")?;
println!("{}", repo.diff("session")?);
```

`change` accepts repository-relative paths and source bytes; `delete` removes a file.
Neither reads the filesystem. An unchanged content hash and parser identity skip parsing
and graph rebuilding, including for cached syntax failures. Named checkpoints remain
queryable after later updates. `diff_with_diagnostics` includes unchanged issue details.

Updates parse only the changed file, but still rebuild resolution and all graph edges from
cached artifacts. Checkpoints clone artifacts, diagnostics, and graph, so retained memory
grows with the number of checkpoints. There is no persistent cache or public bulk-ingestion API yet.
`diff-dir` and the Pi extension still use one-shot directory comparisons. Service requests reuse
the resident core; initial scans and explicit batches rebuild the graph once per batch.

## What it does not do

StackScout is an early prototype. It does **not** currently:

- connect the bundled Pi extension to the resident service;
- provide a non-Unix service transport;
- automatically coordinate coding-agent sessions;
- provide VCS revision lookup;
- expose MCP, editor, or coding-agent integrations beyond the bundled Pi extension;
- avoid rescanning both directories on `diff-dir` or the current Pi tool's diff;
- support languages other than Rust and TypeScript; or
- provide compiler- or typechecker-complete call resolution.

Rust resolves explicit `use` bindings—including groups, aliases, and `crate`/`self`/`super`
paths—for free and associated-function calls. Bindings are scoped to their modules and blocks;
known local shadowing does not become an imported call edge. Conflicting known targets remain
ambiguous, while unexpanded globs and imports with unknown alternatives remain unresolved.
This assumes Rust 2018+ paths and conventional file layouts, not Cargo-aware crate boundaries;
re-export chains, `#[path]` modules, and inferred receiver types are not resolved.

The graph is conservative and syntactic. Dynamic dispatch, reflection, Rust macro expansion,
TypeScript type-driven method resolution, generated code, and calls outside the compared trees may
remain unresolved. Parse-failure transitions are reported as incomplete rather than presented as
trustworthy results. Unchanged failures are hidden by default; `--diagnostics` includes their full
details.

## Checks

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
bun test tests/pi_extension.test.ts
```

The Pi lifecycle tests use temporary source directories and a stub Pi API; they require Bun
but no npm dependency installation. They do not exercise Pi's loader or Cargo integration.

## Performance

[Resolver benchmark results and methodology](docs/performance.md) cover the indexed
lookup improvement. Run the manual scaling benchmark with:

```sh
cargo test --release resolve::bench::scaling -- --ignored --nocapture
```

## License

[MIT](LICENSE)
