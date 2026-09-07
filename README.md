# stackscout

Experimental Rust/TypeScript call graphs and semantic diffs for coding agents.
Inspired by [call-stack workflows](https://x.com/dillon_mulroy/status/2069852516309762411?s=20)
and [FFF's resident index](https://github.com/dmtrKovalenko/fff).

## Directory diff

```sh
cargo build
target/debug/stackscout diff-dir <before-directory> <after-directory>
```

`~` marks local changes; `+`/`-` mark added/removed call paths, not necessarily implementations.
Failed-analysis files are omitted from both sides; `--diagnostics` includes unchanged failures.

## Repository service

Linux/macOS clients share one watched index per canonical root.
`request` starts/connects to an owner; `serve <root>` runs one in the foreground;
`connect <root>` returns `{version, root, socket}` for direct clients.

```sh
target/debug/stackscout request . '{"version":1,"op":"sync","paths":[]}'
target/debug/stackscout request . '{"version":1,"op":"checkpoint","name":"session"}'
# After editing:
target/debug/stackscout request . '{"version":1,"op":"sync","paths":["src/lib.rs"]}'
target/debug/stackscout request . '{"version":1,"op":"diff","name":"session"}'
```

Read [service contracts](docs/service.md) before integration: check JSON `error` even on CLI success.

## Pi

Build first, then start a new Pi session here. The extension uses `target/debug/stackscout`
(or `STACKSCOUT_BIN`) for startup/discovery, then direct sockets. Existing owners do not reload binaries.

- `call_graph`: callers, callees, or impact by unique name or `language:file::qualified::name`.
- `call_graph_diff`: aggregate changes since the immutable session checkpoint, not authorship.
- `call_graph_review`: evidence for explicit file/symbol scopes since their last acknowledgement.
- `call_graph_ack`: acknowledge an inspected review token; later writes remain visible.

Use `refresh: true` after Bash/external writes; successful edit/write tools await sync.
Watching is eventually consistent. Restarting the owner loses checkpoints; Pi never silently recaptures them.
Cancellation closes client requests, not the owner. Truncated output cannot prove absence of changes.

## Rust API and limitations

`Repository` accepts relative paths and supplied bytes; see its [doctests](src/repository.rs) or `cargo doc --open`.
Tree-sitter supports `.rs`, `.ts`, `.tsx`, `.mts`, `.cts`; declarations and hidden directories are skipped.
Receivers, generics, globs/re-exports, closures, macros, dynamic/external/generated code may be unresolved
or omitted. Test/production configurations mix. Missing edges prove nothing: inspect source and run tests.
See the [Rust coverage audit](docs/rust-coverage.md) for verified gaps and recommended priorities.

## Checks

```sh
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
bun test tests/pi_extension.test.ts tests/pi_live.test.ts
node --test scripts/*.test.mjs tests/transport.test.ts
```

Use Node 24+. Live tests require installed Pi (or `STACKSCOUT_PI_PACKAGE`) with matching `pi-ai`;
missing Pi explicitly skips live tests. Service changes require Linux and macOS checks.

[MIT License](LICENSE)
