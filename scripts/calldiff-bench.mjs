import assert from "node:assert/strict";
import { fork, spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import {
  closeSync,
  existsSync,
  lstatSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  readdirSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { cpus, release } from "node:os";
import { createRequire } from "node:module";
import { dirname, join, resolve } from "node:path";
import { performance } from "node:perf_hooks";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseArgs } from "node:util";
import { exchange, Unavailable } from "../.pi/transport.ts";
import { fixtures, validate } from "./calldiff-fixtures.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const sha = (value) => createHash("sha256").update(value).digest("hex");
const sleep = (ms) => new Promise((done) => setTimeout(done, ms));
// calldiff requires Git. Only disposable Git fixtures and read-only competitor metadata use it.
const env = Object.fromEntries(
  Object.entries(process.env).filter(([key]) => !key.startsWith("GIT_")),
);
Object.assign(env, {
  GIT_CONFIG_GLOBAL: "/dev/null",
  GIT_CONFIG_NOSYSTEM: "1",
  GIT_TERMINAL_PROMPT: "0",
  GIT_OPTIONAL_LOCKS: "0",
  NO_COLOR: "1",
});

function command(binary, args, cwd) {
  const result = spawnSync(binary, args, {
    cwd,
    env,
    encoding: "utf8",
    timeout: 120_000,
    maxBuffer: 16 * 1024 * 1024,
  });
  assert.equal(result.status, 0, `${binary} ${args.join(" ")}: ${result.error ?? result.stderr}`);
  return result;
}

export function statistics(samples) {
  assert.ok(samples.length > 0 && samples.every((value) => Number.isFinite(value) && value >= 0));
  const sorted = [...samples].sort((a, b) => a - b);
  const middle = Math.floor(sorted.length / 2);
  let median = sorted[middle];
  if (sorted.length % 2 === 0) median = (sorted[middle - 1] + median) / 2;
  return { samples, median, p95: sorted[Math.ceil(sorted.length * 0.95) - 1] };
}

function treeHash(directory) {
  const hash = createHash("sha256");
  function visit(path, prefix) {
    for (const entry of readdirSync(path, { withFileTypes: true }).sort((a, b) =>
      a.name.localeCompare(b.name),
    )) {
      const name = `${prefix}${entry.name}`;
      if (entry.isDirectory()) visit(join(path, entry.name), `${name}/`);
      else {
        assert.ok(entry.isFile(), `not a regular file: ${name}`);
        hash
          .update(name + "\0")
          .update(readFileSync(join(path, entry.name)))
          .update("\0");
      }
    }
  }
  visit(directory, "");
  return hash.digest("hex");
}

function message(child, request) {
  return new Promise((done, reject) => {
    const timer = setTimeout(() => finish(new Error("calldiff worker timed out")), 120_000);
    function finish(error, value) {
      clearTimeout(timer);
      child.off("message", received);
      child.off("exit", exited);
      child.off("error", failed);
      if (error) reject(error);
      else done(value);
    }
    const received = (value) => {
      if (value.error) finish(new Error(value.error));
      else finish(null, value);
    };
    const exited = (code, signal) => finish(new Error(`calldiff worker exited: ${code}/${signal}`));
    const failed = (error) => finish(error);
    child.once("message", received);
    child.once("exit", exited);
    child.once("error", failed);
    if (request)
      child.send(request, (error) => {
        if (error) finish(error);
      });
  });
}

export function compare(before, after) {
  assert.equal(before.status, "passed");
  assert.equal(after.status, "passed");
  assert.equal(before.schema, after.schema);
  for (const key of [
    "platform",
    "arch",
    "release",
    "cpu",
    "cpu_count",
    "node",
    "node_flags",
    "node_options",
    "threads",
    "samples",
    "warmups",
  ]) {
    assert.equal(before.environment[key], after.environment[key], `different ${key}`);
  }
  for (const key of [
    "driver_sha256",
    "fixtures_sha256",
    "calldiff_dist_sha256",
    "calldiff_lock_sha256",
  ]) {
    assert.equal(before.builds[key], after.builds[key], `different ${key}`);
  }
  assert.deepEqual(
    before.cases.map((c) => c.name),
    after.cases.map((c) => c.name),
  );
  const lines = [
    "| Case / operation | StackScout before → after (ms) | Speedup | calldiff API before → after (ms) |",
    "| --- | ---: | ---: | ---: |",
  ];
  for (let i = 0; i < before.cases.length; i++) {
    const a = before.cases[i],
      b = after.cases[i];
    assert.equal(a.input_sha256, b.input_sha256, `different input: ${a.name}`);
    assert.deepEqual(
      a.startup?.calldiff_native?.map((m) => m.sha256).sort(),
      b.startup?.calldiff_native?.map((m) => m.sha256).sort(),
      "different calldiff native modules",
    );
    assert.deepEqual(Object.keys(a.operations), Object.keys(b.operations));
    for (const op of Object.keys(a.operations)) {
      assert.deepEqual(Object.keys(a.operations[op]), Object.keys(b.operations[op]));
      const x = a.operations[op].stackscout.measured_ms.median,
        y = b.operations[op].stackscout.measured_ms.median;
      const c = a.operations[op].calldiff_api.measured_ms.median,
        d = b.operations[op].calldiff_api.measured_ms.median;
      lines.push(
        `| ${a.name} / ${op} | ${x.toFixed(3)} → ${y.toFixed(3)} | ${(x / y).toFixed(2)}x | ${c.toFixed(3)} → ${d.toFixed(3)} |`,
      );
    }
  }
  return lines.join("\n");
}

async function stop(child) {
  if (!child.pid || child.exitCode !== null || child.signalCode !== null) return;
  await new Promise((done) => {
    const timer = setTimeout(() => child.kill("SIGKILL"), 2000);
    child.once("exit", () => {
      clearTimeout(timer);
      done();
    });
    child.kill("SIGTERM");
  });
}

async function worker(path) {
  const { runDiff, runTree } = await import(pathToFileURL(join(path, "dist/run.js")));
  process.on("message", ({ operation, options }) => {
    if (operation === "metadata") {
      const native = Object.keys(createRequire(import.meta.url).cache)
        .filter((file) => file.endsWith(".node"))
        .sort()
        .map((file) => ({ file, sha256: sha(readFileSync(file)) }));
      process.send({ native });
      return;
    }
    const warnings = [],
      log = console.error;
    console.error = (...args) => {
      warnings.push(args.join(" "));
      log(...args);
    };
    try {
      const start = performance.now();
      let result;
      if (operation === "diff") result = runDiff(options);
      else {
        assert.equal(operation, "tree");
        result = runTree(options);
      }
      const ms = performance.now() - start;
      process.send({ result, ms, warnings });
    } catch (error) {
      process.send({ error: error.stack });
    } finally {
      console.error = log;
    }
  });
  process.send({ ready: true });
}

export async function run(options) {
  const selected = fixtures().filter((f) => !options.case || options.case.includes(f.name));
  assert.ok(selected.length, "no matching cases");
  if (options.case) assert.equal(new Set(options.case).size, selected.length, "unknown case");
  const samples = Number(options.samples ?? 9),
    warmups = Number(options.warmups ?? 2);
  assert.ok(Number.isInteger(samples) && samples >= 3, "samples must be an integer >= 3");
  assert.ok(Number.isInteger(warmups) && warmups >= 1, "warmups must be an integer >= 1");
  const binary = resolve(
    options.stackscout ?? process.env.STACKSCOUT_BIN ?? join(here, "../target/release/stackscout"),
  );
  const calldiff = realpathSync(resolve(options.calldiff ?? join(here, "../../calldiff")));
  assert.ok(options.out, "--out must name a new directory");
  const out = resolve(options.out);
  const executable = join(out, "stackscout-bin");
  const info = statSync(binary);
  assert.ok(info.isFile() && info.mode & 0o111, "StackScout binary must be executable");
  const binaryBytes = readFileSync(binary);
  const report = {
    schema: 1,
    status: "running",
    started: new Date().toISOString(),
    environment: {
      platform: process.platform,
      arch: process.arch,
      release: release(),
      cpu: cpus()[0]?.model,
      cpu_count: cpus().length,
      node: process.version,
      node_flags: JSON.stringify(process.execArgv),
      node_options: process.env.NODE_OPTIONS ?? "",
      samples,
      warmups,
      threads: 4,
    },
    builds: {
      stackscout_sha256: sha(binaryBytes),
      calldiff_revision: command("git", ["rev-parse", "HEAD"], calldiff).stdout.trim(),
      calldiff_dirty: command("git", ["status", "--porcelain"], calldiff).stdout,
      calldiff_dist_sha256: treeHash(join(calldiff, "dist")),
      calldiff_lock_sha256: sha(readFileSync(join(calldiff, "package-lock.json"))),
      driver_sha256: sha(readFileSync(fileURLToPath(import.meta.url))),
      fixtures_sha256: sha(readFileSync(join(here, "calldiff-fixtures.mjs"))),
      transport_sha256: sha(readFileSync(join(here, "../.pi/transport.ts"))),
    },
    cases: [],
  };
  // Exclusive creation: a rerun must never overwrite another experiment's evidence.
  mkdirSync(out);
  // Freeze exactly the bytes we hashed; other agents may rebuild the supplied path during a run.
  writeFileSync(executable, binaryBytes, { mode: 0o700 });
  const save = () =>
    writeFileSync(join(out, "results.json"), JSON.stringify(report, null, 2) + "\n");
  writeFileSync(join(out, "inputs.json"), JSON.stringify(selected, null, 2) + "\n");
  save();
  const children = new Set(),
    abort = new AbortController();
  const interrupt = () => {
    abort.abort(new Error("benchmark interrupted"));
    for (const child of children) child.kill("SIGTERM");
  };
  process.once("SIGINT", interrupt);
  process.once("SIGTERM", interrupt);
  try {
    for (const fixture of selected) {
      abort.signal.throwIfAborted();
      const root = realpathSync(mkdtempSync("/tmp/ss-cd-"));
      const result = {
        name: fixture.name,
        workspace: root,
        files: Object.keys(fixture.before).length,
        input_sha256: sha(JSON.stringify(fixture)),
        startup: {},
        operations: {},
        evidence: {},
      };
      report.cases.push(result);
      const owned = [];
      function launch(kind) {
        const fd = openSync(join(out, `${fixture.name}-${kind}.log`), "wx");
        let child;
        try {
          if (kind === "stackscout")
            child = spawn(executable, ["serve", root], {
              env: { ...env, STACKSCOUT_WATCH: "0", STACKSCOUT_INDEX_THREADS: "4" },
              stdio: ["ignore", fd, fd],
            });
          else
            child = fork(fileURLToPath(import.meta.url), ["--worker", calldiff], {
              env,
              stdio: ["ignore", fd, fd, "ipc"],
            });
        } finally {
          closeSync(fd);
        }
        child.on("error", (error) => {
          child.failure = error;
        });
        owned.push(child);
        children.add(child);
        return child;
      }
      const endpoint = { root, socket: join(root, ".stackscout/service.sock") };
      const request = (operation) => exchange(endpoint, operation, abort.signal);
      function healthy(value) {
        assert.equal(value.version, 1);
        assert.equal(value.root, root);
        assert.equal(value.error, null);
        assert.equal(value.last_update_error, null);
        assert.equal(value.index_complete, true);
        assert.equal(value.analysis_issue_count, 0);
        assert.equal(value.watching, false);
      }
      try {
        for (const [path, text] of Object.entries(fixture.before))
          writeFileSync(join(root, path), text);
        writeFileSync(join(root, ".gitignore"), ".stackscout/\n");
        const git = (args) => command("git", args, root).stdout.trim();
        git(["init", "-q"]);
        git(["config", "user.name", "Benchmark"]);
        git(["config", "user.email", "bench@example.invalid"]);
        git(["config", "commit.gpgsign", "false"]);
        git(["config", "core.hooksPath", "/dev/null"]);
        git(["add", "."]);
        git(["commit", "-qm", "before"]);
        let start = performance.now();
        const owner = launch("stackscout");
        let initial;
        while (!initial) {
          if (owner.failure) throw owner.failure;
          assert.ok(
            owner.exitCode === null && owner.signalCode === null,
            "owner exited before readiness",
          );
          assert.ok(performance.now() - start < 120_000, "owner startup timed out");
          // Our foreground owner binds before chmod. Do not send through a half-created endpoint.
          try {
            const socket = lstatSync(endpoint.socket);
            assert.ok(socket.isSocket(), "unexpected endpoint type");
            if ((socket.mode & 0o777) !== 0o600) {
              await sleep(1);
              continue;
            }
          } catch (error) {
            if (error.code !== "ENOENT") throw error;
            await sleep(1);
            continue;
          }
          try {
            initial = await request({ op: "sync", paths: [] });
          } catch (error) {
            if (!(error instanceof Unavailable)) throw error;
            await sleep(1);
          }
        }
        result.startup.stackscout_owner_ready_ms = performance.now() - start;
        healthy(initial);
        assert.equal(initial.files_parsed, result.files);
        const instance = initial.instance;
        const checkpoint = await request({ op: "checkpoint", name: "before" });
        healthy(checkpoint);
        assert.equal(checkpoint.checkpoint_generation, initial.generation);
        start = performance.now();
        const child = launch("calldiff");
        assert.equal((await message(child)).ready, true);
        result.startup.calldiff_worker_ready_ms = performance.now() - start;
        const apiOptions = (operation) => {
          if (operation === "tree")
            return { cwd: root, entries: [fixture.root], maxDepth: 32, color: false };
          // Pin the sole changed root: calldiff treats non-underscore private Rust functions as exports.
          return { cwd: root, from: "HEAD", entries: [fixture.root], maxDepth: 32, color: false };
        };
        const first = await message(child, { operation: "tree", options: apiOptions("tree") });
        result.startup.calldiff_first_tree_ms = first.ms;
        result.startup.calldiff_warnings = first.warnings;
        result.evidence.calldiff_initial_tree = first.result;
        assert.ok(
          first.warnings.every((line) => !line.includes("warn: failed to parse")),
          first.warnings.join("\n"),
        );
        validate(fixture, "calldiff", "tree", "before", first.result);
        result.startup.calldiff_native = (await message(child, { operation: "metadata" })).native;
        assert.ok(result.startup.calldiff_native.length, "native grammar provenance unavailable");
        const engines = ["stackscout", "calldiff_api"];
        if (!options["no-cli"]) engines.push("calldiff_cli");
        for (const operation of ["same_generation", "edit_diff", "repeated_diff", "tree"]) {
          const rows = Object.fromEntries(engines.map((engine) => [engine, { ms: [], wall: [] }]));
          result.operations[operation] = Object.fromEntries(
            engines.map((engine) => [
              engine,
              {
                measured_ms: { samples: rows[engine].ms },
                parent_wall_ms: { samples: rows[engine].wall },
              },
            ]),
          );
          const side = operation === "same_generation" ? "before" : "after";
          const kind = operation === "tree" ? "tree" : "diff";
          for (let i = 0; i < warmups + samples; i++) {
            // Rotate each round, including warmups. Do not always give one engine warmest pages.
            const order = engines
              .slice(i % engines.length)
              .concat(engines.slice(0, i % engines.length));
            for (const engine of order) {
              abort.signal.throwIfAborted();
              let previous;
              if (operation === "edit_diff") {
                writeFileSync(join(root, fixture.changed), fixture.before[fixture.changed]);
                if (engine === "stackscout")
                  previous = await request({ op: "sync", paths: [fixture.changed] });
              } else if (engine === "stackscout") previous = await request({ op: "status" });
              if (previous) {
                healthy(previous);
                assert.equal(previous.instance, instance);
              }
              const begin = performance.now();
              let writeMs = 0,
                value,
                sync,
                apiMs,
                warnings = [];
              if (operation === "edit_diff") {
                writeFileSync(join(root, fixture.changed), fixture.after[fixture.changed]);
                writeMs = performance.now() - begin;
              }
              if (engine === "stackscout") {
                if (operation === "edit_diff")
                  sync = await request({ op: "sync", paths: [fixture.changed] });
                let op = { op: "diff", name: "before" };
                if (kind === "tree")
                  op = {
                    op: "callees",
                    symbol: `${fixture.language}:${fixture.changed}::${fixture.root}`,
                    depth: 32,
                    limit: 256,
                  };
                value = await request(op);
              } else if (engine === "calldiff_api") {
                const reply = await message(child, { operation: kind, options: apiOptions(kind) });
                value = reply.result;
                apiMs = writeMs + reply.ms;
                warnings = reply.warnings;
              } else {
                let args = [
                  "diff",
                  "HEAD",
                  "-e",
                  fixture.root,
                  "--max-depth",
                  "32",
                  "--format",
                  "json",
                ];
                if (kind === "tree")
                  args = ["tree", "-e", fixture.root, "--max-depth", "32", "--format", "json"];
                const reply = command(
                  process.execPath,
                  [join(calldiff, "dist/cli.js"), ...args],
                  root,
                );
                value = JSON.parse(reply.stdout);
                if (value.data) value = value.data;
                warnings = [reply.stderr];
              }
              const wallMs = performance.now() - begin;
              // Oracles and evidence serialization are outside timing. Keep failed samples too.
              result.last_sample = {
                engine,
                operation,
                round: i,
                warmup: i < warmups,
                ms: apiMs ?? wallMs,
                wall_ms: wallMs,
              };
              result.evidence[`${engine}-${operation}`] = value;
              assert.ok(
                warnings.every((line) => !line.includes("warn: failed to parse")),
                warnings.join("\n"),
              );
              if (engine === "stackscout") {
                healthy(value);
                assert.equal(value.instance, instance);
                let increment = 0;
                if (sync) {
                  healthy(sync);
                  increment = 1;
                  assert.equal(sync.generation, previous.generation + 1);
                }
                assert.equal(value.generation, previous.generation + increment);
                assert.equal(value.files_parsed, previous.files_parsed + increment);
              }
              validate(fixture, engine, kind, side, value);
              if (i >= warmups) {
                rows[engine].wall.push(wallMs);
                rows[engine].ms.push(apiMs ?? wallMs);
              }
            }
          }
          result.operations[operation] = Object.fromEntries(
            Object.entries(rows).map(([engine, row]) => [
              engine,
              { measured_ms: statistics(row.ms), parent_wall_ms: statistics(row.wall) },
            ]),
          );
          save();
          console.log(
            fixture.name,
            operation,
            Object.fromEntries(
              Object.entries(result.operations[operation]).map(([engine, value]) => [
                engine,
                value.measured_ms.median.toFixed(3),
              ]),
            ),
          );
        }
      } finally {
        for (const child of owned) {
          await stop(child);
          children.delete(child);
        }
        rmSync(root, { recursive: true, force: true });
      }
    }
    for (const [key, path] of [
      ["driver_sha256", fileURLToPath(import.meta.url)],
      ["fixtures_sha256", join(here, "calldiff-fixtures.mjs")],
      ["transport_sha256", join(here, "../.pi/transport.ts")],
      ["calldiff_lock_sha256", join(calldiff, "package-lock.json")],
    ])
      assert.equal(sha(readFileSync(path)), report.builds[key], `${key} changed during run`);
    assert.equal(
      treeHash(join(calldiff, "dist")),
      report.builds.calldiff_dist_sha256,
      "calldiff build changed during run",
    );
    for (const c of report.cases)
      for (const module of c.startup.calldiff_native) {
        assert.equal(
          sha(readFileSync(module.file)),
          module.sha256,
          "calldiff native module changed during run",
        );
      }
    report.status = "passed";
  } catch (error) {
    report.status = "failed";
    report.error = error.stack;
    throw error;
  } finally {
    process.off("SIGINT", interrupt);
    process.off("SIGTERM", interrupt);
    save();
  }
  return report;
}

if (
  process.argv[1] &&
  existsSync(process.argv[1]) &&
  pathToFileURL(realpathSync(process.argv[1])).href === import.meta.url
) {
  try {
    if (process.argv[2] === "--worker") await worker(process.argv[3]);
    else if (process.argv[2] === "compare") {
      assert.equal(process.argv.length, 5, "compare BEFORE/results.json AFTER/results.json");
      console.log(
        compare(...process.argv.slice(3).map((path) => JSON.parse(readFileSync(path, "utf8")))),
      );
    } else {
      const { values } = parseArgs({
        options: {
          calldiff: { type: "string" },
          stackscout: { type: "string" },
          out: { type: "string" },
          samples: { type: "string" },
          warmups: { type: "string" },
          case: { type: "string", multiple: true },
          "no-cli": { type: "boolean" },
        },
      });
      await run(values);
    }
  } catch (error) {
    console.error(error.stack);
    process.exitCode = 1;
  }
}
