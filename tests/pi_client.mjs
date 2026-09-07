// Isolated real Pi SDK process. Only the model is scripted; loader, sessions, tools,
// lifecycle, filesystem, subprocess execution, and StackScout service are real.
import { createInterface } from "node:readline";
import { channel } from "node:diagnostics_channel";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { pathToFileURL } from "node:url";

const [pkg, cwd, agentDir, extension, sessionFile] = process.argv.slice(2);
// Use the same bundled runtime as the installed CLI when available.
let entry = join(pkg, "dist/bundle/index.js");
if (!existsSync(entry)) entry = join(pkg, "dist/index.js");
const sdk = await import(pathToFileURL(entry).href);
const ai = await import(pathToFileURL(join(pkg, "../pi-ai/dist/index.js")).href);
const settings = sdk.SettingsManager.inMemory({ compaction: { enabled: false }, retry: { enabled: false } });
const modelRuntime = await sdk.ModelRuntime.create({
	authPath: join(agentDir, "auth.json"), modelsPath: join(agentDir, "models.json"),
	modelsStorePath: join(agentDir, "models-store.json"),
});
let planned = [];
const model = {
	id: "scripted", name: "Scripted", provider: "stackscout-test", api: "openai-completions",
	baseUrl: "http://127.0.0.1:1", reasoning: false, input: ["text"],
	cost: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0 }, contextWindow: 200000, maxTokens: 4096,
};
const loader = new sdk.DefaultResourceLoader({
	cwd, agentDir, settingsManager: settings, noExtensions: true,
	additionalExtensionPaths: [extension], noSkills: true, noPromptTemplates: true, noThemes: true, noContextFiles: true,
	extensionFactories: [(pi) => pi.registerProvider(model.provider, {
		api: model.api, baseUrl: model.baseUrl, apiKey: "not-a-real-key", models: [model],
		streamSimple() {
			const content = planned;
			planned = [];
			const message = {
				role: "assistant", content, api: model.api, provider: model.provider, model: model.id,
				stopReason: content.length ? "toolUse" : "stop", timestamp: Date.now(),
				usage: { input: 0, output: 0, cacheRead: 0, cacheWrite: 0, totalTokens: 0, cost: { ...model.cost, total: 0 } },
			};
			const stream = ai.createAssistantMessageEventStream();
			stream.push({ type: "start", partial: message });
			stream.push({ type: "done", reason: message.stopReason, message });
			stream.end();
			return stream;
		},
	})],
});
// Observe real socket lifetimes without replacing the transport or the SDK.
channel("net.client.socket").subscribe(({ socket }) => {
	socket.once("connect", () => process.send?.({ socket: "open" }));
	socket.once("close", () => process.send?.({ socket: "close" }));
});
await loader.reload();
const sessionManager = sessionFile
	? sdk.SessionManager.open(sessionFile, join(agentDir, "sessions"))
	: sdk.SessionManager.create(cwd, join(agentDir, "sessions"));
const { session, extensionsResult } = await sdk.createAgentSession({
	cwd, agentDir, resourceLoader: loader, modelRuntime, model, sessionManager, settingsManager: settings,
});
if (extensionsResult.errors.length) throw new Error(JSON.stringify(extensionsResult.errors));
await session.bindExtensions({ mode: "json", onError: (error) => { throw new Error(error.message); } });

function info() {
	return {
		sessionId: session.sessionId, sessionFile: session.sessionFile,
		baseline: sessionManager.getEntries().findLast((entry) => entry.type === "custom" && entry.customType === "stackscout-session-v1")?.data,
		tools: session.agent.state.tools.map((tool) => tool.name),
	};
}
function reply(value) { process.stdout.write(JSON.stringify(value) + "\n"); }
reply(info());
// The parent sends this only after observing a real, blocked StackScout connection.
process.on("SIGUSR2", () => { void session.abort(); });

for await (const line of createInterface({ input: process.stdin })) {
	const command = JSON.parse(line);
	try {
		if (command.op === "close") break;
		if (command.op === "reload") {
			await session.reload();
			reply(info());
			continue;
		}
		planned = [{ type: "toolCall", id: crypto.randomUUID(), name: command.name, arguments: command.params ?? {} }];
		const start = session.messages.length;
		await session.prompt("Execute the scripted tool.");
		const result = session.messages.slice(start).findLast((message) => message.role === "toolResult");
		if (!result) throw new Error(JSON.stringify(session.messages.slice(start)));
		reply(result);
	} catch (error) {
		reply({ error: String(error) });
	}
}
await session.extensionRunner.emit({ type: "session_shutdown", reason: "quit" });
session.dispose();
if (process.connected) process.disconnect();
