import { createHash } from "node:crypto";
import { mkdtemp, realpath, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { basename, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, truncateHead } from "@earendil-works/pi-coding-agent";
import { StringEnum } from "@earendil-works/pi-ai";
import { Type } from "typebox";

const STATE = "stackscout-session-v1";
const WARNING = "Syntactic analysis only: unresolved/dynamic calls may be missing; no path does not prove no impact.";
const REFRESH = Type.Optional(Type.Boolean({ description: "Rescan saved files first (for Bash/external edits or ignore changes). Does not advance the baseline." }));

type Generation = { instance: string; generation: number };
type Baseline =
	| (Generation & { status: "ready"; root: string; name: string })
	| { status: "unavailable"; reason: string };
type Range = { start_line: number; start_column: number; end_line: number; end_column: number };
type Graph = {
	symbols: { id: string; file: string; location: Range; depth: number }[];
	calls: { caller: string; callee: string | null; target: string; resolution: string; location: Range }[];
};
type Review = {
	witnesses: { side: string; relation: string; change: string; scope: string; symbol: Graph["symbols"][number]; path: Graph["calls"]; calls: Graph["calls"] }[];
	boundaries: { side: string; call: Graph["calls"][number] }[];
	missing: { side: string; selector: string; reason: string }[];
	analysis_issues: string[]; truncated: boolean;
};
type Response = {
	version: number; root: string; instance: string; generation: number | null;
	index_complete: boolean; watching: boolean; recovery_needed: boolean;
	watcher_error: string | null; last_update_error: string | null;
	analysis_issue_count: number; analysis_issues: string[]; progress: unknown;
	checkpoint_generation: number | null; diff: string | null; graph: Graph | null;
	review?: Review | null; review_name?: string | null; review_token?: string | null; review_initialized?: boolean;
	truncated: boolean; error: string | null;
};

export default function (pi: ExtensionAPI) {
	let baseline: Baseline = { status: "unavailable", reason: "Session initialization has not completed" };
	let acknowledged: Generation | undefined;
	let root: string | undefined;
	let updates = Promise.resolve();
	let needsRescan = false;
	const lifetime = new AbortController();

	function save(ctx: ExtensionContext, value: Baseline) {
		pi.appendEntry(STATE, { sessionId: ctx.sessionManager.getSessionId(), ...value });
		baseline = value;
	}

	function fail(ctx: ExtensionContext, reason: string) {
		baseline = { status: "unavailable", reason };
		try {
			save(ctx, baseline);
		} catch (error) {
			report(ctx, `Could not persist StackScout baseline failure: ${message(error)}`);
		}
		report(ctx, unavailable(reason));
	}

	async function send(ctx: ExtensionContext, operation: object, signal?: AbortSignal) {
		const signals = [lifetime.signal];
		if (signal) signals.push(signal);
		const combined = AbortSignal.any(signals);
		combined.throwIfAborted();
		root ??= await realpath(ctx.cwd);
		const response = await request(pi, root, operation, combined);
		if (baseline.status === "ready" && response.instance !== baseline.instance) {
			fail(ctx, "Repository service restarted; the original checkpoint was lost");
		}
		if (response.error !== null) throw new Error(response.error);
		return response;
	}

	function requireBaseline() {
		if (baseline.status !== "ready") throw new Error(unavailable(baseline.reason));
		return baseline;
	}

	// All synchronization state changes run in this queue. Graph traversal stays concurrent.
	function enqueue<T>(action: () => Promise<T>, signal?: AbortSignal): Promise<T> {
		const result = updates.then(async () => {
			lifetime.signal.throwIfAborted();
			signal?.throwIfAborted();
			try {
				return await action();
			} catch (error) {
				needsRescan = true;
				throw error;
			}
		});
		updates = result.then(() => {}, () => {});
		return wait(result, signal);
	}

	// Called only inside enqueue; undefined paths request a full reconciliation.
	async function sync(ctx: ExtensionContext, paths: string[] | undefined, signal?: AbortSignal) {
		let operation: object = { op: "sync", paths };
		if (paths === undefined || needsRescan || !acknowledged) operation = { op: "rescan" };
		let response = await send(ctx, operation, signal);
		if (acknowledged && acknowledged.instance !== response.instance) {
			response = await send(ctx, { op: "rescan" }, signal);
		}
		if (acknowledged?.instance === response.instance) checkGeneration(response, acknowledged);
		acknowledged = generation(response);
		needsRescan = false;
		return acknowledged;
	}

	function prepare(ctx: ExtensionContext, signal?: AbortSignal, refresh = false) {
		let paths: string[] | undefined = [];
		if (refresh) paths = undefined;
		return enqueue(() => sync(ctx, paths, signal), signal);
	}

	async function compare(ctx: ExtensionContext, expected: Generation, diagnostics?: boolean, signal?: AbortSignal) {
		const saved = requireBaseline();
		let response: Response;
		try {
			// Never use checkpoint here: it would create a late baseline if the name was forgotten.
			response = await send(ctx, { op: "diff", name: saved.name, diagnostics }, signal);
		} catch (error) {
			if (message(error).startsWith("unknown checkpoint:")) fail(ctx, message(error));
			throw error;
		}
		checkGeneration(response, expected);
		if (!Number.isSafeInteger(response.checkpoint_generation) || response.checkpoint_generation! < 1) {
			throw new Error("Service returned no checkpoint generation");
		}
		if (response.checkpoint_generation !== saved.generation) {
			fail(ctx, "Checkpoint generation changed");
			throw new Error(unavailable("Checkpoint generation changed"));
		}
		if (typeof response.diff !== "string") throw new Error("Service returned no diff");
		return response;
	}

	pi.on("session_start", async (event, ctx) => {
		const sessionId = ctx.sessionManager.getSessionId();
		const entries = ctx.sessionManager.getEntries();
		try {
			const saved = entries.findLast((entry) => entry.type === "custom"
				&& entry.customType === STATE && (entry.data as { sessionId?: string })?.sessionId === sessionId);
			if (saved?.type === "custom") {
				const value = saved.data as Baseline;
				if (value.status === "unavailable" && typeof value.reason === "string") {
					baseline = value;
					report(ctx, unavailable(value.reason));
					return;
				}
				if (value.status !== "ready" || typeof value.root !== "string" || typeof value.name !== "string"
					|| typeof value.instance !== "string" || !Number.isSafeInteger(value.generation)) {
					throw new Error("Invalid saved checkpoint metadata");
				}
				baseline = value;
				root = await realpath(ctx.cwd);
				if (root !== value.root) {
					fail(ctx, "Session repository root changed");
					return;
				}
				const expected = await prepare(ctx, undefined, true);
				if (baseline.status !== "ready") return;
				await compare(ctx, expected);
				return;
			}
			if (event.reason === "reload" || event.reason === "resume"
				|| (event.reason !== "fork" && entries.some((entry) => entry.type === "message"))) {
				throw new Error("This session has no service checkpoint; upgrading requires a new session");
			}
			// Persist intent before I/O so interrupted/failed capture cannot be retried after edits.
			save(ctx, { status: "unavailable", reason: "Initial checkpoint capture did not complete" });
			const expected = await prepare(ctx, undefined, true);
			const name = `pi-${createHash("sha256").update(sessionId).digest("hex")}`;
			const response = await send(ctx, { op: "checkpoint", name });
			checkGeneration(response, expected);
			if (!Number.isSafeInteger(response.checkpoint_generation)
				|| response.checkpoint_generation! < expected.generation
				|| response.checkpoint_generation! > response.generation!) {
				throw new Error("Checkpoint does not cover the initial reconciliation");
			}
			save(ctx, { status: "ready", root: response.root, name, instance: response.instance, generation: response.checkpoint_generation! });
		} catch (error) {
			if (baseline.status === "ready") {
				report(ctx, `StackScout refresh failed: ${message(error)}. Original checkpoint retained; retry a graph query after recovery.`);
			} else {
				fail(ctx, message(error));
			}
		}
	});

	pi.on("session_shutdown", () => {
		// Requests belong to this client; checkpoints and the owner deliberately outlive it.
		lifetime.abort(new Error("StackScout session closed"));
	});

	pi.on("tool_result", async (event, ctx) => {
		if (event.isError || (event.toolName !== "edit" && event.toolName !== "write")) return;
		// A saved write must be acknowledged even if its originating turn was cancelled.
		try {
			await enqueue(async () => {
				root ??= await realpath(ctx.cwd);
				const paths = await syncPaths(root, ctx.cwd, event.input.path);
				if (paths?.length === 0) return;
				await sync(ctx, paths);
			});
		} catch (error) {
			return { content: [...event.content, { type: "text" as const, text: `StackScout sync failed (file write succeeded): ${message(error)}. Graph queries will retry reconciliation.` }] };
		}
	});

	pi.registerTool({
		name: "call_graph",
		label: "Call graph",
		description: "Query saved Rust/TypeScript callers, callees, or impact in the shared repository index. Accepts unique names or language:file::qualified::name IDs. Bounded to 2000 lines / 50 KiB; static results are advisory.",
		promptSnippet: "Find callers, callees, or impact with source locations in the shared index",
		promptGuidelines: ["Use call_graph to drill into symbols from call_graph_diff; use refresh after Bash/external edits when freshness matters."],
		parameters: Type.Object({
			action: StringEnum(["callers", "callees", "impact"] as const),
			symbol: Type.String(),
			depth: Type.Optional(Type.Integer({ minimum: 0, maximum: 32 })),
			limit: Type.Optional(Type.Integer({ minimum: 1, maximum: 256 })),
			refresh: REFRESH,
		}),
		async execute(_id, { action, symbol, depth, limit, refresh }, signal, _onUpdate, ctx) {
			const expected = await prepare(ctx, signal, refresh);
			const response = await send(ctx, { op: action, symbol, depth, limit }, signal);
			checkGeneration(response, expected);
			if (!response.graph) throw new Error("Service returned no graph");
			return output(response, baseline, graphText(response.graph), signal);
		},
	});

	pi.registerTool({
		name: "call_graph_review",
		label: "Scoped call graph review",
		description: "Show changes relevant to explicit repository-relative files or symbols since this scope's last acknowledgement (initially session start). Before/after witness paths include dependencies, callers, removals, and uncertainty. Bounded to 2000 lines / 50 KiB. Does not acknowledge or advance the session baseline.",
		promptSnippet: "Review changes relevant to files/symbols since their last explicit acknowledgement",
		promptGuidelines: ["Use call_graph_review for repeated scoped checks. Inspect warnings and paths before call_graph_ack; acknowledgement applies only to that exact scope and inspected generation, not later writes or other scopes."],
		parameters: Type.Object({
			files: Type.Optional(Type.Array(Type.String(), { maxItems: 16 })),
			symbols: Type.Optional(Type.Array(Type.String(), { maxItems: 16 })),
			refresh: REFRESH,
		}),
		async execute(_id, { files = [], symbols = [], refresh }, signal, _onUpdate, ctx) {
			const saved = requireBaseline();
			const scope = { files: [...new Set(files)].sort(), symbols: [...new Set(symbols)].sort() };
			const name = `${saved.name}-review-${createHash("sha256").update(JSON.stringify(scope)).digest("hex")}`;
			const expected = await prepare(ctx, signal, refresh);
			requireBaseline();
			const response = await send(ctx, { op: "review", name, baseline: saved.name, baseline_generation: saved.generation, scope }, signal);
			checkGeneration(response, expected);
			if (!response.review || typeof response.review_token !== "string") throw new Error("Service returned no scoped review/token; upgrade the owner and start a new session");
			const header = `Review: ${JSON.stringify(name)}; token: ${JSON.stringify(response.review_token)}\n`
				+ (response.review_initialized ? "Review initialized from the original session checkpoint.\n" : "")
				+ "Not acknowledged. Inspect evidence and omissions before call_graph_ack({review, token}). Session baseline is unchanged.\n";
			return output(response, baseline, header + reviewText(response.review), signal);
		},
	});

	pi.registerTool({
		name: "call_graph_ack",
		label: "Acknowledge scoped review",
		description: "Explicitly acknowledge a call_graph_review result using its review name and token. Advances only that scope to the inspected generation, never newer unseen writes. Inspect partial evidence/warnings before acknowledging. Stale/superseded tokens are rejected; retries are safe.",
		parameters: Type.Object({ review: Type.String(), token: Type.String() }),
		async execute(_id, { review, token }, signal, _onUpdate, ctx) {
			const saved = requireBaseline();
			if (!review.startsWith(`${saved.name}-review-`) || !token.startsWith(`${saved.instance}:`)) {
				throw new Error("Review/token does not belong to this session and service instance");
			}
			const response = await send(ctx, { op: "acknowledge", name: review, token }, signal);
			checkGeneration(response, saved);
			return output(response, baseline, `Acknowledged ${JSON.stringify(review)} through generation ${response.checkpoint_generation}. Later changes remain unreviewed; session baseline unchanged.`, signal);
		},
	});

	pi.registerTool({
		name: "call_graph_diff",
		label: "Call graph diff",
		description: "Compare saved Rust/TypeScript call paths against the immutable Pi session-start checkpoint. Includes aggregate workspace changes, not authorship. Bounded to 2000 lines / 50 KiB; service truncation is explicit.",
		promptSnippet: "Compare current call paths with the Pi session-start checkpoint",
		promptGuidelines: ["Use call_graph_diff after call-affecting Rust/TypeScript edits; inspect unintended impact and report missing, noisy, or incorrect results. Use refresh after Bash/external edits when freshness matters."],
		parameters: Type.Object({ refresh: REFRESH, diagnostics: Type.Optional(Type.Boolean()) }),
		async execute(_id, { refresh, diagnostics }, signal, _onUpdate, ctx) {
			signal?.throwIfAborted();
			requireBaseline();
			const expected = await prepare(ctx, signal, refresh);
			const response = await compare(ctx, expected, diagnostics, signal);
			const text = response.diff || "No semantic call-graph changes in the analyzed files since session start.\n";
			return output(response, baseline, "Locations: '-' uses checkpoint; other lines use queried generation. Ranges are definitions at roots, call sites below.\n" + text, signal);
		},
	});
}

function message(error: unknown): string {
	if (error instanceof Error) return error.message;
	return String(error);
}

function unavailable(reason: string): string {
	return `Session-start baseline unavailable: ${reason}. Start a new session to capture a new baseline.`;
}

function report(ctx: ExtensionContext, text: string) {
	if (ctx.hasUI) ctx.ui.notify(text, "error");
	else console.error(text);
}

function generation(response: Response): Generation {
	if (!response.index_complete || response.generation === null) throw new Error("Repository index is not ready");
	return { instance: response.instance, generation: response.generation };
}

function checkGeneration(response: Response, expected: Generation) {
	const current = generation(response);
	if (current.instance !== expected.instance || current.generation < expected.generation) {
		throw new Error("Repository instance/generation changed during request; retry the query (baseline is never recaptured)");
	}
}

async function request(pi: ExtensionAPI, root: string, operation: object, signal: AbortSignal): Promise<Response> {
	signal.throwIfAborted();
	const binary = process.env.STACKSCOUT_BIN || join(root, "target", "debug", "stackscout");
	const result = await pi.exec(binary, ["request", root, JSON.stringify({ version: 1, ...operation })], { cwd: root, signal, timeout: 125_000 });
	signal.throwIfAborted();
	if (result.code !== 0 || result.killed) {
		throw new Error(result.stderr.trim() || "StackScout request failed or timed out; run cargo build before starting Pi");
	}
	const response = JSON.parse(result.stdout) as Response;
	if (!response || response.version !== 1 || response.root !== root || typeof response.instance !== "string"
		|| (response.generation !== null && (!Number.isSafeInteger(response.generation) || response.generation < 1))
		|| typeof response.index_complete !== "boolean" || typeof response.watching !== "boolean"
		|| typeof response.recovery_needed !== "boolean" || typeof response.truncated !== "boolean"
		|| (response.error !== null && typeof response.error !== "string")) {
		throw new Error("Invalid StackScout protocol response or repository root");
	}
	return response;
}

// Match Pi's saved-file path spellings; service discovery enforces ignores/symlink policy.
// [] means no indexed paths; undefined means ignore configuration changed and needs a rescan.
async function syncPaths(root: string, cwd: string, input: unknown): Promise<string[] | undefined> {
	if (typeof input !== "string") throw new Error("Saved-file tool did not provide a path");
	let path = input.replace(/[\u00A0\u2000-\u200A\u202F\u205F\u3000]/g, " ").replace(/^@/, "");
	if (path === "~") path = homedir();
	else if (path.startsWith("~/")) path = join(homedir(), path.slice(2));
	else if (path.startsWith("file://")) path = fileURLToPath(path);
	path = resolve(await realpath(cwd), path);
	const candidates = [path];
	try {
		candidates.push(await realpath(path));
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error;
	}
	const paths = [...new Set(candidates.map((candidate) => relative(root, candidate)))]
		.filter((candidate) => candidate && candidate !== ".." && !candidate.startsWith("../"));
	if (paths.some((candidate) => [".gitignore", ".ignore"].includes(basename(candidate)) || candidate === ".git/info/exclude")) return undefined;
	return paths.filter((candidate) => /\.(rs|ts|tsx|mts|cts)$/.test(candidate) && !/\.d\.(ts|mts|cts)$/.test(candidate));
}

async function wait<T>(promise: Promise<T>, signal?: AbortSignal): Promise<T> {
	signal?.throwIfAborted();
	if (!signal) return promise;
	let abort!: () => void;
	try {
		return await Promise.race([promise, new Promise<never>((_resolve, reject) => {
			abort = () => reject(signal.reason);
			signal.addEventListener("abort", abort, { once: true });
		})]);
	} finally {
		signal.removeEventListener("abort", abort);
	}
}

function range(location: Range): string {
	return `${location.start_line}:${location.start_column}-${location.end_line}:${location.end_column}`;
}

function graphText(graph: Graph): string {
	const lines = graph.symbols.map((symbol) => `depth ${symbol.depth}: ${JSON.stringify(symbol.id)} @ ${JSON.stringify(symbol.file)}:${range(symbol.location)}`);
	for (const call of graph.calls) {
		lines.push(`${JSON.stringify(call.caller)} @ ${range(call.location)} -> ${JSON.stringify(call.callee ?? call.target)} [${call.resolution}]`);
	}
	return lines.join("\n");
}

function reviewText(review: Review): string {
	const lines: string[] = [];
	for (const witness of review.witnesses) {
		lines.push(`${witness.side} ${witness.relation} ${witness.change}: ${JSON.stringify(witness.symbol.id)} @ ${JSON.stringify(witness.symbol.file)}:${range(witness.symbol.location)} (scope ${JSON.stringify(witness.scope)})`);
		for (const call of witness.path) lines.push(`  path: ${JSON.stringify(call.caller)} @ ${range(call.location)} -> ${JSON.stringify(call.callee ?? call.target)} [${call.resolution}]`);
		for (const call of witness.calls) lines.push(`  call: ${JSON.stringify(call.caller)} @ ${range(call.location)} -> ${JSON.stringify(call.callee ?? call.target)} [${call.resolution}]`);
	}
	if (!review.witnesses.length) lines.push("No relevant semantic changes found in the bounded analyzed graph; inspect omissions below.");
	for (const boundary of review.boundaries) lines.push(`${boundary.side} boundary: ${JSON.stringify(boundary.call.caller)} @ ${range(boundary.call.location)} -> ${JSON.stringify(boundary.call.callee ?? boundary.call.target)} [${boundary.call.resolution}]`);
	for (const missing of review.missing) lines.push(`${missing.side} missing scope: ${JSON.stringify(missing.selector)}: ${missing.reason}`);
	for (const issue of review.analysis_issues) lines.push(`Before/after analysis issue: ${JSON.stringify(issue)}`);
	return lines.join("\n");
}

async function output(response: Response, baseline: Baseline, body: string, signal?: AbortSignal) {
	const lines = [
		`Repository: ${JSON.stringify(response.root)}; instance: ${JSON.stringify(response.instance)}; generation: ${response.generation}; checkpoint: ${response.checkpoint_generation ?? "none"}`,
		`Index complete: ${response.index_complete}; analysis issues: ${response.analysis_issue_count}; watching: ${response.watching}; recovery needed: ${response.recovery_needed}; update in progress: ${response.progress !== null}`,
		"Freshness: explicit saved-file writes are acknowledged; Bash/external writes are eventually consistent even with a healthy watcher. Use refresh:true to reconcile.",
		WARNING,
	];
	if (response.watcher_error) lines.push(`Watcher error: ${JSON.stringify(response.watcher_error)}`);
	if (response.last_update_error) lines.push(`Last update error: ${JSON.stringify(response.last_update_error)}`);
	if (response.truncated) lines.push("[Service result truncated: omitted evidence is unavailable in this response. Drill down with call_graph and narrower limits/selectors.]");
	for (const issue of response.analysis_issues) lines.push(`Analysis issue: ${JSON.stringify(issue)}`);
	lines.push("", body);
	const text = lines.join("\n");
	const cut = truncateHead(text, { maxBytes: DEFAULT_MAX_BYTES - 1024, maxLines: DEFAULT_MAX_LINES - 4 });
	let content = cut.content;
	let responsePath: string | undefined;
	if (cut.truncated) {
		const directory = await mkdtemp(join(tmpdir(), "stackscout-pi-"));
		responsePath = join(directory, "response.txt");
		await writeFile(responsePath, text, "utf8");
		content += `\n\n[Pi display truncated. Saved response: ${responsePath}. Service-side omissions are not recoverable from this file.]`;
	}
	signal?.throwIfAborted();
	return {
		content: [{ type: "text" as const, text: content }],
		details: { ...response, baseline, display_truncated: cut.truncated, responsePath },
	};
}
