import { createHash } from "node:crypto";
import { copyFile, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join } from "node:path";
import { tmpdir } from "node:os";
import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import {
	DEFAULT_MAX_BYTES,
	DEFAULT_MAX_LINES,
	formatSize,
	truncateHead,
} from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";

const READY = ".ready-v2";

export default function (pi: ExtensionAPI) {
	let baseline: Promise<string> | undefined;

	pi.on("session_start", (_event, ctx) => {
		baseline = ensureBaseline(pi, ctx.cwd, ctx.sessionManager.getSessionId());
		return baseline.then(() => undefined);
	});

	pi.registerTool({
		name: "call_graph_diff",
		label: "Call graph diff",
		description: `Compare the repository's current Rust and TypeScript call graph with its Pi session-start baseline. Output is truncated to ${DEFAULT_MAX_LINES} lines or ${formatSize(DEFAULT_MAX_BYTES)}.`,
		promptSnippet: "Compare current call paths with the Pi session-start baseline",
		promptGuidelines: [
			"Use call_graph_diff after edits that may change Rust or TypeScript functions or calls; treat missing, noisy, or incorrect results as StackScout dogfooding findings.",
		],
		parameters: Type.Object({}),
		async execute(_toolCallId, _params, signal, _onUpdate, ctx) {
			const snapshot = await (baseline ??=
				ensureBaseline(pi, ctx.cwd, ctx.sessionManager.getSessionId()));
			const result = await pi.exec(
				"cargo",
				[
					"run",
					"--quiet",
					"--manifest-path",
					join(ctx.cwd, "Cargo.toml"),
					"--",
					"diff-dir",
					snapshot,
					ctx.cwd,
				],
				{ cwd: ctx.cwd, signal },
			);
			if (result.code !== 0) {
				throw new Error(result.stderr.trim() || result.stdout.trim() || "StackScout failed");
			}

			const output = result.stdout || "No semantic call-graph changes since session start.\n";
			const truncated = truncateHead(output, {
				maxBytes: DEFAULT_MAX_BYTES,
				maxLines: DEFAULT_MAX_LINES,
			});
			let text = truncated.content;
			let fullOutputPath: string | undefined;
			if (truncated.truncated) {
				const directory = await mkdtemp(join(tmpdir(), "stackscout-pi-"));
				fullOutputPath = join(directory, "diff.txt");
				await writeFile(fullOutputPath, output, "utf8");
				text += `\n\n[Output truncated. Full output: ${fullOutputPath}]`;
			}

			return {
				content: [{ type: "text", text }],
				details: { baseline: snapshot, fullOutputPath },
			};
		},
	});
}

async function ensureBaseline(pi: ExtensionAPI, cwd: string, sessionId: string): Promise<string> {
	const key = createHash("sha256").update(sessionId).digest("hex").slice(0, 16);
	const baseline = join(cwd, ".pi", "stackscout-baselines", key);
	try {
		await readFile(join(baseline, READY));
		return baseline;
	} catch {
		await rm(baseline, { recursive: true, force: true });
	}

	const files = await pi.exec(
		"rg",
		[
			"--files",
			"--null",
			"--glob",
			"*.rs",
			"--glob",
			"*.ts",
			"--glob",
			"*.tsx",
			"--glob",
			"*.mts",
			"--glob",
			"*.cts",
			"--glob",
			"!*.d.ts",
			"--glob",
			"!*.d.mts",
			"--glob",
			"!*.d.cts",
		],
		{ cwd },
	);
	if (files.code !== 0 && files.code !== 1) {
		throw new Error(files.stderr.trim() || "Could not list source files for StackScout baseline");
	}

	await mkdir(baseline, { recursive: true });
	for (const path of files.stdout.split("\0").filter(Boolean)) {
		const destination = join(baseline, path);
		await mkdir(dirname(destination), { recursive: true });
		await copyFile(join(cwd, path), destination);
	}
	await writeFile(join(baseline, READY), "", "utf8");
	return baseline;
}
