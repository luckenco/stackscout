// Real-model, paired Pi trials. Opt-in: uses configured provider credentials; see docs/evaluation.md.
import assert from "node:assert/strict";
import { fork, execFileSync, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, readFileSync, writeFileSync, mkdirSync, readdirSync, copyFileSync, appendFileSync, realpathSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL, fileURLToPath } from "node:url";
import { files, prepare, implement, guidance, shippingChange, telemetryChange, acceptance } from "./workflow-fixture.mjs";

const script = fileURLToPath(import.meta.url);
const project = resolve(fileURLToPath(new URL("..", import.meta.url)));
const binary = resolve(process.env.STACKSCOUT_BIN ?? join(project, "target/release/stackscout"));
const hash = (text) => createHash("sha256").update(text).digest("hex");

if (process.argv[2] === "--client") {
	const config = JSON.parse(process.argv[3]);
	let entry = join(config.pkg, "dist/bundle/index.js");
	if (!existsSync(entry)) entry = join(config.pkg, "dist/index.js");
	const sdk = await import(pathToFileURL(entry).href);
	const modelRuntime = await sdk.ModelRuntime.create();
	const model = modelRuntime.getModel(config.provider, config.model);
	if (!model) throw new Error("Model not found: " + config.provider + "/" + config.model);
	const settings = sdk.SettingsManager.inMemory({ compaction: { enabled: false }, retry: { enabled: false } });
	let calls = 0;
	const loader = new sdk.DefaultResourceLoader({
		cwd: config.root, agentDir: config.agentDir, settingsManager: settings,
		noExtensions: true, noSkills: true, noPromptTemplates: true, noThemes: true, noContextFiles: true,
		additionalExtensionPaths: config.graph ? [join(project, ".pi/extensions/stackscout.ts")] : [],
		extensionFactories: [(pi) => pi.on("tool_call", () => {
			if (++calls > 50) return { block: true, reason: "Evaluation tool budget exhausted", terminate: true };
		})],
	});
	await loader.reload();
	const { session, extensionsResult } = await sdk.createAgentSession({
		cwd: config.root, agentDir: config.agentDir, modelRuntime, model, thinkingLevel: config.thinking,
		resourceLoader: loader, settingsManager: settings,
		sessionManager: sdk.SessionManager.create(config.root, join(config.agentDir, "sessions")),
	});
	assert.deepEqual(extensionsResult.errors, []);
	await session.bindExtensions({ mode: "json", onError: (error) => { throw new Error(error.message); } });
	session.subscribe((event) => {
		if (event.type !== "message_end") return;
		const message = event.message;
		// Retain tool/text evidence and usage, not provider-internal thinking/signature blocks.
		let content = message.content;
		if (Array.isArray(content)) content = content.filter((part) => part.type !== "thinking");
		process.send({ event: { at: Date.now(), ...message, content, details: undefined } });
	});
	process.send({ ready: true, tools: session.agent.state.tools.map((tool) => tool.name),
		thinking: session.thinkingLevel, session: session.sessionFile });
	process.on("message", async (command) => {
		if (command.close) {
			await session.extensionRunner.emit({ type: "session_shutdown", reason: "quit" });
			session.dispose();
			process.disconnect();
			return;
		}
		let timedOut = false;
		const timer = setTimeout(() => { timedOut = true; void session.abort(); }, 180_000);
		try {
			await session.prompt(command.prompt);
			if (timedOut) throw new Error("Evaluation prompt deadline exceeded");
			if (calls > 50) throw new Error("Evaluation tool budget exhausted");
			const last = session.messages.findLast((message) => message.role === "assistant");
			if (!last || ["error", "aborted", "length"].includes(last.stopReason)) throw new Error(last?.errorMessage ?? "Incomplete model response");
			process.send({ done: true });
		} catch (error) { process.send({ error: String(error) }); }
		finally { clearTimeout(timer); }
	});
} else {
	const [pkgArg, provider, model, outputArg, repetitions = "2"] = process.argv.slice(2);
	if (!outputArg || !Number.isInteger(Number(repetitions)) || Number(repetitions) < 1) {
		throw new Error("usage: node scripts/workflow.mjs <pi-package> <provider> <model> <new-output-dir> [repetitions=2]");
	}
	const pkg = resolve(pkgArg);
	const output = resolve(outputArg);
	const thinking = process.env.STACKSCOUT_EVAL_THINKING ?? "low";
	let report = { schema: 1, provider, model, thinking, repetitions: Number(repetitions),
		node: process.version, pi: JSON.parse(readFileSync(join(pkg, "package.json"))).version,
		platform: process.platform, arch: process.arch, binary_sha256: hash(readFileSync(binary)),
		adapter_sha256: hash(readFileSync(join(project, ".pi/extensions/stackscout.ts"))),
		fixture_sha256: hash(JSON.stringify(files)), prompts: { prepare, implement, guidance }, trials: [] };
	if (process.env.STACKSCOUT_EVAL_RESUME === "1") {
		const previous = JSON.parse(readFileSync(join(output, "summary.json"), "utf8"));
		assert.deepEqual({ ...previous, trials: [] }, report, "Resume requires identical trial settings");
		report = previous;
	} else mkdirSync(output); // Refuse overwriting an earlier trial.

	function request(root, operation) {
		const text = execFileSync(binary, ["request", root, JSON.stringify({ version: 1, ...operation })],
			{ env: { ...process.env, STACKSCOUT_WATCH: "0" }, stdio: ["ignore", "pipe", "inherit"], timeout: 30_000 });
		const result = JSON.parse(text);
		assert.equal(result.error, null);
		return result;
	}

	async function client(root, directory, graph, role, events) {
		const agentDir = join(directory, role);
		mkdirSync(agentDir);
		const child = fork(script, ["--client", JSON.stringify({ pkg, provider, model, thinking, root, agentDir, graph })], {
			env: { ...process.env, STACKSCOUT_BIN: binary, STACKSCOUT_WATCH: "0" }, silent: true,
		});
		const exited = new Promise((resolveExit) => child.once("exit", (code, signal) => resolveExit({ code, signal })));
		let pending;
		child.stderr.on("data", (text) => appendFileSync(join(directory, `${role}.stderr`), text));
		child.stdout.on("data", (text) => appendFileSync(join(directory, `${role}.stdout`), text));
		child.on("message", (message) => {
			if (message.event) {
				const event = { ...message.event, agent: role };
				events.push(event);
				appendFileSync(join(directory, "events.jsonl"), JSON.stringify(event) + "\n");
			} else if (pending) {
				const { resolveReply, reject } = pending;
				pending = undefined;
				if (message.error) reject(new Error(message.error));
				else resolveReply(message);
			}
		});
		child.on("exit", (code) => pending?.reject(new Error(`${role} exited ${code}; inspect stderr`)));
		child.on("error", (error) => pending?.reject(error));
		const receive = () => new Promise((resolveReply, reject) => { pending = { resolveReply, reject }; });
		try {
			const ready = await Promise.race([receive(), new Promise((_, reject) => {
				setTimeout(() => reject(new Error("Pi startup deadline exceeded")), 30_000).unref();
			})]);
			writeFileSync(join(directory, `${role}.ready.json`), JSON.stringify(ready, null, 2));
			assert.equal(ready.thinking, thinking);
			return { async prompt(prompt) { const reply = receive(); child.send({ prompt }); await reply; },
				async close() {
					if (child.connected) child.send({ close: true });
					const timer = setTimeout(() => child.kill("SIGKILL"), 10_000);
					const status = await exited;
					clearTimeout(timer);
					assert.deepEqual(status, { code: 0, signal: null }, `${role} shutdown`);
				} };
		} catch (error) { child.kill("SIGKILL"); await exited; throw error; }
	}

	for (let repeat = 0; repeat < Number(repetitions); repeat++) {
		for (const scenario of ["single", "relevant", "unrelated"]) {
			// Counterbalance treatment order; each trial starts with identical files and empty sessions.
			for (const graph of repeat % 2 === 0 ? [false, true] : [true, false]) {
				const id = `${repeat}-${scenario}-${graph ? "graph" : "control"}`;
				if (report.trials.some((trial) => trial.id === id)) continue; // Keep failures in the denominator.
				console.error("Trial " + id);
				const directory = join(output, id);
				const root = join(directory, "repo");
				mkdirSync(root, { recursive: true });
				mkdirSync(join(root, ".baseline"));
				for (const [name, content] of Object.entries(files)) {
					writeFileSync(join(root, name), content);
					writeFileSync(join(root, ".baseline", name), content);
				}
				const events = [];
				const clients = [];
				const trial = { id, repeat, scenario, graph, started_at: Date.now() };
				let changedAt;
				let ownerPid;
				try {
					// Stop only this isolated service; never touch the developer's index.
					if (graph) ownerPid = Number(request(root, { op: "sync", paths: [] }).instance.split("-")[0]);
					const start = Date.now();
					const reviewer = await client(root, directory, graph, "reviewer", events);
					clients.push(reviewer);
					trial.reviewer_startup_ms = Date.now() - start;
					await reviewer.prompt(prepare);
					for (const [name, content] of Object.entries(files)) assert.equal(readFileSync(join(root, name), "utf8"), content, `preparation changed ${name}`);
					trial.prepared_at = Date.now();
					if (scenario !== "single") {
						const start = Date.now();
						const writer = await client(root, directory, graph, "writer", events);
						clients.push(writer);
						trial.writer_startup_ms = Date.now() - start;
						if (graph) trial.shared_files_parsed = request(root, { op: "status" }).files_parsed;
						let prompt = `Work only in this workspace. Replace telemetry.ts with exactly this source, preserving its final newline:\n${telemetryChange}\n`;
						if (scenario === "relevant") prompt += `Also replace shipping.ts with exactly this source, preserving its final newline:\n${shippingChange}\n`;
						await writer.prompt(prompt + "Only write those files; another agent owns checkout.ts. Reply briefly.");
						assert.equal(readFileSync(join(root, "telemetry.ts"), "utf8"), telemetryChange);
						if (scenario === "relevant") assert.equal(readFileSync(join(root, "shipping.ts"), "utf8"), shippingChange);
						changedAt = Date.now();
						trial.writer_complete_at = changedAt;
					}
					await reviewer.prompt(implement + (graph ? "\n" + guidance : ""));
					trial.completed_at = Date.now();
					assert.equal(readFileSync(join(root, "shipping.ts"), "utf8"), scenario === "relevant" ? shippingChange : files["shipping.ts"]);
					assert.equal(readFileSync(join(root, "telemetry.ts"), "utf8"), scenario === "single" ? files["telemetry.ts"] : telemetryChange);
					const result = spawnSync(process.execPath, ["--input-type=module", "-e", acceptance(scenario === "relevant" ? 200 : 100)],
						{ cwd: root, encoding: "utf8", timeout: 10_000 });
					trial.acceptance = { passed: result.status === 0, stdout: result.stdout, stderr: result.stderr };
					const allowed = new Set(["checkout.ts", "report.json"]);
					if (scenario !== "single") allowed.add("telemetry.ts");
					if (scenario === "relevant") allowed.add("shipping.ts");
					trial.unexpected_files = readdirSync(root).filter((name) => ![".baseline", ".stackscout"].includes(name) && !allowed.has(name)
						&& (!files[name] || readFileSync(join(root, name), "utf8") !== files[name]));
					for (const [name, content] of Object.entries(files)) {
						assert.ok(existsSync(join(root, name)), `deleted ${name}`);
						assert.equal(readFileSync(join(root, ".baseline", name), "utf8"), content, `changed baseline ${name}`);
					}
					const alerts = JSON.parse(readFileSync(join(root, "report.json"), "utf8")).relevant;
					assert.ok(Array.isArray(alerts) && alerts.every((file) => typeof file === "string"));
					const expected = scenario === "relevant" ? ["shipping.ts"] : [];
					trial.alerts = alerts;
					trial.false_alerts = alerts.filter((file) => !expected.includes(file));
					trial.missed_alerts = expected.filter((file) => !alerts.includes(file));
					copyFileSync(join(root, "checkout.ts"), join(directory, "checkout.final.ts"));
					trial.patch = spawnSync("diff", ["-u", join(root, ".baseline/checkout.ts"), join(root, "checkout.ts")], { encoding: "utf8" }).stdout;
				} catch (error) { trial.error = String(error); }
				finally {
					const closed = await Promise.allSettled(clients.map((app) => app.close()));
					for (const result of closed) if (result.status === "rejected") trial.error = String(result.reason);
					if (ownerPid) {
						try { process.kill(ownerPid, "SIGTERM"); }
						catch (error) { trial.error = String(error); }
					}
				}
				trial.wall_ms = (trial.completed_at ?? Date.now()) - trial.started_at;
				trial.metrics = {};
				for (const agent of ["reviewer", "writer"]) {
					const messages = events.filter((event) => event.agent === agent);
					const tools = messages.filter((event) => event.role === "toolResult");
					const usage = messages.filter((event) => event.role === "assistant").map((event) => event.usage);
					trial.metrics[agent] = { model_tokens: usage.reduce((total, usage) => total + usage.totalTokens, 0),
						input_tokens: usage.reduce((total, usage) => total + usage.input, 0),
						cache_read_tokens: usage.reduce((total, usage) => total + usage.cacheRead, 0),
						output_tokens: usage.reduce((total, usage) => total + usage.output, 0),
						tool_calls: tools.length, tool_output_bytes: tools.reduce((total, event) => total + Buffer.byteLength(JSON.stringify(event.content)), 0),
						followup_read_calls: tools.filter((event) => event.toolName === "read" && event.at > trial.prepared_at).length,
						followup_bash_calls: tools.filter((event) => event.toolName === "bash" && event.at > trial.prepared_at).length,
						graph_calls: tools.filter((event) => event.toolName.startsWith("call_graph")).length };
				}
				// First successful explicit alert, not first evidence arrival or an inferred thought.
				const alerts = events.filter((event) => event.agent === "reviewer" && event.role === "assistant" && event.at >= changedAt)
					.flatMap((event) => event.content ?? []).filter((part) => {
						if (part.type !== "toolCall" || part.name !== "write") return false;
						try {
							const relevant = JSON.parse(part.arguments.content).relevant;
							return Array.isArray(relevant) && relevant.includes("shipping.ts")
								&& realpathSync(resolve(root, part.arguments.path.replace(/^@/, ""))) === realpathSync(join(root, "report.json"));
						} catch { return false; }
					});
				const detection = events.find((event) => event.role === "toolResult" && !event.isError && alerts.some((call) => call.id === event.toolCallId));
				trial.detection_ms = scenario === "relevant" && detection ? detection.at - changedAt : null;
				report.trials.push(trial);
				writeFileSync(join(output, "summary.json"), JSON.stringify(report, null, 2) + "\n");
				if (trial.error) throw new Error(`${id}: ${trial.error}; partial results saved`);
			}
		}
	}
	if (report.trials.some((trial) => trial.error)) process.exitCode = 1;
}
