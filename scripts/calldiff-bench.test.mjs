import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { existsSync, mkdtempSync, readFileSync, rmSync, symlinkSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { test } from "node:test";
import { compare, run, statistics } from "./calldiff-bench.mjs";
import { fixtures, validate } from "./calldiff-fixtures.mjs";

test("sample statistics retain observations and use arithmetic even medians", () => {
  assert.deepEqual(statistics([4, 1, 3, 2]), { samples: [4, 1, 3, 2], median: 2.5, p95: 4 });
  assert.equal(statistics([9, 1, 3]).median, 3);
  for (const values of [[], [NaN], [Infinity], [-1]]) assert.throws(() => statistics(values));
});

test("fixtures are deterministic, change one file, and include both languages/densities", () => {
  const cases = fixtures();
  assert.deepEqual(cases, fixtures());
  assert.equal(cases.length, 9);
  assert.equal(new Set(cases.map((f) => f.name)).size, cases.length);
  for (const fixture of cases) {
    assert.deepEqual(
      Object.keys(fixture.before).filter((path) => fixture.before[path] !== fixture.after[path]),
      [fixture.changed],
    );
    assert.equal(fixture.edges.after[0][0], fixture.root);
  }
  assert.equal(Object.keys(cases.find((f) => f.name === "rust-10000").before).length, 1000);
  assert.ok(cases.find((f) => f.name === "ts-1000-padded").before["m0.ts"].includes("pad_26"));
});

test("oracles reject missing/extra changes, wrong calls, errors and truncation", () => {
  const fixture = fixtures()[0];
  const response = {
    error: null,
    last_update_error: null,
    index_complete: true,
    analysis_issue_count: 0,
    truncated: false,
    diff: "  ts:file.ts::boot\n+   ts:file.ts::migrate\n",
  };
  validate(fixture, "stackscout", "diff", "after", response);
  for (const patch of [
    { diff: "" },
    { diff: response.diff + "+   ts:file.ts::unexpected\n" },
    { diff: "  ts:file.ts::boot\n    ts:file.ts::loadConfig\n+     ts:file.ts::migrate\n" },
    { error: "failed" },
    { truncated: true },
  ]) {
    assert.throws(() =>
      validate(fixture, "stackscout", "diff", "after", { ...response, ...patch }),
    );
  }
  const tree = {
    mode: "tree",
    trees: [
      {
        tree: {
          key: "boot",
          children: ["loadConfig", "migrate", "connect"].map((key) => ({ key, children: [] })),
        },
      },
    ],
  };
  validate(fixture, "calldiff", "tree", "after", tree);
  tree.trees[0].tree.children.pop();
  assert.throws(() => validate(fixture, "calldiff", "tree", "after", tree));
  const diff = {
    mode: "diff",
    trees: [
      {
        tree: {
          key: "boot",
          status: "same",
          children: [{ key: "migrate", status: "added", children: [] }],
        },
      },
    ],
  };
  validate(fixture, "calldiff", "diff", "after", diff);
  diff.trees[0].tree.children[0].status = "same";
  assert.throws(() => validate(fixture, "calldiff", "diff", "after", diff));
});

test("comparison rejects mismatched inputs, controls and failed runs", () => {
  const before = {
    status: "passed",
    schema: 1,
    environment: { platform: "test" },
    builds: { driver_sha256: "same" },
    cases: [
      {
        name: "fixture",
        input_sha256: "input",
        operations: {
          tree: {
            stackscout: { measured_ms: statistics([2, 2, 2]) },
            calldiff_api: { measured_ms: statistics([4, 4, 4]) },
          },
        },
      },
    ],
  };
  const after = structuredClone(before);
  after.cases[0].operations.tree.stackscout.measured_ms = statistics([1, 1, 1]);
  assert.ok(compare(before, after).includes("2.00x"));
  after.cases[0].input_sha256 = "changed";
  assert.throws(() => compare(before, after));
  after.cases[0].input_sha256 = "input";
  after.builds.driver_sha256 = "changed";
  assert.throws(() => compare(before, after));
  after.builds.driver_sha256 = "same";
  after.status = "failed";
  assert.throws(() => compare(before, after));
});

test("CLI entrypoints work through symlinks, including macOS /tmp", () => {
  const directory = mkdtempSync("/tmp/ss-cd-entry-");
  try {
    const alias = join(directory, "bench.mjs");
    symlinkSync(fileURLToPath(new URL("./calldiff-bench.mjs", import.meta.url)), alias);
    const result = spawnSync(process.execPath, [alias, "--case", "missing"], {
      encoding: "utf8",
      timeout: 10_000,
    });
    assert.equal(result.status, 1);
    assert.match(result.stderr, /no matching cases/);
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
});

test("invalid configuration fails before launching tools", async () => {
  await assert.rejects(run({ case: ["missing"] }), /no matching cases/);
  await assert.rejects(run({ samples: "0" }), /samples/);
  await assert.rejects(run({ warmups: "0" }), /warmups/);
});

test("real upstream fixture: warm API, CLI and resident service agree", {
  skip: !process.env.STACKSCOUT_CALLDIFF,
  timeout: 120_000,
}, async () => {
  const parent = mkdtempSync("/tmp/ss-cd-test-");
  try {
    const out = join(parent, "run");
    const options = {
      calldiff: process.env.STACKSCOUT_CALLDIFF,
      out,
      samples: "3",
      warmups: "1",
      case: ["upstream-free-calls"],
    };
    const result = await run(options);
    assert.equal(result.status, "passed");
    assert.ok(
      result.cases.every((c) => !existsSync(c.workspace)),
      "successful run leaked a workspace",
    );
    for (const op of Object.values(result.cases[0].operations)) {
      assert.equal(Object.keys(op).length, 3);
      for (const engine of Object.values(op)) assert.equal(engine.measured_ms.samples.length, 3);
    }
    const frozen = readFileSync(join(out, "stackscout-bin"));
    assert.equal(
      createHash("sha256").update(frozen).digest("hex"),
      result.builds.stackscout_sha256,
    );
    const saved = readFileSync(join(out, "results.json"), "utf8");
    await assert.rejects(run(options), /EEXIST/);
    assert.equal(readFileSync(join(out, "results.json"), "utf8"), saved);
    // Replace the supplied executable during its first launch. Later cases must use the frozen copy.
    const wrapper = join(parent, "replace-me");
    writeFileSync(
      wrapper,
      `#!/bin/sh\nprintf '#!/bin/sh\\nexit 73\\n' > "${wrapper}"\nexec "${join(out, "stackscout-bin")}" "$@"\n`,
      { mode: 0o700 },
    );
    const frozenRun = await run({
      ...options,
      stackscout: wrapper,
      out: join(parent, "frozen"),
      case: ["upstream-free-calls", "rust-100"],
      "no-cli": true,
    });
    assert.equal(frozenRun.status, "passed");
    assert.equal(readFileSync(wrapper, "utf8"), "#!/bin/sh\nexit 73\n");
    const bad = join(parent, "exits-before-readiness");
    writeFileSync(bad, "#!/bin/sh\nexit 73\n", { mode: 0o700 });
    const failedOut = join(parent, "failed");
    await assert.rejects(run({ ...options, out: failedOut, stackscout: bad }), /owner exited/);
    const failed = JSON.parse(readFileSync(join(failedOut, "results.json"), "utf8"));
    assert.equal(failed.status, "failed");
    assert.ok(
      failed.cases.every((c) => !existsSync(c.workspace)),
      "failed run leaked a workspace",
    );
  } finally {
    rmSync(parent, { recursive: true, force: true });
  }
});
