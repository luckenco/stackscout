# Rust productivity coverage audit

Audited **2026-09-07**, against **`ab51fae7`** (`main`), stackscout 0.1.0.

> Historical snapshot, not a current gap list. Later changes added cfg-qualified identities,
> safe foreign-item handling, trait default owners, path/raw-identifier normalization,
> and explicit opaque/deferred call boundaries. Check current code and tests before
> using the priorities below.

## Verdict

**No: stackscout does not cover all Rust syntax or the common semantic cases needed
for dependable, general-purpose Rust call/impact analysis.** It is useful as an
advisory graph for direct function calls in conventionally laid-out modules. It is
not yet a reliable answer to “who calls this method?” or “what can this change affect?”
across an ordinary idiomatic Rust project.

The biggest problems are not just unsupported syntax:

1. Valid conditional definitions can **fail construction of the whole graph**.
2. Some valid modern syntax **causes an entire file to be omitted**.
3. Methods, trait defaults, closures, and macro-contained code lose important edges.
4. Some calls receive **incorrect resolved edges**, not merely an unresolved label.
5. Changes to constants, types, macro definitions, and other semantic inputs can
   produce **an empty diff**.

Parsing successfully, `index_complete: true`, a passing test suite, and an empty
callers/diff result do **not** establish semantic completeness. The README already
warns about several categories; this report identifies concrete consequences and
additional cases.

## Method and verification

- Read Rust extraction, resolution, the shared model, discovery, graph construction,
  querying, diff/snapshot behavior, and existing Rust fixtures.
- Ran **59 small audit cases** through a temporary harness using the repository's
  unchanged source and locked dependency versions: `tree-sitter` 0.26.12 and
  `tree-sitter-rust` 0.24.2. Inspected extracted symbols, query edges, errors, and
  selected before/after diffs.
- Checked their source with **Rust 1.95.0, edition 2024, on macOS**:
  **69 of 70 compilation checks succeeded**. The remaining check deliberately
  referenced an undeclared module to test false resolution. Multiple snapshots
  and the lib/bin case account for more compilation checks than audit cases.
- Separately executed a method-dispatch example to confirm Rust's actual target
  where stackscout disagreed.
- Ran `cargo test`: **102 passed, 4 ignored, 0 failed**, including 52 directory-diff
  tests. Ignored: resolver timing, service performance, watcher drift, startup stress.
- `tokei`: Rust extraction + Rust resolution contain **706 code lines**. This is a
  deliberately small syntactic implementation, not a compiler-level semantic engine.

**Evidence labels:** **P** = reproduced in the temporary probes; **F** = existing
fixture/test; **S** = established by source inspection, not separately exercised.
The probes were not added to the regression suite; representative reproducers are
included below. No production code changed. Linux, other Rust versions/editions,
nightly syntax, exhaustive grammar fuzzing, and external-project benchmarks were
not tested. Unrelated JS/Pi checks were not run for this documentation-only audit.

## Productivity by use case

| Workflow | Assessment |
| --- | --- |
| Explore direct free-function calls in a small single crate | Useful, subject to module/import restrictions below. |
| Review changes to a function and its directly resolved callers | Useful advisory evidence; not a complete semantic diff. |
| Find callers of a method or refactor a type | Major gap: ordinary receivers remain unresolved. |
| Navigate traits, generic APIs, extension traits, or trait objects | Major gap: little dispatch/type information; trait defaults omitted. |
| Work in a Cargo workspace or a package with lib + binaries/tests | Major gap: no Cargo target/crate ownership model. |
| Review feature-gated or cross-platform implementations | Unsafe to assume completeness; valid alternatives can fail indexing. |
| Trace iterator callbacks, async tasks, handlers, framework macros | Major gap: missing closures/macros and inaccurate async ownership. |
| Determine impact of a constant, type, trait contract, or derive change | Not supported as general dependency/impact analysis. |
| Prove a function is unused, a refactor is safe, or a change has no impact | Not supported. Even a better static graph cannot prove every runtime behavior. |

## What already works

“Works” below means the stated syntactic case, not every semantically equivalent form.

| Case | Evidence and boundary |
| --- | --- |
| Free functions, recursion, direct calls and call-order changes | **F:** `rust-basic`, `recursion`, `reordered`; file-qualified identities and cycles are handled. |
| `async fn`, `const fn`, `unsafe fn`, `extern "C" fn` with bodies | **P:** symbols and explicit calls inside their bodies are extracted. Modifiers do not imply execution/ABI semantics. |
| Free-function turbofish: `leaf::<u8>()` | **P:** resolves to `leaf`. This does **not** establish support for `Type::<T>::method()`. |
| Simple inherent `Type::method`, `self.method`, `Self::method` | **P/F:** works when the owner/name matches the syntactic key; `self` lookup is restricted to the same file and lexical impl namespace. |
| Separate trait implementations with the same method name | **F:** `rust-trait-impls` keeps `<Thing as First>::work` separate from `<Thing as Second>::work`. |
| Simple explicit UFCS: `<S as T>::method(...)` | **P:** works for matching unqualified type/trait spellings. Qualified/generic forms do not generally work. |
| Inline modules, conventional file modules, `crate`, `self`, `super` paths | **F:** `rust-inline-module`, `rust-file-module`, `rust-use-imports`; depends on filename-derived module layout. |
| Explicit `use`, aliases, grouped imports, scoped imports | **F:** `rust-use-imports`, `rust-use-scope`, `rust-use-transition`; direct local paths only, not general import/re-export chasing. |
| Shadowing by parameters, local bindings, patterns and type parameters | **F/P:** uncertain calls avoid false links; includes `if let`, `while let`, `match`, `for`, and a `let ... else` scope probe. This is not function-pointer resolution. |
| `if`/`else if`/`else`, `match` and guards | **F:** `rust-branches` preserves explicit branch context. |
| Loops, let chains, labeled blocks, unsafe/const blocks, raw borrows, `?`, `.await` | **P:** tested forms parse and nested explicit calls are visited; control flow/desugared calls are not fully modeled. |
| Precise captures (`impl Trait + use<'a>`) and unsafe attributes | **P:** tested forms parse; function bodies remain analyzable. Attributes themselves are not generally part of function identity. |
| Receiver/literal text, ambiguity and failed-analysis reporting | **F:** literal fixtures, import/module collision fixtures, diagnostics fixtures. Unknown calls are retained **when extraction creates a call record**. |

Fixture names refer to [`test/fixtures`](../test/fixtures); their assertions are in
[`tests/diff_dir.rs`](../tests/diff_dir.rs).

## Missing or incorrect behavior

Priorities: **P0** blocks indexing of valid, ordinary code; **P1** breaks common
navigation/impact workflows or produces misleading evidence; **P2** improves
precision or extends the advertised scope. These are audit recommendations, not
existing project commitments.

### P0 — valid code can block or remove evidence

| Gap | Observed behavior | Evidence / implementation |
| --- | --- | --- |
| Mutually exclusive `#[cfg]` definitions | Two valid alternatives named `run` in the same file cause `duplicate symbol identity: rust:src/lib.rs::run`. Graph construction fails rather than representing alternatives. This can prevent initial publication or leave an update at the previous snapshot. | **P:** `cfg_duplicate`. Attributes are not evaluated by [`definitions`](../src/analyze/rust.rs#L137); duplicate keys are fatal in [`graph::build`](../src/graph.rs#L60); snapshot updates are transactional in [`repository.rs`](../src/repository.rs). |
| `safe fn` / `safe static` declarations in an `unsafe extern` block | Rust accepts the probe; Tree-sitter reports a syntax error. The entire file, including unrelated normal functions, is omitted. | **P:** `safe_foreign_items`. [`Analyzers::file`](../src/analyze/mod.rs#L96) rejects Rust trees containing parser errors; failed files are omitted from both comparison sides. |

These are distinct failure modes: graph construction returns an error; a parser
failure becomes an analysis issue with omitted evidence. Neither should be described
as merely an unresolved external call.

### P1 — common Rust call paths are missing or wrong

| Gap | Observed behavior / impact | Evidence |
| --- | --- | --- |
| Receiver type resolution | `fn run(s: &S) { s.leaf(); }` has an unresolved call despite an explicit parameter type and local implementation. Local variables, fields, chains, borrowed/boxed receivers and extension methods have the same missing type-information boundary. | **P:** typed receiver and chained-call cases; **S:** all ordinary `CallTarget::Method` values return unresolved in [`resolve/rust.rs`](../src/resolve/rust.rs#L14). |
| `self` across impl blocks/files or impl namespaces | An impl in another file cannot link `self.leaf()` / `Self::leaf()` to the original file. A trait impl calling an inherent helper is also unresolved. | **P:** `split_impl_self`, `self_trait_to_inherent`. Lookup uses only the caller's file and lexical impl prefix. |
| Incorrect inherent-vs-trait precedence | Inside `impl T for S`, `self.leaf()` and `Self::leaf(self)` resolve to `<S as T>::leaf` when Rust actually selects the inherent `S::leaf`. | **P:** `trait_inherent_precedence`; compiler execution confirmed both targets are inherent. This is a **false resolved edge**. |
| Trait declarations and default bodies | All trait items are skipped. Default methods, their calls, and required method declarations have no symbols; changing a default body can produce an empty diff. | **P:** `trait_default`, `trait_default_change_invisible`; [`definitions`](../src/analyze/rust.rs#L146) returns immediately for `trait_item`. |
| Trait dispatch and generic bounds | `T::leaf(s)`, `S::leaf(s)` for a trait method, and `s.leaf()` do not link to the implementation. `A::leaf()` for `A: T` is uncertain. Trait-object dispatch, blanket implementations and associated-type-driven dispatch have no semantic machinery. | **P:** simple trait/UFCS and generic-bound probes; **S:** dynamic/associated dispatch extensions. |
| Impl/type identity and path normalization | `impl inner::S` is stored with `inner::S` as one component, while a call splits it into two. `S::<u8>::new()` does not match `impl<T> S<T>`. `<S as traits::T>::leaf()` fails although simple `<S as T>::leaf()` works. Type aliases do not resolve. | **P:** `qualified_impl_type`, `generic_impl`, `trait_ufcs_qualified`, `alias_type`; raw impl text and string-split call paths in [`analyze/rust.rs`](../src/analyze/rust.rs#L178). |
| Closures, callbacks and function values | Closures have no symbols or body edges. Named closure calls and callback parameters are uncertain; passing a function item does not connect it to eventual invocation. Immediately invoked closures are omitted too. An edit inside a closure can still mark the **parent's shape** changed. | **P:** `closure_callback`, `async_closure`, `closure_edit_parent_shape`; **F:** `closure-ownership` deliberately prevents parent attribution. |
| Nested functions and local impls/modules | Local definitions inside a function are not extracted. A local function's invocation is uncertain and calls inside its body are lost. | **P:** `nested_items`; extraction returns after recording the enclosing function, and step collection skips nested owners. |
| Calls through nontrivial callee expressions | `(leaf)()` and `(*&fp)()` are omitted, not recorded as unknown calls. `factory()()` records `factory()` but omits invocation of its returned function. | **P:** `call_expression_wrappers`; [`call_target`](../src/analyze/rust.rs#L377) accepts only a small set of AST forms. |
| Macro input and expansion | Calls inside `println!("{}", leaf())`, `assert_eq!(leaf(), 1)` and custom macro invocations disappear. A macro-generated function is absent; a direct call to its name remains unresolved. Proc-macro/derive expansion is absent as well. | **P:** `macro_calls`, `macro_generated_function`; **S:** proc macros/derives. Token trees are not Rust expression ASTs and are not expanded. |
| Async ownership and execution | `let _unused = async { leaf(); };` attributes `leaf()` directly to the containing function even though the future is never polled. Closures are skipped, but async blocks are recursively flattened. No creation/poll/spawn distinction exists. | **P:** `async_blocks`; [`collect_steps`](../src/analyze/rust.rs#L230). |
| Globs and import/re-export chains | `use inner::*`, public re-exports consumed elsewhere, alias-to-import chains, and `super::alias::leaf()` do not resolve. A glob can even suppress a same-module function that would otherwise resolve. | **P:** `glob`, `glob_shadows_local_item`, `reexport`, `chained_import`, `super_parent_alias`; [`use_tree`](../src/analyze/rust.rs#L59), [`resolve`](../src/resolve/rust.rs#L6). |
| Cargo crate/target ownership | `src/lib.rs` and `src/main.rs` functions are merged into one lookup namespace: a valid binary-local call can become ambiguous with a library function. Under `crates/a/src/lib.rs`, `crate::leaf()` fails while bare `leaf()` works. | **P:** `lib_bin_collision`, `workspace_crate_paths`; [`module`](../src/resolve/rust.rs#L109) derives namespace solely from filenames and removes `src`; `crate::` clears all prefixes. |
| Actual Rust module membership | `#[path = "other.rs"] mod renamed` is not honored. `include!("generated.rs")` does not place functions into the including module. An undeclared `hidden.rs` can supply a falsely resolved `hidden::leaf()` call. | **P:** path/include cases and one intentionally compiler-invalid undeclared-module control. [`discover`](../src/discover.rs) walks source files without checking `mod` declarations. |
| Active configuration | `#[cfg(any())]` functions and their call edges are indexed despite being disabled. Test/production, platform and feature alternatives mix. No edition/target/features/build-script configuration is supplied to resolution. | **P:** inactive `cfg` case; **S:** broader configuration model absence. Cargo manifests are not source inputs in [`Syntax::for_path`](../src/model.rs#L13). |

The Cargo gap also applies to custom target paths, integration-test/example/bench
crate roots, dependency aliases, `extern crate`, and cross-workspace-crate calls
(**S**, not individually compiled as Cargo projects in this audit). A file walker
cannot establish those namespaces, visibility rules or dependency relationships.

### P1/P2 — semantic changes, precision, and diagnostics

| Gap | Observed behavior / consequence | Evidence / priority |
| --- | --- | --- |
| Non-callable semantic dependencies | Changing a struct field type, module constant, associated constant, trait default body, or macro definition produced an **empty diff** when indexed function text stayed unchanged. Structs/enums/unions, type aliases, trait contracts, fields and constants are not general dependency nodes. | **P** for the listed changes; **S** for the broader item family. **P1** for a general impact-analysis claim; otherwise explicitly out of scope. |
| Enclosing attributes and impl contracts | Adding `#[cfg(any())]` to a function, or changing `impl<T: A>` to `impl<T: B>` without changing the method body, produced an empty diff. Associated-type changes have the same missing enclosing-contract dependency. | **P** for `cfg`/bounds; **S** for associated types. **P1**. Function shape includes the function node, not all enclosing/sibling semantic inputs. |
| Const/static initializer execution | `const A: u8 = leaf(); static B: u8 = leaf();` creates no initializer symbols or edges to `leaf`. Module-level initialization/const evaluation is outside the function-body graph. | **P:** `const_static_initializers`. **P2**, or **P1** if claiming non-function impact coverage. |
| Implicit/desugared calls | `S + S`, `s[0]`, and destruction do not link to local `Add::add`, `Index::index`, or `Drop::drop`. `Deref`, iterator protocol, `Future::poll`, and `?`/conversion machinery are likewise not modeled. | **P** for operators/index/drop; **S** for other desugarings. **P2** semantic extension, important for completeness claims. |
| Raw identifier normalization | `fn r#work() {}` resolves from `r#work()` but not the equivalent `work()`. | **P:** `raw_identifiers`. **P2**, small syntactic fix. |
| Whitespace-sensitive path/impl identity | `inner :: leaf()` is unresolved while `inner::leaf()` resolves. Formatting `impl S<T>` as `impl S < T >` produces method removal/addition. | **P:** `qualified_path_format`, `generic_impl_format_noise`. **P2**, avoidable syntax noise. |
| Rust visibility/export roots | Any visibility modifier is treated as public, including `pub(crate)` / `pub(super)`. Effective visibility through containing modules/re-exports is not computed. Trait impl methods lack a visibility modifier and therefore appear private in this model. | **P** that `pub(crate)` parses/resolves; **S** for visibility/root consequences in [`definitions`](../src/analyze/rust.rs#L169), [`Visibility`](../src/model.rs#L142), and [`diff.rs`](../src/diff.rs). **P2**. |
| Constructors versus calls | `S(1)`, `Some(1)` and `Ok(())` become unresolved call records without data-constructor classification. This adds noise to any “unresolved call” coverage measurement. | **P:** tuple constructor and `?` probes. **P2**. |
| FFI/dependency boundaries | Ordinary foreign declarations have no symbols; their invocations remain unresolved. External crates, generated/ignored build output, and assembly have no connected semantic graph. | **P:** ordinary foreign declaration; **S:** dependency/build/assembly behavior. **P2**, usually classify as external/opaque rather than pretending to know implementations. |
| Execution/control-flow precision | `outer(inner())` records `outer` before `inner`; loops are flattened rather than given loop context. Short-circuiting, early returns, polling and reachability are not an execution model. | **P:** nested-call and loop probes; **S:** full CFG absence. **P2**; document source traversal order, not runtime call order. |
| Coverage diagnostics | Skipped traits/closures/macros and unsupported callee-expression forms generally produce no specific analysis issue. There is a generic syntactic warning, but no per-file “omitted N closures/macros/call forms” evidence. | **P/S:** successful probe artifacts with missing symbols/edges; [`query.rs`](../src/query.rs#L130), [`service/mod.rs`](../src/service/mod.rs#L71). **P1** for trustworthy interpretation. |

## Representative reproducers

These are minimal standalone sources unless noted. To check syntax/types without
linking external declarations:

```sh
rustc --edition=2024 --crate-type=lib --emit=metadata -A warnings lib.rs
```

For diff reproducers, put the source in `before/lib.rs` and `after/lib.rs`, make only
the stated change, then run:

```sh
cargo build
target/debug/stackscout diff-dir --diagnostics before after
```

For graph evidence, index a directory containing the source and query `run`:

```sh
target/debug/stackscout request probe '{"version":1,"op":"sync","paths":[]}'
target/debug/stackscout request probe '{"version":1,"op":"callees","symbol":"run"}'
```

Inspect JSON `error` and analysis issues, not only the CLI exit status. Existing
service owners do not reload rebuilt binaries; use a fresh probe directory.

### Valid `cfg` alternatives fail graph construction

```rust
#[cfg(unix)]
pub fn run() {}
#[cfg(not(unix))]
pub fn run() {}
```

Compiler: accepted. Stackscout: duplicate symbol identity, including when comparing
the directory with itself. Preserve the duplicate-key safety invariant when fixing
this; model conditional definitions or select a configuration instead of dropping
one arbitrarily.

### Valid foreign syntax omits the whole file

```rust
unsafe extern "C" {
    pub safe fn foreign();
    pub safe static FOREIGN_VALUE: u8;
}
pub fn run() { foreign(); }
```

Compiler: accepted. Stackscout: syntax issue; even `run` is absent.

### Ordinary typed receiver loses a known local target

```rust
pub struct S;
impl S { pub fn leaf(&self) {} }
pub fn run(s: &S) { s.leaf(); }
```

Stackscout: unresolved `s::leaf`; no incoming edge to `S::leaf`.

### A resolved target can be wrong

```rust
pub trait T {
    fn leaf(&self);
    fn run(&self);
}
pub struct S;
impl S { fn leaf(&self) {} }
impl T for S {
    fn leaf(&self) {}
    fn run(&self) { self.leaf(); Self::leaf(self); }
}
```

Rust selects inherent `S::leaf` for both calls. Stackscout selects
`<S as T>::leaf` for both. Query `<S as T>::run`, not the ambiguous short name `leaf`.

### Calls inside a ubiquitous macro disappear

```rust
fn leaf() -> u8 { 1 }
pub fn run() { println!("{}", leaf()); }
```

Stackscout: `run` has no call records. Changing `leaf` does not establish an impact
path to `run`. Blindly treating arbitrary macro tokens as executed Rust would not
be a sound fix; macros decide whether/how their inputs are evaluated.

### Behavior-changing edits can produce an empty diff

```rust
const VALUE: u8 = 1;
pub fn run() -> u8 { VALUE }
```

Change only `1` to `2`: the observed diff is empty, including with `--diagnostics`.
The same empty result was reproduced for a trait default-body edit and a
macro-definition-only edit.

### Formatting can break a previously resolved call

```rust
mod inner { pub fn leaf() {} }
pub fn run() { inner::leaf(); }
```

Change only the call to `inner :: leaf()`: its target changes from resolved to
unresolved despite equivalent Rust semantics.

## Existing tests do not establish broad Rust coverage

Important strengths: the suite protects ambiguity, source identity, lexical
shadowing, failure filtering, deterministic output, recursion and diff propagation.
Those guarantees should remain intact.

Important limits:

- `rust-trait-impls` tests distinct implementation identities, **not trait dispatch**.
- `rust-use-scope` deliberately checks uncertain/unresolved receivers and generic
  bindings; that does not demonstrate productive method navigation.
- `closure-ownership` protects against falsely attributing deferred calls to the
  parent; it does **not** provide closure symbols or callback edges.
- `stackscout_can_analyze_itself` checks that a self-comparison has no diff. Equal
  omissions on both sides satisfy that assertion.
- Snapshot expectations describe current behavior, including limitations. They are
  not compiler-derived completeness or correctness oracles.

A fresh source-only audit of stackscout itself extracted **211 symbols** and queried
**2,349 call records**: 347 resolved, 1,995 unresolved and 7 uncertain. The scan included
all `src/**/*.rs`, including test modules, without selecting a Cargo configuration.
This is **not a Rust coverage percentage**: standard-library calls and constructors
contribute to unresolved records, while omitted calls never enter the denominator. It does show
why “parses itself” is much weaker than “can navigate its own call paths.”

The live `call_graph` query for `rust:src/repository.rs::Repository::change` returned
no callers, despite calls such as `repo.change(...)` in
[`src/repository/tests.rs`](../src/repository/tests.rs#L122). The source-only probe
also classified receiver calls as unresolved. The live repository index included
11 deliberately malformed fixture-file issues; those are separate from the method
resolution gap.

## Recommended implementation order and acceptance tests

### 1. Stop avoidable failures and misleading certainty

- Handle valid `cfg` alternatives without losing duplicate-identity protection or
  failing the entire root. Make configuration/alternative policy explicit.
- Fix/update parser support for safe foreign items. Keep real syntax failures
  explicit; do not blanket-ignore parser errors.
- Normalize AST path components and raw identifiers; make impl identity insensitive
  to whitespace. Do not erase generic specializations into colliding keys.
- Until dispatch can be established, prefer uncertain/unresolved over the currently
  wrong inherent-vs-trait `self` edge.
- Expose per-file unsupported/omitted categories, separate from parse failures and
  transport/index readiness.

Acceptance fixtures: `cfg` alternatives, safe foreign items, both equivalent path
spellings, qualified impl/UFCS paths, raw identifiers, and inherent/trait collisions.
Check **the target identity and absence of false edges**, not just nonempty output.

### 2. Establish real crate/module/import ownership

- Accept Cargo-derived crate roots, target membership, editions, dependencies and
  selected configuration as plain inputs to resolution; keep Cargo I/O at the edge.
- Honor module declarations and `#[path]`; represent included/generated sources and
  unresolved expansion boundaries explicitly.
- Resolve explicit alias/re-export chains and bounded local globs using actual
  namespaces and shadowing rules. Preserve ambiguity and unknown alternatives.

Acceptance fixtures: a workspace with two crates, a lib plus binary sharing helper
names, integration-test/custom roots, renamed path modules, include boundaries,
re-export chains and a glob beside a same-module function. Verify both false-negative
and false-positive cases.

### 3. Make methods, traits and deferred callables useful

- Represent trait declarations/defaults and nested functions/closures with independent
  ownership. Do not “fix” missing bodies by attributing every nested call to its parent.
- Resolve the easy explicit-type cases first; correctly handle split impls and `Self`.
- Preserve calls through unsupported expressions as opaque evidence; normalize simple
  parentheses safely.
- Model async blocks as deferred owners; distinguish a body dependency from immediate
  execution. Add function-value/callback evidence without claiming arbitrary dataflow
  is statically knowable.

Acceptance fixtures: typed receivers, split impls, trait defaults/overrides, generic
bounds, local helpers, function pointers, invoked/uninvoked closures, iterator
callbacks, and created/awaited/spawned futures.

For broad generic/trait/macro accuracy, evaluate a **rust-analyzer or compiler-backed
semantic input** rather than growing a second Rust type checker inside the string
resolver. Keep the current syntax-only mode usable, with explicit uncertainty.

### 4. Decide and document the semantic-diff boundary

Either add dependency evidence for constants, types, associated items, attributes,
impl bounds and macro definitions, or prominently state that their impact is not
covered and surface their changes separately. Treat external/generated/implicit
calls as explicit boundaries when no semantic provider is available.

Acceptance fixtures: constant-only, struct-field-only, associated-type-only,
trait-default-only, impl-bound-only, attribute-only and macro-definition-only changes.
Each must produce useful evidence or a clear unsupported-change signal—not silent
assurance from an empty result.

## Using stackscout safely today

Use it to discover **some** direct call paths and inspect local function changes.
Pair it with source search, rust-analyzer navigation, and the relevant Cargo
checks/tests. Treat unresolved calls, skipped owners, macro boundaries and active
configuration as missing evidence. Do not use an empty graph/diff to declare code
unused, approve a Rust refactor, or conclude that a behavior change is isolated.
