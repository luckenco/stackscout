import assert from "node:assert/strict";

export function fixtures() {
  // Adapted from calldiff 2086f6e, test/callstack-diff.test.ts:
  // "adds and removes free function calls" (MIT). This fixture actually adds a call.
  const before =
    "export function boot() { loadConfig(); connect(); }\nfunction loadConfig() {}\nfunction connect() {}\n";
  const after =
    "export function boot() { loadConfig(); migrate(); connect(); }\nfunction loadConfig() {}\nfunction migrate() {}\nfunction connect() {}\n";
  const cases = [
    {
      name: "upstream-free-calls",
      language: "ts",
      root: "boot",
      changed: "file.ts",
      provenance:
        "calldiff 2086f6e: test/callstack-diff.test.ts, adds and removes free function calls",
      before: { "file.ts": before },
      after: { "file.ts": after },
      edges: {
        before: [
          ["boot", "loadConfig"],
          ["boot", "connect"],
        ],
        after: [
          ["boot", "loadConfig"],
          ["boot", "migrate"],
          ["boot", "connect"],
        ],
      },
      added: ["migrate"],
      removed: [],
    },
  ];
  for (const language of ["ts", "rust"]) {
    for (const functions of [100, 1000, 10000]) cases.push(chain(language, functions, 0));
    cases.push(chain(language, 1000, 27));
  }
  return cases;
}

function chain(language, functions, padding) {
  const files = {};
  const edges = [];
  let extension = "ts",
    declaration = "function",
    exported = "export",
    variable = "const";
  if (language === "rust") {
    extension = "rs";
    declaration = "fn";
    exported = "pub";
    variable = "let";
  }
  for (let file = 0; file < functions / 10; file++) {
    let source = "";
    for (let i = 0; i < 10; i++) {
      let visibility = "",
        body = "";
      if (i === 0) visibility = `${exported} `;
      for (let p = 0; p < padding; p++) body += `  ${variable} pad_${p} = ${p};\n`;
      if (i < 9) body += `  f_${file}_${i + 1}();\n`;
      else if (file === 0) body += "  removed();\n";
      source += `${visibility}${declaration} f_${file}_${i}() {\n${body}}\n`;
      if (file === 0 && i < 9) edges.push([`f_0_${i}`, `f_0_${i + 1}`]);
    }
    if (file === 0) source += `${declaration} removed() {}\n${declaration} added() {}\n`;
    files[`m${file}.${extension}`] = source;
  }
  const changed = `m0.${extension}`;
  let name = `${language}-${functions}`;
  if (padding) name += "-padded";
  return {
    name,
    language,
    root: "f_0_0",
    changed,
    provenance: "generated independent 10-function chains; two additional leaf definitions",
    before: files,
    after: { ...files, [changed]: files[changed].replace("removed();", "added();") },
    edges: { before: [...edges, ["f_0_9", "removed"]], after: [...edges, ["f_0_9", "added"]] },
    added: ["added"],
    removed: ["removed"],
  };
}

function changePaths(fixture, side, names) {
  return names
    .map((name) => {
      const path = [name];
      while (path[0] !== fixture.root) {
        const parents = fixture.edges[side].filter(([, callee]) => callee === path[0]);
        assert.equal(
          parents.length,
          1,
          "these fixtures require a unique path to each changed call",
        );
        assert.ok(path.length <= fixture.edges[side].length, "cyclic fixture");
        path.unshift(parents[0][0]);
      }
      return path.join(" -> ");
    })
    .sort();
}

export function validate(fixture, tool, operation, side, result) {
  const expected = fixture.edges[side];
  const symbols = [...new Set([fixture.root, ...expected.flat()])].sort();
  if (tool === "stackscout") {
    assert.equal(result.error, null);
    assert.equal(result.last_update_error, null);
    assert.equal(result.index_complete, true);
    assert.equal(result.analysis_issue_count, 0);
    assert.equal(result.truncated, false, "partial output is not an equivalent-output win");
    if (operation === "tree") {
      assert.equal(result.graph.truncated, false);
      const names = new Map(
        result.graph.symbols.map((symbol) => {
          assert.equal(symbol.file, fixture.changed);
          return [symbol.id, symbol.id.split("::").at(-1)];
        }),
      );
      assert.equal(names.size, result.graph.symbols.length, "duplicate symbols");
      assert.deepEqual([...names.values()].sort(), symbols);
      const edges = result.graph.calls.map((call) => {
        assert.equal(call.resolution, "resolved");
        assert.ok(names.has(call.caller) && names.has(call.callee));
        return [names.get(call.caller), names.get(call.callee)];
      });
      assert.deepEqual(edges.sort(), [...expected].sort());
      return;
    }
    if (side === "before") return assert.equal(result.diff, "");
    assert.ok(result.diff.includes(`::${fixture.root}`));
    const changes = { "+": [], "-": [] },
      path = [];
    for (const line of result.diff.split("\n")) {
      const match = line.match(/^([ ~+-]) ( *)(?:ts|rust):[^ ]+::([\w]+)(?:\s|$)/);
      if (!match) {
        assert.ok(!/^[+-]/.test(line), `unexpected changed line: ${line}`);
        continue;
      }
      const depth = match[2].length / 2;
      assert.ok(Number.isInteger(depth) && depth <= path.length, "invalid tree indentation");
      path.length = depth;
      path.push(match[3]);
      if (changes[match[1]]) changes[match[1]].push(path.join(" -> "));
    }
    assert.deepEqual(changes["+"].sort(), changePaths(fixture, "after", fixture.added));
    assert.deepEqual(changes["-"].sort(), changePaths(fixture, "before", fixture.removed));
    return;
  }
  assert.equal(result.mode, operation);
  if (operation === "diff" && side === "before") return assert.deepEqual(result.trees, []);
  assert.equal(result.trees.length, 1);
  assert.equal(result.trees[0].tree.key, fixture.root);
  const edges = [],
    names = [],
    added = [],
    removed = [];
  function visit(node, path) {
    path = [...path, node.key];
    names.push(node.key);
    if (node.status === "added") added.push(path.join(" -> "));
    if (node.status === "removed") removed.push(path.join(" -> "));
    for (const child of node.children) {
      edges.push([node.key, child.key]);
      visit(child, path);
    }
  }
  visit(result.trees[0].tree, []);
  if (operation === "tree") {
    assert.deepEqual(names.sort(), symbols);
    assert.deepEqual(edges, expected);
  } else {
    assert.deepEqual(added.sort(), changePaths(fixture, "after", fixture.added));
    assert.deepEqual(removed.sort(), changePaths(fixture, "before", fixture.removed));
  }
}
