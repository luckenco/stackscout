import assert from "node:assert/strict";
import { test } from "node:test";
import { mkdtempSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { files, shippingChange, acceptance } from "./workflow-fixture.mjs";

test("acceptance rejects the old implementation and stale shipping assumptions", () => {
	const root = mkdtempSync(join(tmpdir(), "ss-workflow-test-"));
	try {
		for (const [name, content] of Object.entries(files)) writeFileSync(join(root, name), content);
		for (const objectShipping of [false, true]) {
			writeFileSync(join(root, "shipping.ts"), objectShipping ? shippingChange : files["shipping.ts"]);
			const rate = objectShipping ? 200 : 100;
			for (const [body, passes] of [
				["return subtotal + shipping(weight);", false],
				["return Math.max(0, subtotal - discount) + shipping(weight);", false],
				["const charge = shipping(weight); return Math.max(0, subtotal - Math.max(0, discount)) + (typeof charge === 'number' ? charge : charge.cents);", true],
			]) {
				writeFileSync(join(root, "checkout.ts"), `import { shipping } from './shipping.ts';\nexport function quote(subtotal, weight, discount = 0) { ${body} }\n`);
				const result = spawnSync(process.execPath, ["--input-type=module", "-e", acceptance(rate)], { cwd: root, encoding: "utf8", timeout: 10_000 });
				assert.equal(result.status === 0, passes, result.stderr);
			}
		}
	} finally { rmSync(root, { recursive: true, force: true }); }
});
