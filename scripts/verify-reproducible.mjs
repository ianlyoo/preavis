// npm run verify:reproducible: rebuilds res/preavis.wasm from the commit its NEP-330 metadata names,
// in the pinned linux/amd64 Docker image, and compares the bytes with the committed file, its
// recorded sha256 and the code deployed on testnet. Needs Docker and git.
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { base58, query, sha256 } from "./near-rpc.mjs";

const prebuilt = readFileSync("res/preavis.wasm");
const rev = prebuilt.toString("latin1").match(/github\.com\/ianlyoo\/preavis\?rev=([0-9a-f]{40})/)?.[1];
if (!rev) throw new Error("res/preavis.wasm carries no NEP-330 source revision");
execFileSync("bash", ["scripts/build-reproducible.sh", rev], { stdio: "inherit" });
const rebuilt = readFileSync("target/reproducible/preavis.wasm");
const hex = sha256(rebuilt).toString("hex");
const recorded = readFileSync("res/preavis.wasm.sha256", "utf8").split(/\s+/)[0];
const mandate = JSON.parse(readFileSync("testnet/receipts.json", "utf8")).mandate;
const deployed = (await query({ request_type: "view_account", account_id: mandate })).code_hash;

const checks = [
  [rebuilt.equals(prebuilt), `rebuilt from commit ${rev}: byte-identical to res/preavis.wasm`, `sha256 ${hex} vs ${sha256(prebuilt).toString("hex")}`],
  [recorded === hex, "equals res/preavis.wasm.sha256", recorded],
  [deployed === base58(sha256(rebuilt)), `equals the code deployed on testnet at ${mandate}`, `chain code_hash ${deployed}, rebuilt ${base58(sha256(rebuilt))}`],
];
for (const [ok, label, detail] of checks) console.log(`${ok ? "OK  " : "FAIL"}  ${label}\n      ${detail}`);
const failed = checks.filter(([ok]) => !ok).length;
console.log(failed ? `\n${failed} check(s) FAILED.` : "\nSource, committed wasm and deployed code are the same bytes.");
process.exit(failed ? 1 : 0);
