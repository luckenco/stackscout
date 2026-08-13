# Project shape

- Keep one Rust package until a component needs an independent build or release lifecycle.
- Organize modules by responsibility, not one module per type.
- Keep the core independent of CLI, filesystem watching, VCS, transport, and presentation.
- Keep I/O at the edges. Core functions should accept plain data and return plain data or
  explicit errors.
- Keep dependencies one-way: extraction → resolution → graph → diff → rendering.
- Do not use global mutable state or hidden singletons. Pass state and dependencies
  explicitly.
- Prefer concrete types and functions. Add a trait only when multiple implementations exist or
  it protects a meaningful boundary.
- Default to private or `pub(crate)` APIs; expose only deliberate contracts.
- Preserve unresolved and ambiguous calls explicitly rather than guessing.
- Make symbol identity and graph ordering deterministic; sort at output boundaries when
  necessary.
- Avoid async, background tasks, caches, persistence, and plugin systems until their need is
  demonstrated.
- Prefer small fixture-based behavioral tests over mocks, builders, and test-only
  abstractions.

# Dogfooding

- After Rust or TypeScript implementation edits that may affect the call graph, usually run
  `call_graph_diff` once near the end.
- Check for unintended impact. Report missing, noisy, or wrong results as dogfooding findings.

# Rust

- Keep things modular.
- Define clear, testable contract boundaries. Document public contracts, and ensure Rustdoc examples compile and run as doctests.
- Use assertions for invariants when encoding within types is not feasible.
- Use clippy and fix code smells reported.
- Prefer iterators when they are clearer; use explicit control flow when it is easier to read.
- Make invalid state unrepresentable.
- Prefer importing modules over long lists of types. Using `module::Type` at call sites is fine, keeps names unambiguous, and reduces namespace-collision churn.
- Be wary of designs that will not scale. Report the limiting assumption, expected failure mode, and likely impact to the user.
