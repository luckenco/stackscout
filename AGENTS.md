# Design

- Keep one Rust package until a component needs independent builds or releases; group modules by responsibility.
- Keep I/O and scheduling at the edges. Core functions take plain data and return data or explicit errors, independent of CLI, watching, VCS, and transport.
- Keep dependencies one-way: extraction → resolution → graph → diff → rendering.
- Pass state explicitly. Prefer concrete functions/types; add traits for multiple implementations or meaningful boundaries.
- Default to private or `pub(crate)` APIs. Document public contracts with runnable doctests.
- Preserve unresolved and ambiguous calls; keep symbol identity and output ordering deterministic.
- Encode invariants in types, otherwise assert them. Prefer module-qualified imports over long type-import lists.
- Add concurrency, caching, persistence, or plugins only for demonstrated needs. Report scaling assumptions, failure modes, and impact.
- Prefer small behavioral fixtures over mocks and test-only abstractions.

# Verification

- For code changes, run the [README checks](README.md#checks); fix Clippy findings.
- After call-affecting Rust/TypeScript edits, usually run `call_graph_diff` once near the end. Inspect unintended impact; report missing, noisy, or wrong results. Skip docs-only changes.
- For service work, read [protocol, consistency, and resource contracts](docs/service.md) and verify on Linux and macOS.
