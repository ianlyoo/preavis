// Read-only check of the recorded testnet run: no key, no signing, no dependencies (Node >= 18).
// Usage: node scripts/observe-testnet.mjs [testnet/receipts.json]
// PREAVIS_TESTNET_RPC=<url> replaces the default endpoint list.
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";

const record = JSON.parse(readFileSync(process.argv[2] ?? "testnet/receipts.json", "utf8"));
const endpoints = process.env.PREAVIS_TESTNET_RPC
  ? [process.env.PREAVIS_TESTNET_RPC]
  : ["https://archival-rpc.testnet.fastnear.com", "https://test.rpc.fastnear.com", "https://archival-rpc.testnet.near.org"];

// Retries rate limits (429) and server errors with backoff, then tries the next endpoint.
async function rpc(method, params) {
  for (const url of endpoints) {
    for (let attempt = 0; attempt < 4; attempt++) {
      try {
        const response = await fetch(url, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params }),
        });
        if (response.status === 429 || response.status >= 500) throw new Error(`HTTP ${response.status}`);
        const body = await response.json();
        if (body.error) throw new Error(JSON.stringify(body.error).slice(0, 300));
        return body.result;
      } catch (error) {
        if (!/HTTP (429|5)|fetch failed/.test(error.message)) throw error;
        await new Promise((resolve) => setTimeout(resolve, 1000 * 2 ** attempt));
      }
    }
  }
  throw new Error(`every RPC endpoint failed for ${method}; set PREAVIS_TESTNET_RPC to another testnet RPC`);
}

const query = (params) => rpc("query", { finality: "final", ...params });
const view = async (method, args) => {
  const result = await query({
    request_type: "call_function", account_id: record.mandate, method_name: method,
    args_base64: Buffer.from(JSON.stringify(args)).toString("base64"),
  });
  return JSON.parse(Buffer.from(result.result).toString());
};

function base58(bytes) {
  const alphabet = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
  let n = BigInt("0x" + Buffer.from(bytes).toString("hex"));
  let out = "";
  while (n > 0n) { out = alphabet[Number(n % 58n)] + out; n /= 58n; }
  for (const b of bytes) { if (b !== 0) break; out = "1" + out; }
  return out;
}

let failures = 0;
const check = (ok, label, detail) => {
  if (!ok) failures++;
  console.log(`${ok ? "OK  " : "FAIL"}  ${label}${detail ? `\n      ${detail}` : ""}`);
};

console.log(`Observing ${record.mandate} on testnet (recorded run of ${new Date(record.recorded_at_unix * 1000).toISOString()})\n`);

const wasm = readFileSync("res/preavis.wasm");
const localHash = base58(createHash("sha256").update(wasm).digest());
const account = await query({ request_type: "view_account", account_id: record.mandate });
check(account.code_hash === localHash, "deployed code hash equals sha256 of res/preavis.wasm",
  `chain ${account.code_hash}, local ${localHash} (hex ${createHash("sha256").update(wasm).digest("hex")})`);

const agentKeys = (await query({ request_type: "view_access_key_list", account_id: record.agent })).keys;
const working = agentKeys.filter((k) => k.access_key.permission.FunctionCall);
const fc = working[0]?.access_key.permission.FunctionCall;
const fullAccess = agentKeys.filter((k) => k.access_key.permission === "FullAccess").map((k) => k.public_key);
check(working.length === 1 && working[0].public_key === record.agent_public_key
  && fc.receiver_id === record.mandate && fc.method_names.join() === "request_spend"
  && fullAccess.join() === record.agent_offline_public_key,
  "the agent's working key is function-call only (request_spend on the mandate); its only other key is the owner's offline full-access key",
  agentKeys.map((k) => `${k.public_key} ${JSON.stringify(k.access_key.permission)}`).join("\n      "));

const mandateKeys = (await query({ request_type: "view_access_key_list", account_id: record.mandate })).keys;
console.log(`INFO  mandate access keys: ${mandateKeys.length}${mandateKeys.length
  ? " (the deployer key; NOT LOCKED yet: the lock deletes it, see THREAT_MODEL.md)" : " (locked: no key can sign for it)"}`);

console.log("\nRecorded transactions (status and events re-read from chain):");
for (const claim of record.claims.filter((c) => c.tx)) {
  const tx = await rpc("tx", { tx_hash: claim.tx, sender_account_id: claim.signer, wait_until: "FINAL" });
  const status = "SuccessValue" in tx.status ? "success" : "failure";
  const events = tx.receipts_outcome.flatMap((r) => r.outcome.logs)
    .filter((l) => l.startsWith("EVENT_JSON:"))
    .map((l) => JSON.parse(l.slice(11)))
    .map((e) => `${e.event}${e.data[0].reason ? ` ${e.data[0].reason}` : ""}`);
  const failure = JSON.stringify(tx.receipts_outcome.map((r) => r.outcome.status).find((s) => s.Failure) ?? "")
    .match(/panicked: (Method \w+ is private|[A-Z_]+)|AccountDoesNotExist/)?.[0];
  check(status === claim.tx_status, claim.claim,
    `${status}${events.length ? `; events: ${events.join(", ")}` : ""}${failure ? `; ${failure}` : ""}; https://testnet.nearblocks.io/txns/${claim.tx}`);
}
for (const claim of record.claims.filter((c) => !c.tx)) {
  console.log(`INFO  ${claim.claim}\n      no transaction: ${claim.observed}`);
}

const summary = await view("mandate", {});
const attempts = await view("attempts", { from: 0, limit: 100 });
console.log(`\nOn-chain attempts (committed ${summary.committed} of ${summary.policy.total_cap} yoctoNEAR):`);
console.table(attempts.map((a) => ({ request_id: a.request_id, payee: a.payee.split(".")[0],
  NEAR: Number(BigInt(a.amount)) / 1e24, status: a.status, reason: a.reason ?? "" })));

console.log(failures ? `\n${failures} check(s) FAILED.` : "\nEvery recorded claim matches the chain.");
process.exit(failures ? 1 : 0);
