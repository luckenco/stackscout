import assert from "node:assert/strict";
import { test, type TestContext } from "node:test";
import { chmod, chown, mkdir, mkdtemp, realpath, rename, rm, symlink, writeFile } from "node:fs/promises";
import { createServer, type Socket } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { exchange, Unavailable } from "../.pi/transport.ts";

// Real Unix sockets; no Pi SDK or credentials needed.
async function owner(t: TestContext, reply: (socket: Socket) => void) {
	const root = await realpath(await mkdtemp(join(tmpdir(), "ss-wire-")));
	await mkdir(join(root, ".stackscout"), { mode: 0o700 });
	const socket = join(root, ".stackscout/service.sock");
	const server = createServer(client => {
		client.on("error", () => {}); // Deliberately rejected frames can reset the peer.
		reply(client);
	});
	await new Promise<void>((resolve, reject) => {
		server.once("error", reject);
		server.listen(socket, resolve);
	});
	await chmod(socket, 0o600);
	t.after(async () => {
		await new Promise<void>(resolve => server.close(() => resolve()));
		await rm(root, { recursive: true, force: true });
	});
	return { root, socket };
}

const status = { op: "status" };
const active = new AbortController().signal;

test("UTF-8 may span chunks; frame delimiters are LF only", async t => {
	const endpoint = await owner(t, socket => socket.once("data", data => {
		assert.deepEqual(JSON.parse(data.toString()), { version: 1, op: "status" });
		const bytes = Buffer.from('{"diff":"α\u2028β"}\n');
		const split = bytes.indexOf(Buffer.from("α")) + 1;
		socket.write(bytes.subarray(0, split));
		setTimeout(() => socket.end(bytes.subarray(split)), 10);
	}));
	assert.deepEqual(await exchange(endpoint, status, active), { diff: "α\u2028β" });
});

for (const [name, bytes, error] of [
	["incomplete", Buffer.from('{}'), /Incomplete/],
	["invalid JSON", Buffer.from('nope\n'), /JSON/],
	["invalid UTF-8", Buffer.from([0x22, 0xff, 0x22, 10]), /encoded data/],
	["BOM", Buffer.from('\ufeff{}\n'), /JSON/],
	["oversized", Buffer.from('"' + "x".repeat(1024 * 1024) + '"\n'), /exceeds 1 MiB/],
] as const) {
	test(`rejects ${name} replies without making them retryable`, async t => {
		const endpoint = await owner(t, socket => socket.once("data", () => socket.end(bytes)));
		await assert.rejects(exchange(endpoint, status, active), failure => {
			assert.ok(!(failure instanceof Unavailable));
			assert.match(String(failure), error);
			return true;
		});
	});
}

test("request bounds count encoded bytes; oversized and pre-cancelled requests never connect", async t => {
	let connections = 0;
	const endpoint = await owner(t, () => { connections++; });
	await assert.rejects(exchange(endpoint, { name: "α".repeat(32768) }, active), /exceeds 64 KiB/);
	await assert.rejects(exchange(endpoint, status, AbortSignal.abort(new Error("cancelled"))), /cancelled/);
	assert.equal(connections, 0);
});

test("missing endpoints are retryable before any request is sent", async t => {
	const endpoint = await owner(t, () => assert.fail("must not connect"));
	await rm(endpoint.socket);
	await assert.rejects(exchange(endpoint, status, active), Unavailable);
});

for (const kind of ["directory mode", "socket mode", "symlink directory", "symlink socket", "regular file"]) {
	test(`refuses ${kind} on every connection`, async t => {
		let connections = 0;
		const endpoint = await owner(t, socket => {
			connections++;
			socket.once("data", () => socket.end('{}\n'));
		});
		await exchange(endpoint, status, active);
		const directory = join(endpoint.root, ".stackscout");
		if (kind === "directory mode") await chmod(directory, 0o755);
		else if (kind === "socket mode") await chmod(endpoint.socket, 0o666);
		else if (kind === "symlink directory") {
			await rename(directory, directory + "-real");
			await symlink(directory + "-real", directory);
		} else {
			await rename(endpoint.socket, endpoint.socket + "-real");
			if (kind === "symlink socket") await symlink(endpoint.socket + "-real", endpoint.socket);
			else await writeFile(endpoint.socket, "", { mode: 0o600 });
		}
		await assert.rejects(exchange(endpoint, status, active), /must be/);
		assert.equal(connections, 1);
	});
}

test("refuses an unowned socket", { skip: process.geteuid!() !== 0 }, async t => {
	const endpoint = await owner(t, () => assert.fail("must not connect"));
	await chown(endpoint.socket, 1, 1);
	await assert.rejects(exchange(endpoint, status, active), /owned/);
});

test("cancellation closes a connected socket, without retrying a possibly sent mutation", async t => {
	const received = Promise.withResolvers<void>();
	const closed = Promise.withResolvers<void>();
	let connections = 0;
	const endpoint = await owner(t, socket => {
		connections++;
		socket.once("data", () => received.resolve());
		socket.once("close", () => closed.resolve());
	});
	const controller = new AbortController();
	const pending = exchange(endpoint, { op: "rescan" }, controller.signal);
	await received.promise;
	controller.abort(new Error("cancelled"));
	await assert.rejects(pending, /cancelled/);
	await closed.promise;
	assert.equal(connections, 1);
});

test("response deadline is absolute, not extended by partial replies", async t => {
	const received = Promise.withResolvers<Socket>();
	const endpoint = await owner(t, socket => socket.once("data", () => received.resolve(socket)));
	t.mock.timers.enable({ apis: ["setTimeout"] });
	const pending = exchange(endpoint, status, active);
	const socket = await received.promise;
	t.mock.timers.tick(119_000);
	socket.write('{');
	await new Promise<void>(resolve => setImmediate(resolve));
	t.mock.timers.tick(1000);
	await assert.rejects(pending, /response deadline/);
});
