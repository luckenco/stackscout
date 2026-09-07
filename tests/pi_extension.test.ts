import { afterEach, beforeEach, expect, mock, spyOn, test } from "bun:test";
import * as fs from "node:fs/promises";
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
	Object: () => ({}), Array: () => ({}), Optional: () => ({}), Boolean: () => ({}), String: () => ({}), Integer: () => ({}),
} }));
class Unavailable extends Error {}
const endpoints = new Map<string, (op: Operation, signal?: AbortSignal) => Promise<object>>();
mock.module("../.pi/transport.ts", () => ({
	Unavailable,
	exchange: (endpoint: { socket: string }, operation: Operation, signal: AbortSignal) => endpoints.get(endpoint.socket)!(operation, signal),
}));
const { default: extension } = await import("../.pi/extensions/stackscout.ts");

let cwd: string;
beforeEach(async () => { cwd = await realpath(await mkdtemp(join(tmpdir(), "stackscout-test-"))); });
afterEach(async () => { mock.restore(); endpoints.clear(); await rm(cwd, { recursive: true, force: true }); });

type Entry = { type: "custom"; customType: string; data: unknown };
type Operation = { op: string; paths?: string[]; name?: string; baseline?: string; baseline_generation?: number; scope?: { files: string[]; symbols: string[] }; token?: string };

function harness(
	handle: (operation: Operation, signal?: AbortSignal) => object | Promise<object> = () => ({}),
	entries: Entry[] = [], sessionId = "session",
) {
	const handlers = new Map<string, (event: any, ctx: ExtensionContext) => Promise<any>>();
	const tools = new Map<string, ToolDefinition>();
	const notices: string[] = [];
	const calls: Operation[] = [];
	let starts = 0;
	const socket = join(cwd, crypto.randomUUID() + ".sock");
	endpoints.set(socket, async (operation, signal) => {
		calls.push(operation);
		return {
			version: 1, root: cwd, instance: "owner-1", generation: 1,
			index_complete: true, watching: true, recovery_needed: false, watcher_error: null,
			last_update_error: null, analysis_issue_count: 0, analysis_issues: [], progress: null,
			checkpoint_generation: 1, diff: "", graph: { symbols: [], calls: [] }, truncated: false, error: null,
			...await handle(operation, signal),
		};
	});
	const ctx = {
		cwd, hasUI: true, ui: { notify: (text: string) => notices.push(text) },
		sessionManager: { getSessionId: () => sessionId, getEntries: () => entries },
	} as unknown as ExtensionContext;
	extension({
		on: (name: string, handler: any) => handlers.set(name, handler),
		registerTool: (tool: ToolDefinition) => tools.set(tool.name, tool),
		appendEntry: (customType: string, data: unknown) => entries.push({ type: "custom", customType, data }),
		exec: async (_command: string, args: string[]) => {
			expect(args).toEqual(["connect", cwd]);
			starts++;
			return { code: 0, stdout: JSON.stringify({ version: 1, root: cwd, socket }), stderr: "", killed: false };
		},
	} as unknown as ExtensionAPI);
	return {
		start: (reason = "startup") => handlers.get("session_start")!({ reason }, ctx),
		close: () => handlers.get("session_shutdown")!({}, ctx),
		run: (name = "call_graph_diff", params = {}, signal?: AbortSignal) => tools.get(name)!.execute("call", params, signal, undefined, ctx),
		write: (path: string, isError = false, toolName = "write") => handlers.get("tool_result")!({ toolName, input: { path }, isError, content: [] }, ctx),
		calls, entries, notices, get starts() { return starts; },
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

test("bootstrap is reused and only pre-connect absence allows an automatic retry", async () => {
	let failure: Error | undefined;
	const app = harness(() => {
		if (failure) { const error = failure; failure = undefined; throw error; }
		return {};
	});
	await app.start();
	await app.run();
	expect(app.starts).toBe(1);
	failure = new Unavailable("owner stopped");
	await app.run();
	expect(app.starts).toBe(2);
	const calls = app.calls.length;
	failure = new Error("reply lost after sending mutation");
	await expect(app.run()).rejects.toThrow("reply lost");
	expect(app.calls.length).toBe(calls + 1);
	expect(app.starts).toBe(2);
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

test("resume invalidates a changed repository root or checkpoint generation", async () => {
	const app = harness();
	await app.start();
	const entries = JSON.parse(JSON.stringify(app.entries));
	entries.at(-1).data.root = "/other";
	const moved = harness(() => ({}), entries);
	await moved.start("resume");
	await expect(moved.run()).rejects.toThrow("repository root changed");
	const replaced = harness((op) => op.op === "diff" ? { checkpoint_generation: 2 } : {}, [...app.entries]);
	await replaced.start("resume");
	await expect(replaced.run()).rejects.toThrow("Checkpoint generation changed");
});

test.each(["reload", "resume", "startup"])("%s retains checkpoint identity through transient rescan failure", async (reason) => {
	const app = harness();
	await app.start();
	const entries = JSON.parse(JSON.stringify(app.entries));
	let broken = true;
	const resumed = harness((op) => ({ error: broken && op.op === "rescan" ? "temporary read failure" : null }), entries);
	await resumed.start(reason);
	expect(entries).toEqual(app.entries);
	expect(resumed.notices.at(-1)).toContain("Original checkpoint retained");
	broken = false;
	const result = await resumed.run();
	expect(result.details).toMatchObject({ baseline: { status: "ready", generation: 1 } });
	expect(resumed.calls.map((call) => call.op)).toEqual(["rescan", "rescan", "diff"]);
});

test.each([{ error: "query queue unavailable" }, { root: "/wrong" }, { checkpoint_generation: null }])("resume retains the baseline when checkpoint validation is inconclusive: %j", async (failure) => {
	const app = harness();
	await app.start();
	let broken = true;
	const resumed = harness((op) => broken && op.op === "diff" ? failure : {}, [...app.entries]);
	await resumed.start("resume");
	expect(resumed.entries).toEqual(app.entries);
	broken = false;
	expect((await resumed.run()).details).toMatchObject({ baseline: { status: "ready", generation: 1 } });
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

test("queries await known writes and cannot return older generations", async () => {
	const gate = Promise.withResolvers<void>();
	const started = Promise.withResolvers<void>();
	let acknowledged = false;
	const app = harness(async (op) => {
		if (op.paths?.length) { started.resolve(); await gate.promise; acknowledged = true; }
		if (op.op === "diff") return { generation: 1 };
		return { generation: acknowledged ? 2 : 1 };
	});
	await app.start();
	const write = app.write("@./main.ts");
	const query = app.run();
	await started.promise;
	expect(app.calls.some((call) => call.op === "diff")).toBe(false);
	gate.resolve();
	await write;
	await expect(query).rejects.toThrow("instance/generation changed");
	expect(app.calls.find((call) => call.paths?.length)?.paths).toEqual(["main.ts"]);
});

test("overlapping writes synchronize in order before the next query", async () => {
	const started = Promise.withResolvers<void>();
	const release = Promise.withResolvers<void>();
	let generation = 1;
	const app = harness(async (op) => {
		if (op.paths?.[0] === "first.ts") { started.resolve(); await release.promise; }
		if (op.paths?.length) generation++;
		return { generation };
	});
	await app.start();
	const first = app.write("first.ts");
	await started.promise;
	const second = app.write("second.ts");
	const query = app.run();
	expect(app.calls.filter((call) => call.paths?.length).map((call) => call.paths)).toEqual([["first.ts"]]);
	release.resolve();
	await Promise.all([first, second]);
	expect((await query).details).toMatchObject({ generation: 3 });
	expect(app.calls.filter((call) => call.paths?.length).map((call) => call.paths)).toEqual([["first.ts"], ["second.ts"]]);
});

test("an earlier query barrier cannot clear a later failed-write recovery requirement", async () => {
	const started = Promise.withResolvers<void>();
	const release = Promise.withResolvers<void>();
	let delay = false;
	const app = harness(async (op) => {
		if (op.op === "sync" && op.paths?.length === 0 && delay) {
			delay = false;
			started.resolve();
			await release.promise;
		}
		return { error: op.paths?.length ? "update queue unavailable; retry the request" : null };
	});
	await app.start();
	delay = true;
	const earlier = app.run("call_graph", { action: "callers", symbol: "run" });
	await started.promise;
	// Resolve path normalization immediately so all runnable microtasks drain deterministically.
	spyOn(fs, "realpath").mockImplementation(async (path) => String(path));
	const write = app.write("main.ts");
	await new Promise<void>((resolve) => setImmediate(resolve));
	release.resolve();
	expect(await write).toMatchObject({ content: [{ text: expect.stringContaining("file write succeeded") }] });
	await earlier;
	const start = app.calls.length;
	await app.run();
	expect(app.calls.slice(start).map((call) => call.op)).toEqual(["rescan", "diff"]);
});

test.each(["read failed", ""])("failed sync is visible and requires recovery even with an empty error: %j", async (error) => {
	let broken = true;
	const app = harness((op) => ({ error: broken && op.paths?.length ? error : null }));
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

test("scoped review names survive reload, canonicalize scope, and keep session baseline immutable", async () => {
	const handle = (op: Operation) => op.op === "review" ? {
		generation: 2, review_token: "owner-1:1", review_initialized: true,
		review: { witnesses: [], boundaries: [], missing: [{ side: "after", selector: "absent", reason: "unknown symbol" }], analysis_issues: ["broken.ts"], truncated: false },
	} : {};
	const app = harness(handle);
	await app.start();
	const entries = [...app.entries];
	const result = await app.run("call_graph_review", { files: ["b.ts", "a.ts", "a.ts"], refresh: true });
	const review = app.calls.at(-1)!;
	expect(review).toMatchObject({ op: "review", baseline_generation: 1, scope: { files: ["a.ts", "b.ts"], symbols: [] } });
	expect(app.calls.at(-2)?.op).toBe("rescan");
	expect(app.entries).toEqual(entries);
	expect(result.details).toMatchObject({ baseline: { generation: 1 }, review_token: "owner-1:1" });
	for (const word of ["Not acknowledged", "original session checkpoint", "missing scope", "broken.ts"]) {
		expect((result.content[0] as { text: string }).text).toContain(word);
	}
	const resumed = harness(handle, app.entries);
	await resumed.start("reload");
	await resumed.run("call_graph_review", { files: ["a.ts", "b.ts"] });
	expect(resumed.calls.at(-1)?.name).toBe(review.name);
	await resumed.run("call_graph_review", { files: ["a.ts"] });
	expect(resumed.calls.at(-1)?.name).not.toBe(review.name);
});

test("review acknowledgement does not rescan or acknowledge another session/owner", async () => {
	const app = harness((op) => op.op === "acknowledge" ? { generation: 3, checkpoint_generation: 2 } : {});
	await app.start();
	const name = `${app.calls.at(-1)!.name}-review-scope`;
	const start = app.calls.length;
	const result = await app.run("call_graph_ack", { review: name, token: "owner-1:1" });
	expect(app.calls.slice(start)).toEqual([{ op: "acknowledge", name, token: "owner-1:1" }]);
	expect(result.details).toMatchObject({ generation: 3, checkpoint_generation: 2, baseline: { generation: 1 } });
	await expect(app.run("call_graph_ack", { review: name, token: "owner-2:1" })).rejects.toThrow("does not belong");
	await expect(app.run("call_graph_ack", { review: "other", token: "owner-1:1" })).rejects.toThrow("does not belong");
});

test("cancelling a review never acknowledges its pending result", async () => {
	const controller = new AbortController();
	const app = harness((op) => {
		if (op.op === "review") controller.abort(new Error("review cancelled"));
		return {};
	});
	await app.start();
	const entries = [...app.entries];
	await expect(app.run("call_graph_review", { symbols: ["run"] }, controller.signal)).rejects.toThrow("review cancelled");
	expect(app.calls.some((op) => op.op === "acknowledge")).toBe(false);
	expect(app.entries).toEqual(entries);
});

test("review cannot create a late baseline after capture failure or owner restart", async () => {
	const app = harness((op) => ({ error: op.op === "checkpoint" ? "capture failed" : null }));
	await app.start();
	await expect(app.run("call_graph_review", { symbols: ["run"] })).rejects.toThrow("capture failed");
	expect(app.calls.some((op) => op.op === "review")).toBe(false);
	let instance = "owner-1";
	const restarted = harness(() => ({ instance }));
	await restarted.start();
	instance = "owner-2";
	await expect(restarted.run("call_graph_review", { symbols: ["run"] })).rejects.toThrow("baseline unavailable");
	expect(restarted.calls.some((op) => op.op === "review")).toBe(false);
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
