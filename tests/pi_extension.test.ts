import { afterEach, beforeEach, expect, mock, spyOn, test } from "bun:test";
import { createHash } from "node:crypto";
import * as fs from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { ExtensionAPI, ExtensionContext, ToolDefinition } from "@earendil-works/pi-coding-agent";

// Pi supplies these imports through its extension loader. Only the harness-facing
// schema/truncation exports are stubbed; baseline tests use the real filesystem.
mock.module("@earendil-works/pi-coding-agent", () => ({
	DEFAULT_MAX_BYTES: 50_000,
	DEFAULT_MAX_LINES: 2_000,
	formatSize: (bytes: number) => `${bytes} bytes`,
	truncateHead: (content: string) => ({ content, truncated: false }),
}));
mock.module("typebox", () => ({ Type: { Object: () => ({}) } }));
const { default: extension } = await import("../.pi/extensions/stackscout.ts");

let cwd: string;
beforeEach(async () => { cwd = await fs.mkdtemp(join(tmpdir(), "stackscout-test-")); });
afterEach(async () => {
	mock.restore();
	await fs.rm(cwd, { recursive: true, force: true });
});

type Entry = { type: "custom"; customType: string; data: unknown };

function harness(exec: ExtensionAPI["exec"], entries: Entry[] = [], sessionId = "session") {
	let start!: (event: unknown, ctx: ExtensionContext) => Promise<void>;
	let tool!: ToolDefinition;
	const notices: string[] = [];
	const ctx = {
		cwd,
		hasUI: true,
		ui: { notify: (text: string) => notices.push(text) },
		sessionManager: { getSessionId: () => sessionId, getEntries: () => entries },
	} as unknown as ExtensionContext;
	extension({
		on: (_event: string, handler: typeof start) => { start = handler; },
		registerTool: (definition: ToolDefinition) => { tool = definition; },
		appendEntry: (customType: string, data: unknown) => entries.push({ type: "custom", customType, data }),
		exec,
	} as unknown as ExtensionAPI);
	return {
		start: () => start({}, ctx),
		run: (signal?: AbortSignal) => tool.execute("call", {}, signal, undefined, ctx),
		baseline: join(cwd, ".pi", "stackscout-baselines", createHash("sha256").update(sessionId).digest("hex").slice(0, 16)),
		notices,
		entries,
		ctx,
	};
}

const success = { code: 0, stdout: "", stderr: "", killed: false };

test("execution never captures a late baseline", async () => {
	const exec = mock(async () => success);
	const app = harness(exec);
	await expect(app.run()).rejects.toThrow("Start a new session to capture a new baseline");
	expect(exec).not.toHaveBeenCalled();
});

test("completed snapshots are reused after edits and extension reload", async () => {
	await fs.writeFile(join(cwd, "main.ts"), "export function before() {}");
	const exec = mock(async (command: string) => {
		if (command === "rg") return { ...success, stdout: "main.ts\0" };
		return success;
	});
	const app = harness(exec);
	await app.start();
	expect(await fs.readFile(join(app.baseline, ".ready-v2"), "utf8")).toBe("");
	await fs.writeFile(join(cwd, "main.ts"), "export function after() {}");
	const resumed = harness(exec, app.entries);
	await resumed.start();
	const result = await resumed.run();
	expect(await fs.readFile(join(app.baseline, "main.ts"), "utf8")).toContain("before");
	expect(exec.mock.calls.map(([command]) => command)).toEqual(["rg", "cargo"]);
	expect(result.content).toEqual([{ type: "text", text: "No semantic call-graph changes since session start.\n" }]);
});

test("a non-ENOENT marker error preserves the entire baseline", async () => {
	const exec = mock(async () => success);
	const app = harness(exec);
	await fs.mkdir(join(app.baseline, ".ready-v2"), { recursive: true });
	await fs.writeFile(join(app.baseline, "keep.ts"), "untouched");
	await app.start(); // Reading a directory fails with EISDIR, regardless of user permissions.
	expect(app.notices[0]).toContain("EISDIR");
	expect(await fs.readFile(join(app.baseline, "keep.ts"), "utf8")).toBe("untouched");
	expect((await fs.stat(join(app.baseline, ".ready-v2"))).isDirectory()).toBe(true);
	expect(exec).not.toHaveBeenCalled();
	await expect(app.run()).rejects.toThrow("Session-start baseline unavailable");
});

test.each(["EACCES", "EIO"])("marker read error %s never deletes a completed baseline", async (code) => {
	const exec = mock(async () => success);
	const app = harness(exec);
	await fs.mkdir(app.baseline, { recursive: true });
	await fs.writeFile(join(app.baseline, ".ready-v2"), "ready");
	spyOn(fs, "readFile").mockRejectedValueOnce(Object.assign(new Error(code), { code }));
	await app.start();
	expect(app.notices[0]).toContain(code);
	expect(await fs.readFile(join(app.baseline, ".ready-v2"), "utf8")).toBe("ready");
	expect(exec).not.toHaveBeenCalled();
});

test("a missing marker permits rebuilding an incomplete snapshot, including an empty repository", async () => {
	const exec = mock(async () => ({ ...success, code: 1 }));
	const app = harness(exec);
	await fs.mkdir(app.baseline, { recursive: true });
	await fs.writeFile(join(app.baseline, "stale.ts"), "stale");
	await app.start();
	expect(await fs.readdir(app.baseline)).toEqual([".ready-v2"]);
	expect(app.notices).toEqual([]);
});

test("failed copies are cleaned up and failure survives retries, reload, and resume but not a new session", async () => {
	await fs.writeFile(join(cwd, "first.ts"), "export function first() {}");
	const exec = mock(async () => ({ ...success, stdout: "first.ts\0missing.ts\0" }));
	const app = harness(exec);
	await app.start();
	expect(app.notices[0]).toContain("missing.ts");
	await expect(fs.stat(app.baseline)).rejects.toMatchObject({ code: "ENOENT" });
	await fs.writeFile(join(cwd, "missing.ts"), "export function late() {}");
	const persisted = JSON.parse(JSON.stringify(app.entries)) as Entry[];
	for (const runtime of [app, harness(exec, persisted), harness(exec, persisted)]) {
		await runtime.start();
		await expect(runtime.run()).rejects.toThrow("missing.ts");
		await expect(runtime.run()).rejects.toThrow("Start a new session");
	}
	expect(exec).toHaveBeenCalledTimes(1);
	const fresh = harness(exec, app.entries, "new-session");
	await fresh.start();
	expect(await fs.readFile(join(fresh.baseline, "missing.ts"), "utf8")).toContain("late");
});

test("a ready-marker write failure removes the copied snapshot", async () => {
	await fs.writeFile(join(cwd, "main.ts"), "export function main() {}");
	const app = harness(async () => ({ ...success, stdout: "main.ts\0" }));
	spyOn(fs, "writeFile").mockRejectedValueOnce(new Error("marker write failed"));
	await app.start();
	await expect(fs.stat(app.baseline)).rejects.toMatchObject({ code: "ENOENT" });
	await expect(app.run()).rejects.toThrow("marker write failed");
});

test("cleanup errors are reported separately without replacing the copy failure", async () => {
	const app = harness(async () => ({ ...success, stdout: "missing.ts\0" }));
	const remove = fs.rm;
	spyOn(fs, "rm")
		.mockImplementationOnce(remove)
		.mockRejectedValueOnce(new Error("cleanup denied"));
	await app.start();
	expect(app.notices).toHaveLength(2);
	expect(app.notices[0]).toContain("Could not remove incomplete StackScout baseline");
	expect(app.notices[0]).toContain("cleanup denied");
	expect(app.notices[1]).toContain("missing.ts");
	await expect(app.run()).rejects.toThrow("missing.ts");
	await expect(fs.stat(join(app.baseline, ".ready-v2"))).rejects.toMatchObject({ code: "ENOENT" });
});

test("listing failures are explicit in headless mode and never retried by execution", async () => {
	const exec = mock(async () => ({ ...success, code: 2, stderr: "rg failed" }));
	const app = harness(exec);
	app.ctx.hasUI = false;
	const log = spyOn(console, "error").mockImplementation(() => {});
	await app.start();
	expect(log).toHaveBeenCalledWith(expect.stringContaining("rg failed"));
	await expect(app.run()).rejects.toThrow("rg failed");
	expect(exec).toHaveBeenCalledTimes(1);
	await expect(fs.stat(app.baseline)).rejects.toMatchObject({ code: "ENOENT" });
});

test("cancelling a tool cannot cancel baseline capture or discard a completed snapshot", async () => {
	const controller = new AbortController();
	const reason = new Error("cancel tool only");
	const exec = mock(async (command: string, _args: string[], options?: { signal?: AbortSignal }) => {
		if (command === "rg") {
			expect(options?.signal).toBeUndefined();
			controller.abort(reason);
		}
		return success;
	});
	const app = harness(exec);
	await app.start();
	await expect(app.run(controller.signal)).rejects.toBe(reason);
	expect(exec.mock.calls.map(([command]) => command)).toEqual(["rg"]);
	await app.run();
	expect(exec.mock.calls.map(([command]) => command)).toEqual(["rg", "cargo"]);
	expect((await fs.stat(join(app.baseline, ".ready-v2"))).isFile()).toBe(true);
});

test.each([0, 1])("cancellation after Cargo returns takes precedence over exit code %i", async (code) => {
	const controller = new AbortController();
	const reason = new Error("cancelled during cargo");
	const app = harness(async (command, _args, options) => {
		if (command === "cargo") {
			expect(options?.signal).toBe(controller.signal);
			controller.abort(reason);
			return { ...success, code, stderr: "generic command failure", killed: true };
		}
		return success;
	});
	await app.start();
	await expect(app.run(controller.signal)).rejects.toBe(reason);
	expect((await fs.stat(join(app.baseline, ".ready-v2"))).isFile()).toBe(true);
});
