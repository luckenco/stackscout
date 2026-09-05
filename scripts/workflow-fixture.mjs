// Small, deterministic workflow pilot. Distractors have no dependency on checkout.
export const files = {
	"checkout.ts": 'import { shipping } from "./shipping.ts";\nexport function quote(subtotal: number, weight: number) {\n  return subtotal + shipping(weight);\n}\n',
	"shipping.ts": "export function shipping(weight: number) {\n  return Math.ceil(Math.max(0, weight)) * 100;\n}\n",
	"telemetry.ts": 'export function label() { return "checkout-v1"; }\n',
	"check.mjs": 'import assert from "node:assert/strict";\nimport { quote } from "./checkout.ts";\nimport { shipping } from "./shipping.ts";\nconst rate = shipping(1);\nconst cents = typeof rate === "number" ? rate : rate.cents;\nassert.equal(quote(1000, 2), 1000 + 2 * cents);\nconsole.log("smoke passed");\n',
};
for (const name of ["inventory", "returns", "accounts", "search", "catalog", "reviews", "receipts", "notifications"]) {
	files[`${name}.ts`] = `export function key(id: string) { return "${name}:" + id; }\nexport function active(id: string) { return id.length > 0; }\n`;
}

// Exact writer payload makes the interleaving identical across treatment/control trials.
export const shippingChange = "export function shipping(weight: number) {\n  return { cents: Math.ceil(Math.max(0, weight)) * 200, carrier: \"ground\" };\n}\n";
export const telemetryChange = 'export function label() { return "checkout-v2"; }\n';

export const prepare = "Inspect checkout.ts and its dependencies to prepare a small change to quote. Read only for now; I will send implementation requirements next. Keep your response brief.";
export const implement = `Implement quote(subtotal, weight, discount = 0): subtract a nonnegative discount from subtotal, floor the discounted subtotal at zero, then add the current shipping charge. Preserve the two-argument API. Only edit checkout.ts and report.json. Run node check.mjs and inspect your final patch against .baseline/checkout.ts.
Other work may have landed since preparation. Identify any concurrent file changes that require adapting checkout.ts, without attributing unrelated changes to this task. As soon as you identify one, write report.json as {"relevant": ["filename"], "reason": "brief evidence"}; write {"relevant": [], "reason": "..."} at completion if none. Use source and tests to verify, not graph absence as proof.`;
export const guidance = "Use one call_graph_review with files:[\"checkout.ts\"] and refresh:true before implementing. Reuse still-applicable preparation reads and read changed dependencies from their source ranges. Advance the scope with call_graph_ack only if another review is needed. Use the final patch and tests for implementation verification.";

// Evaluator lives outside the agent workspace; agents see only the smoke test above.
export function acceptance(rate) {
	return `import assert from "node:assert/strict";
import { quote } from "./checkout.ts";
for (const [subtotal, weight, discount, expected] of [
  [1000, 2, undefined, ${1000 + 2 * rate}], [1000, 2, 200, ${800 + 2 * rate}],
  [1000, 0, 2000, 0], [1000, -2, 100, 900], [0, 1.1, 0, ${2 * rate}],
  [1000, 1, -200, ${1000 + rate}], [1000, 0, undefined, 1000]
]) assert.equal(quote(subtotal, weight, discount), expected);
console.log("7 acceptance cases passed");`;
}
