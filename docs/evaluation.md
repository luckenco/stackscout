# Workflow evaluation

Run when evaluating whether StackScout changes agent decisions or inspection cost. This is
an opt-in real-model pilot, separate from scripted SDK acceptance tests. It consumes the
selected provider's configured credentials/quota and runs model-chosen shell commands in
local disposable workspaces; it is not a sandbox. Use a trusted model/environment.

## Run

```sh
cargo build --release
node --test scripts/workflow.test.mjs
node scripts/workflow.mjs /path/to/pi-coding-agent openai-codex <model-id> /tmp/ss-trial-new 2
```

Requires Node with native TypeScript support (tested on 24), installed Pi, and authenticated
model access. `STACKSCOUT_BIN` overrides the release binary; `STACKSCOUT_EVAL_THINKING` defaults
to `low`. The output directory must not exist. Global extensions/skills/context are disabled;
only the repository adapter is loaded in treatment. Authentication stays in Pi's normal store.
After a recorded trial failure, rerun the same command with `STACKSCOUT_EVAL_RESUME=1`: settings
must match, recorded trials (including failures) are preserved, and only unattempted trials run.
Hard-interrupted, unrecorded trials cannot be resumed; retain their evidence and use a new output
directory rather than overwriting the incomplete trial.

## Design and completion criteria

- Default: two repetitions × three scenarios × control/treatment = 12 trials, counterbalanced order.
  Initial file contents, prompts, model/thinking, built-in tools, budgets, and evaluator match.
  Treatment adds graph tools plus narrow-review guidance. Provider sampling/cache/load are
  uncontrolled; retain both repeated-token totals and cache-read usage.
- Small synthetic TypeScript checkout: single-agent feature change; two-agent relevant shipping
  contract change plus unrelated telemetry edit; two-agent unrelated-only edit. The reviewer
  prepares read-only, then an independent real Pi writer makes exact payload edits between reviewer
  turns. This controlled interleaving is reproducible, not simultaneous model execution or an
  open-ended real-repository task. Assert writer payloads and preserve them through review.
- Reviewer implements discounts, checks smoke tests and a patch, and reports relevant concurrent
  files. An external seven-case evaluator checks values, API compatibility, discount clamping,
  shipping adaptation, preserved files, and unexpected edits. Inspect every final patch and
  report against traces before drawing a correctness conclusion; tests are not the whole gate.
- Each prompt has a three-minute deadline; each agent has 50 tool calls. Errors stop the suite
  with partial results saved. Never discard failed trials from the denominator.

`summary.json` stores per-agent provider-reported model tokens (including repeated context and
cache reads), serialized tool-content bytes, read/Bash counts after preparation, graph calls, wall/startup
latency, and false/missed alerts. Tool tokens are part of model input; there is no exact separate
provider count, so bytes are reported rather than relabeled as tokens. Bash counts include tests
and patch inspection; trace review distinguishes them from source reads. Detection latency is
writer-completion to the first successful `write`-tool shipping alert, not inferred model awareness.
Other report channels and single/unrelated trials have no measured detection time.

Retain `events.jsonl`, final sources, patches, and settings with the summary. Full SDK journals
are local evidence; review before publishing, as real tasks may contain sensitive source.

Gate: lower paired inspection cost or better detection, with no correctness loss. This pilot
can falsify an easy usefulness claim or identify a workflow problem; two repeats of one small
fixture cannot establish general benefit. Follow promising results with matched real Rust/TS
repository tasks, including less explicit concurrent edits. Results live in [performance](performance.md).
