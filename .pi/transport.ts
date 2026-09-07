import { lstat, realpath } from "node:fs/promises";
import { connect } from "node:net";
import { dirname, join } from "node:path";

export type Endpoint = { readonly root: string; readonly socket: string };
// Only pre-connect absence/refusal permits startup and retry. Sent mutations are never replayed.
export class Unavailable extends Error {}

export async function exchange(endpoint: Endpoint, operation: object, signal: AbortSignal): Promise<unknown> {
	signal.throwIfAborted();
	const bytes = Buffer.from(JSON.stringify({ version: 1, ...operation }) + "\n");
	if (bytes.length > 64 * 1024) throw new Error("StackScout request exceeds 64 KiB");
	try {
		if (await realpath(endpoint.root) !== endpoint.root) throw new Error("Repository root is no longer canonical");
		for (const path of new Set([join(endpoint.root, ".stackscout"), dirname(endpoint.socket)])) {
			const info = await lstat(path);
			if (!info.isDirectory() || info.uid !== process.geteuid!() || (info.mode & 0o777) !== 0o700) {
				throw new Error("Service directory must be real, owned, and mode 0700");
			}
		}
		const info = await lstat(endpoint.socket);
		if (!info.isSocket() || info.uid !== process.geteuid!() || (info.mode & 0o777) !== 0o600) {
			throw new Error("Service endpoint must be an owned, mode-0600 socket");
		}
	} catch (error) {
		if ((error as NodeJS.ErrnoException).code === "ENOENT") throw new Unavailable("Service endpoint is missing", { cause: error });
		throw error;
	}
	signal.throwIfAborted();
	return new Promise((resolve, reject) => {
		const socket = connect(endpoint.socket);
		const chunks: Buffer[] = [];
		let size = 0;
		let connected = false;
		let finished = false;
		let failure: unknown;
		let response: unknown;
		let writing: ReturnType<typeof setTimeout> | undefined;
		const deadline = setTimeout(() => finish(new Error("StackScout response deadline exceeded")), 120_000);
		const abort = () => finish(signal.reason);
		function finish(error?: unknown, value?: unknown) {
			if (finished) return;
			finished = true;
			failure = error;
			response = value;
			socket.destroy();
		}
		signal.addEventListener("abort", abort, { once: true });
		socket.once("close", () => {
			clearTimeout(deadline);
			clearTimeout(writing);
			signal.removeEventListener("abort", abort);
			// Settle only after the OS handle closes, including cancellation.
			if (failure !== undefined) reject(failure);
			else if (!finished) reject(new Error("Incomplete StackScout response"));
			else resolve(response);
		});
		socket.once("error", (error: NodeJS.ErrnoException) => {
			if (!connected && ["ENOENT", "ECONNREFUSED"].includes(error.code ?? "")) {
				finish(new Unavailable("Service owner is unavailable", { cause: error }));
			} else finish(error);
		});
		socket.once("connect", () => {
			connected = true;
			writing = setTimeout(() => finish(new Error("StackScout write deadline exceeded")), 5000);
			socket.write(bytes, error => {
				clearTimeout(writing);
				if (error) finish(error);
			});
		});
		socket.on("data", (chunk: Buffer) => {
			if (finished) return;
			const end = chunk.indexOf(10);
			if (end !== -1) chunk = chunk.subarray(0, end + 1);
			size += chunk.length;
			if (size > 1024 * 1024) return finish(new Error("StackScout response exceeds 1 MiB"));
			chunks.push(chunk);
			if (end === -1) return;
			try {
				const text = new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(Buffer.concat(chunks, size));
				finish(undefined, JSON.parse(text));
			} catch (error) { finish(error); }
		});
		socket.once("end", () => finish(new Error("Incomplete StackScout response")));
	});
}
