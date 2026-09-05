import { afterEach, beforeEach, expect, test } from "bun:test";
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { createInterface } from "node:readline";

const binary = resolve("target/debug/stackscout");
const extension = resolve(".pi/extensions/stackscout.ts");
let pkg = process.env.STACKSCOUT_PI_PACKAGE;
if (!pkg && Bun.which("pi")) {
	let path = dirname(await realpath(Bun.which("pi")!));
	while (path !== dirname(path)) {
		if (existsSync(join(path, "dist/core/sdk.js"))) { pkg = path; break; }
		path = dirname(path);
	}
}
if (!pkg) console.warn("Live Pi tests skipped: install Pi or set STACKSCOUT_PI_PACKAGE to its package directory.");

let temporary: string;
let root: string;
let clients: Awaited<ReturnType<typeof client>>[];
beforeEach(async () => {
	temporary = await realpath(await mkdtemp(join(tmpdir(), "stackscout-live-")));
	root = join(temporary, "repo");
	await mkdir(root);
	await writeFile(join(root, "api.ts"), 'import { audit } from "./dep";\nexport function run() {}\n');
	await writeFile(join(root, "dep.ts"), "export function audit() {}\n");
	clients = [];
});
afterEach(async () => {
	for (const app of clients) app.child.stdin.end();
	let forced = false;
	// Pi's exec cancellation keeps a five-second escalation timer alive.
	const timeout = setTimeout(() => {
		forced = true;
		for (const app of clients) app.child.kill("SIGKILL");
	}, 10_000);
	try {
		const results = await Promise.all(clients.map((app) => app.exited));
		expect(forced).toBe(false);
		for (const result of results) expect(result).toEqual({ code: 0, signal: null });
	} finally {
		clearTimeout(timeout);
		for (const app of clients) app.child.kill();
		// Only stop the owner of this temporary workspace, never the developer's owner.
		if (existsSync(join(root, ".stackscout"))) {
			const status = request({ op: "status" });
			process.kill(Number(status.instance.split("-")[0]), "SIGTERM");
		}
		await rm(temporary, { recursive: true, force: true });
	}
}, 15_000);

function request(operation: object) {
	const result = Bun.spawnSync([binary, "request", root, JSON.stringify({ version: 1, ...operation })], {
		env: { ...process.env, STACKSCOUT_WATCH: "0" },
	});
	if (result.exitCode !== 0) throw new Error(result.stderr.toString());
	return JSON.parse(result.stdout.toString());
}

async function client(sessionFile?: string) {
	const agentDir = await mkdtemp(join(temporary, "agent-"));
	const args = [resolve("tests/pi_client.mjs"), pkg!, root, agentDir, extension];
	if (sessionFile) args.push(sessionFile);
	const child = spawn("node", args, {
		env: { ...process.env, PI_OFFLINE: "1", STACKSCOUT_WATCH: "0", STACKSCOUT_BIN: binary },
		stdio: ["pipe", "pipe", "pipe"],
	});
	let stderr = "";
	child.stderr.on("data", (data) => { stderr += data; });
	const lines = createInterface({ input: child.stdout })[Symbol.asyncIterator]();
	const exited = new Promise<{ code: number | null; signal: NodeJS.Signals | null }>((resolve) => {
		child.once("close", (code, signal) => resolve({ code, signal }));
	});
	async function receive() {
		const line = await lines.next();
		if (line.done) throw new Error(`Pi client exited: ${stderr}`);
		const result = JSON.parse(line.value);
		if (result.error) throw new Error(`Pi client: ${result.error}\n${stderr}`);
		return result;
	}
	const app = {
		child, exited, ready: undefined as any,
		async send(command: object) { child.stdin.write(JSON.stringify(command) + "\n"); return receive(); },
	};
	clients.push(app);
	app.ready = await receive();
	return app;
}

const live = test.skipIf(!pkg);

live("two real Pi processes share parsing, acknowledge edit/write, and preserve baseline meaning across lifecycle", async () => {
	const a = await client();
	expect(a.ready.tools).toContain("call_graph");
	expect(a.ready.baseline.status).toBe("ready");
	const indexed = request({ op: "status" });
	expect(indexed.files_parsed).toBe(2);
	const b = await client();
	expect(b.ready.baseline.instance).toBe(a.ready.baseline.instance);
	expect(request({ op: "status" }).files_parsed).toBe(indexed.files_parsed);

	const edited = await a.send({ name: "edit", params: { path: "api.ts", edits: [{ oldText: "run() {}", newText: "run() { audit(); }" }] } });
	expect(edited.isError).toBe(false);
	const diff = await b.send({ name: "call_graph_diff" });
	expect(diff.isError).toBe(false);
	expect(diff.details.generation).toBeGreaterThan(diff.details.checkpoint_generation);
	expect(diff.details.diff).toContain('ts:dep.ts::audit @ "api.ts":2:');
	expect(request({ op: "status" }).files_parsed).toBe(3);

	const written = await a.send({ name: "write", params: { path: "dep.ts", content: "export function audit() { external(); }\n" } });
	expect(written.isError).toBe(false);
	const graph = await b.send({ name: "call_graph", params: { action: "callees", symbol: "run", depth: 3 } });
	expect(graph.isError).toBe(false);
	expect(graph.details.graph.calls.some((call: any) => call.resolution === "unresolved")).toBe(true);
	expect(graph.details.generation).toBeGreaterThan(diff.details.generation);
	expect(graph.content[0].text).toContain("eventually consistent");

	const parses = request({ op: "status" }).files_parsed;
	const reloaded = await a.send({ op: "reload" });
	expect(reloaded.baseline).toEqual(a.ready.baseline);
	b.child.stdin.end();
	await b.exited;
	const resumed = await client(b.ready.sessionFile);
	expect(resumed.ready.baseline).toEqual(b.ready.baseline);
	expect(request({ op: "status" }).files_parsed).toBe(parses);

	request({ op: "forget", name: a.ready.baseline.name });
	expect((await a.send({ name: "call_graph_diff" })).isError).toBe(true);
	expect((await a.send({ op: "reload" })).baseline.status).toBe("unavailable");
	expect((await resumed.send({ name: "call_graph_diff" })).isError).toBe(false);

	const owner = request({ op: "status" }).instance;
	process.kill(Number(owner.split("-")[0]), "SIGKILL");
	// Wait for the old owner to release its lock before the CLI starts its replacement.
	await Bun.sleep(50);
	expect(request({ op: "sync", paths: [] }).instance).not.toBe(owner);
	expect((await resumed.send({ name: "call_graph_diff" })).isError).toBe(true);
	expect((await resumed.send({ op: "reload" })).baseline.status).toBe("unavailable");
	expect((await resumed.send({ name: "call_graph", params: { action: "impact", symbol: "audit" } })).isError).toBe(false);
}, 60_000);

live("cancelling a spawned request leaves the owner and checkpoint usable, then exits cleanly", async () => {
	const app = await client();
	const owner = request({ op: "status" }).instance;
	const ownerPid = Number(owner.split("-")[0]);
	process.kill(ownerPid, "SIGSTOP");
	try {
		const pending = app.send({ name: "call_graph", params: { action: "callers", symbol: "audit", refresh: true } });
		// Hold the owner so the real request cannot finish before cancellation.
		const deadline = Date.now() + 10_000;
		let requestPid: number | undefined;
		while (!requestPid) {
			const processes = Bun.spawnSync(["ps", "-axo", "pid=,ppid=,comm="]);
			if (processes.exitCode !== 0) throw new Error(processes.stderr.toString());
			for (const line of processes.stdout.toString().trim().split("\n")) {
				const [pid, parent, ...command] = line.trim().split(/\s+/);
				if (Number(parent) === app.child.pid && command.join(" ").endsWith("stackscout")) requestPid = Number(pid);
			}
			if (Date.now() > deadline) throw new Error("Pi did not spawn a StackScout request");
			if (!requestPid) await Bun.sleep(10);
		}
		app.child.kill("SIGUSR2");
		expect((await pending).isError).toBe(true);
		// The cancelled tool can return before the OS reaps its subprocess (observed on Linux).
		const stopped = Date.now() + 6000;
		for (;;) {
			try { process.kill(requestPid, 0); }
			catch (error) {
				if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
				break;
			}
			if (Date.now() > stopped) throw new Error("Cancelled StackScout request did not exit");
			await Bun.sleep(10);
		}
	} finally {
		process.kill(ownerPid, "SIGCONT");
	}
	expect(request({ op: "status" }).instance).toBe(owner);
	const recovered = await app.send({ name: "call_graph_diff" });
	expect(recovered.isError).toBe(false);
	expect(recovered.details.baseline).toMatchObject({ name: app.ready.baseline.name, generation: app.ready.baseline.generation });
}, 60_000);

live("reload and resume retain the original checkpoint after a recoverable rescan failure", async () => {
	let app = await client();
	const baseline = app.ready.baseline;
	// Pi flushes a new session journal only after its first assistant turn.
	expect((await app.send({ name: "call_graph_diff" })).isError).toBe(false);
	for (const phase of ["reload", "resume"]) {
		await writeFile(join(root, "oversized.ts"), " ".repeat(8 * 1024 * 1024 + 1));
		if (phase === "reload") {
			expect((await app.send({ op: "reload" })).baseline).toEqual(baseline);
		} else {
			app.child.stdin.end();
			await app.exited;
			app = await client(app.ready.sessionFile);
			expect(app.ready.baseline).toEqual(baseline);
		}
		await rm(join(root, "oversized.ts"));
		await writeFile(join(root, "api.ts"), 'import { audit } from "./dep";\nexport function run() { audit(); }\n');
		const result = await app.send({ name: "call_graph_diff" });
		expect(result.isError).toBe(false);
		expect(result.details.baseline).toEqual(baseline);
		expect(result.details.diff).toContain('ts:dep.ts::audit @ "api.ts":2:');
	}
}, 60_000);

live("failed capture remains failed after repair/resume; current graph and a new session can recover", async () => {
	await writeFile(join(root, "oversized.ts"), " ".repeat(8 * 1024 * 1024 + 1));
	const a = await client();
	expect(a.ready.baseline.status).toBe("unavailable");
	await rm(join(root, "oversized.ts"));
	expect((await a.send({ name: "call_graph_diff" })).isError).toBe(true);
	expect((await a.send({ name: "call_graph", params: { action: "callers", symbol: "run" } })).isError).toBe(false);
	a.child.stdin.end();
	await a.exited;
	const resumed = await client(a.ready.sessionFile);
	expect(resumed.ready.baseline.status).toBe("unavailable");
	expect((await resumed.send({ name: "call_graph_diff" })).isError).toBe(true);
	const fresh = await client();
	expect(fresh.ready.baseline.status).toBe("ready");
}, 60_000);

live("real Pi truncation respects its budget and does not promise recovery of service omissions", async () => {
	const a = await client();
	const content = `export function run() { ${Array.from({ length: 3500 }, (_, index) => `unknown${index}();`).join(" ")} }`;
	expect((await a.send({ name: "write", params: { path: "api.ts", content } })).isError).toBe(false);
	for (let index = 0; index < 32; index++) {
		await writeFile(join(root, `${"broken".repeat(30)}-${index}.ts`), "export function broken( {");
	}
	const diff = await a.send({ name: "call_graph_diff", params: { refresh: true } });
	expect(diff.isError).toBe(false);
	expect(diff.details.truncated).toBe(true);
	expect(diff.details.display_truncated).toBe(true);
	const text = diff.content[0].text;
	expect(Buffer.byteLength(text)).toBeLessThanOrEqual(50 * 1024);
	expect(text.split("\n").length).toBeLessThanOrEqual(2000);
	expect(text).toContain("Service-side omissions are not recoverable");
	const saved = await readFile(diff.details.responsePath, "utf8");
	expect(saved).toContain("Service result truncated");
	expect(saved.length).toBeGreaterThan(text.length);
	await rm(dirname(diff.details.responsePath), { recursive: true });
}, 60_000);
