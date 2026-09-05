import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { mkdtemp, realpath, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { ExtensionAPI, ExtensionContext, ToolDefinition } from "@earendil-works/pi-coding-agent";

// Fast failure/race tests. tests/pi_live.test.ts separately uses Pi's real loader and agent loop.
mock.module("@earendil-works/pi-coding-agent", () => ({
	DEFAULT_MAX_BYTES: 50 * 1024,
	DEFAULT_MAX_LINES: 2000,
	truncateHead: (content: string) => ({ content, truncated: false }),
}));
mock.module("@earendil-works/pi-ai", () => ({ StringEnum: () => ({}) }));
mock.module("typebox", () => ({ Type: {
	Object: () => ({}), Optional: () => ({}), Boolean: () => ({}), String: () => ({}), Integer: () => ({}),
} }));
const { default: extension } = await import("../.pi/extensions/stackscout.ts");

let cwd: string;
beforeEach(async () => { cwd = await realpath(await mkdtemp(join(tmpdir(), "stackscout-test-"))); });
afterEach(async () => { await rm(cwd, { recursive: true, force: true }); });

type Entry = { type: "custom"; customType: string; data: unknown };
type Operation = { op: string; paths?: string[]; name?: string };

function harness(
	handle: (operation: Operation, signal?: AbortSignal) => object | Promise<object> = () => ({}),
	entries: Entry[] = [], sessionId = "session",
) {
	const handlers = new Map<string, (event: any, ctx: ExtensionContext) => Promise<any>>();
	const tools = new Map<string, ToolDefinition>();
	const notices: string[] = [];
	const calls: Operation[] = [];
	const ctx = {
		cwd, hasUI: true, ui: { notify: (text: string) => notices.push(text) },
		sessionManager: { getSessionId: () => sessionId, getEntries: () => entries },
	} as unknown as ExtensionContext;
	extension({
		on: (name: string, handler: any) => handlers.set(name, handler),
		registerTool: (tool: ToolDefinition) => tools.set(tool.name, tool),
		appendEntry: (customType: string, data: unknown) => entries.push({ type: "custom", customType, data }),
		exec: async (_command: string, args: string[], options: { signal?: AbortSignal }) => {
			const operation = JSON.parse(args[2]) as Operation;
			calls.push(operation);
			const response = {
				version: 1, root: cwd, instance: "owner-1", generation: 1,
				index_complete: true, watching: true, recovery_needed: false, watcher_error: null,
				last_update_error: null, analysis_issue_count: 0, analysis_issues: [], progress: null,
				checkpoint_generation: 1, diff: "", graph: { symbols: [], calls: [] }, truncated: false, error: null,
				...await handle(operation, options.signal),
			};
			return { code: 0, stdout: JSON.stringify(response), stderr: "", killed: false };
		},
	} as unknown as ExtensionAPI);
	return {
		start: (reason = "startup") => handlers.get("session_start")!({ reason }, ctx),
		close: () => handlers.get("session_shutdown")!({}, ctx),
		run: (name = "call_graph_diff", params = {}, signal?: AbortSignal) => tools.get(name)!.execute("call", params, signal, undefined, ctx),
		write: (path: string, isError = false, toolName = "write") => handlers.get("tool_result")!({ toolName, input: { path }, isError, content: [] }, ctx),
		calls, entries, notices,
	};
}

test("factory is inert; only current-graph first use can start resources", async () => {
	const app = harness();
	expect(app.calls).toEqual([]);
	await expect(app.run()).rejects.toThrow("Session-start baseline unavailable");
	expect(app.calls).toEqual([]);
	await app.run("call_graph", { action: "callers", symbol: "run" });
	expect(app.calls.map((call) => call.op)).toEqual(["rescan", "callers"]);
	expect(app.entries).toEqual([]);
});

test("capture intent precedes I/O; reload/resume reuse the original checkpoint without creating it", async () => {
	let generation = 1;
	const entries: Entry[] = [];
	const handle = () => {
		expect(entries.length).toBeGreaterThan(0);
		return { generation };
	};
	const app = harness(handle, entries);
	await app.start();
	expect(app.calls.map((call) => call.op)).toEqual(["rescan", "checkpoint"]);
	generation = 2;
	for (const reason of ["reload", "resume", "startup"]) {
		const resumed = harness(handle, JSON.parse(JSON.stringify(entries)));
		await resumed.start(reason);
		const result = await resumed.run();
		expect(result.details).toMatchObject({ generation: 2, baseline: { status: "ready", generation: 1 } });
		expect(resumed.calls.some((call) => call.op === "checkpoint")).toBe(false);
	}
});

test("capture failure survives retries and reload but not a new session or fork", async () => {
	const app = harness((op) => ({ error: op.op === "checkpoint" ? "checkpoint limit reached" : null }));
	await app.start();
	await expect(app.run()).rejects.toThrow("checkpoint limit reached");
	const resumed = harness(() => ({}), app.entries);
	await resumed.start();
	await expect(resumed.run()).rejects.toThrow("checkpoint limit reached");
	expect(resumed.calls).toEqual([]);
	await resumed.run("call_graph", { action: "impact", symbol: "run" });
	for (const reason of ["new", "fork"]) {
		const fresh = harness(() => ({}), app.entries, reason);
		await fresh.start(reason);
		expect((await fresh.run()).details).toMatchObject({ baseline: { status: "ready" } });
	}
});

test.each(["reload", "resume"])("%s without metadata cannot capture a late baseline", async (reason) => {
	const app = harness();
	await app.start(reason);
	expect(app.calls).toEqual([]);
	await expect(app.run()).rejects.toThrow("requires a new session");
});

test("initial checkpoint must use the reconciled owner and generation", async () => {
	for (const mismatch of [{ instance: "owner-2" }, { checkpoint_generation: 0 }, { checkpoint_generation: 2 }]) {
		const app = harness((op) => op.op === "checkpoint" ? mismatch : {});
		await app.start();
		await expect(app.run()).rejects.toThrow("baseline unavailable");
	}
});

test("resume validates root and checkpoint generation", async () => {
	const app = harness();
	await app.start();
	for (const mismatch of [{ root: "/other" }, { checkpoint_generation: 2 }]) {
		const resumed = harness((op) => op.op === "diff" ? mismatch : {}, [...app.entries]);
		await resumed.start("resume");
		await expect(resumed.run()).rejects.toThrow("baseline unavailable");
	}
});

test("missing checkpoints stay unavailable across reload, without recreation", async () => {
	let missing = false;
	const app = harness((op) => ({ error: missing && op.op === "diff" ? "unknown checkpoint: session" : null }));
	await app.start();
	missing = true;
	await expect(app.run()).rejects.toThrow("unknown checkpoint");
	const resumed = harness(() => ({}), app.entries);
	await resumed.start("reload");
	await expect(resumed.run()).rejects.toThrow("unknown checkpoint");
	expect(resumed.calls).toEqual([]);
});

test("a checkpoint lost while disconnected is not recreated on resume", async () => {
	const app = harness();
	await app.start();
	const resumed = harness((op) => ({ error: op.op === "diff" ? "unknown checkpoint: session" : null }), app.entries);
	await resumed.start("resume");
	await expect(resumed.run()).rejects.toThrow("unknown checkpoint");
	expect(resumed.calls.map((call) => call.op)).toEqual(["rescan", "diff"]);
});

test("owner restart invalidates only baseline, not current graph queries", async () => {
	let instance = "owner-1";
	const app = harness(() => ({ instance }));
	await app.start();
	instance = "owner-2";
	await expect(app.run()).rejects.toThrow("baseline unavailable");
	const result = await app.run("call_graph", { action: "callees", symbol: "run" });
	expect(result.details).toMatchObject({ instance: "owner-2", baseline: { status: "unavailable" } });
	expect(app.calls.filter((call) => call.op === "checkpoint")).toHaveLength(1);
});

test("a restart between the barrier and query is rejected", async () => {
	const app = harness((op) => ({ instance: op.op === "diff" ? "owner-2" : "owner-1" }));
	await app.start();
	await expect(app.run()).rejects.toThrow("instance/generation changed");
	await expect(app.run()).rejects.toThrow("baseline unavailable");
});

test("known writes are serialized and awaited; queries cannot return older generations", async () => {
	const gate = Promise.withResolvers<void>();
	let acknowledged = false;
	const app = harness(async (op) => {
		if (op.paths?.length) { await gate.promise; acknowledged = true; }
		if (op.op === "diff") return { generation: 1 };
		return { generation: acknowledged ? 2 : 1 };
	});
	await app.start();
	const write = app.write("@./main.ts");
	const query = app.run();
	await Bun.sleep(10);
	expect(app.calls.some((call) => call.op === "diff")).toBe(false);
	gate.resolve();
	await write;
	await expect(query).rejects.toThrow("instance/generation changed");
	expect(app.calls.find((call) => call.paths?.length)?.paths).toEqual(["main.ts"]);
});

test("failed sync is visible on a successful write and requires full recovery before queries", async () => {
	let broken = true;
	const app = harness((op) => ({ error: broken && op.paths?.length ? "read failed" : null }));
	await app.start();
	expect(await app.write("main.ts")).toMatchObject({ content: [{ text: expect.stringContaining("file write succeeded") }] });
	broken = false;
	const start = app.calls.length;
	await app.run();
	expect(app.calls.slice(start).map((call) => call.op)).toEqual(["rescan", "diff"]);
});

test("failed writes/non-source writes are skipped; ignore changes rescan; aliases sync their target", async () => {
	const app = harness();
	await app.start();
	const start = app.calls.length;
	await app.write("main.ts", true);
	await app.write("README.md");
	await app.write("types.d.mts");
	expect(app.calls).toHaveLength(start);
	await app.write(".gitignore");
	expect(app.calls.at(-1)?.op).toBe("rescan");
	await writeFile(join(cwd, "main.ts"), "");
	await symlink(join(cwd, "main.ts"), join(cwd, "alias.ts"));
	await app.write(join(cwd, "alias.ts"));
	expect(app.calls.at(-1)?.paths).toEqual(["alias.ts", "main.ts"]);
});

test("refresh reconciles external writes without advancing baseline", async () => {
	const app = harness();
	await app.start();
	const start = app.calls.length;
	const result = await app.run("call_graph_diff", { refresh: true });
	expect(app.calls.slice(start).map((call) => call.op)).toEqual(["rescan", "diff"]);
	expect(result.details).toMatchObject({ baseline: { generation: 1 } });
});

test.each([{ version: 2 }, { root: "/wrong" }, { generation: null, index_complete: false }, { error: "queue full" }])("protocol failures do not look like empty diffs: %j", async (failure) => {
	const app = harness(() => failure);
	await app.start();
	await expect(app.run()).rejects.toThrow("baseline unavailable");
});

test("cancelling a waiting query does not cancel a saved-write acknowledgement", async () => {
	const gate = Promise.withResolvers<void>();
	const app = harness(async (op, signal) => {
		if (op.paths?.length) {
			await gate.promise;
			expect(signal?.aborted).toBe(false);
		}
		return {};
	});
	await app.start();
	const write = app.write("main.ts");
	const controller = new AbortController();
	const query = app.run("call_graph_diff", {}, controller.signal);
	controller.abort(new Error("query cancelled"));
	await expect(query).rejects.toThrow("query cancelled");
	gate.resolve();
	await write;
	await app.run();
});

test("cancellation after request takes precedence over protocol errors", async () => {
	const controller = new AbortController();
	const app = harness((op) => {
		if (op.op === "diff") controller.abort(new Error("cancelled during query"));
		return { error: op.op === "diff" ? "query failed" : null };
	});
	await app.start();
	await expect(app.run("call_graph_diff", {}, controller.signal)).rejects.toThrow("cancelled during query");
});

test("shutdown cancels client requests but does not forget checkpoints", async () => {
	const app = harness();
	await app.start();
	await app.close();
	await expect(app.run()).rejects.toThrow("session closed");
	expect(app.calls.some((call) => call.op === "forget")).toBe(false);
});

test("output exposes health, uncertainty, and service truncation even for empty diffs", async () => {
	const app = harness(() => ({ watching: false, recovery_needed: true, watcher_error: "lost events",
		last_update_error: "scan failed", analysis_issue_count: 1, analysis_issues: ["broken.ts"], truncated: true }));
	await app.start();
	const result = await app.run();
	expect(result.content).toEqual([{ type: "text", text: expect.stringContaining("Service result truncated") }]);
	for (const word of ["watching: false", "lost events", "scan failed", "broken.ts", "eventually consistent", "Syntactic analysis"]) {
		expect((result.content[0] as { text: string }).text).toContain(word);
	}
});
