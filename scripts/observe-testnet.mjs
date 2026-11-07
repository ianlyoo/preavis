// Read-only check of the recorded testnet run: no key, no signing, no dependencies (Node >= 18).
// Usage: node scripts/observe-testnet.mjs [testnet/receipts.json]
//
// For every claim the run recorded the transaction and what it expects to see in it: signer,
// receiver, actions (method, args, deposit), success or failure, the NEP-297 events, each failure
// reason and the transfers the mandate sent. This script fetches each transaction from the chain,
// reduces it the same way, and fails on any difference. Then it re-reads the end state (views and
// balances), recomputes the totals from the stored attempts, and fails unless the mandate account
// has no access key left. Exit code: 0 only if every check holds.
import { readFileSync } from "node:fs";
import { base58, query, rpc, sha256, view } from "./near-rpc.mjs";

const record = JSON.parse(readFileSync(process.argv[2] ?? "testnet/receipts.json", "utf8"));
const mandate = record.mandate;

// JSON with sorted keys, so recorded and re-read values compare independent of key order.
const canon = (v) => Array.isArray(v) ? `[${v.map(canon).join(",")}]`
  : v && typeof v === "object" ? `{${Object.keys(v).sort().map((k) => `${JSON.stringify(k)}:${canon(v[k])}`).join(",")}}`
  : JSON.stringify(v);
const same = (a, b) => canon(a) === canon(b);
const near = (yocto) => `${(Number(BigInt(yocto)) / 1e24).toFixed(6)} NEAR`;

let failed = 0;
const check = (ok, label, details = []) => {
  if (!ok) failed++;
  console.log(`${ok ? "OK  " : "FAIL"}  ${label}`);
  for (const d of [details].flat().filter(Boolean)) console.log(`      ${d}`);
};

// The same reductions tests/common/mod.rs applies when it records a claim.
const reduce = (tx) => {
  const outcomes = [tx.transaction_outcome, ...tx.receipts_outcome];
  const events = outcomes.flatMap((o) => o.outcome.logs)
    .filter((l) => l.startsWith("EVENT_JSON:")).map((l) => JSON.parse(l.slice(11)));
  const failures = outcomes.map((o) => o.outcome.status).filter((s) => s.Failure).map((s) => {
    const text = JSON.stringify(s);
    const panic = text.split("panicked: ")[1];
    return panic !== undefined ? panic.split(/["\\]/)[0] : text.includes("AccountDoesNotExist") ? "AccountDoesNotExist" : "other";
  }).sort();
  const transfers = tx.receipts.filter((r) => r.predecessor_id === mandate)
    .flatMap((r) => r.receipt.Action.actions.filter((a) => a.Transfer).map((a) => ({ to: r.receiver_id, amount: a.Transfer.deposit })));
  return {
    signer: tx.transaction.signer_id, receiver: tx.transaction.receiver_id, actions: tx.transaction.actions,
    tx_status: "SuccessValue" in tx.status ? "success" : "failure", events, failures, transfers,
  };
};
// Transfers the recorded events say the mandate sent: one per `pending` attempt, one per `closed`.
const expectedTransfers = (events) => events.flatMap((e) =>
  e.event === "pending" ? [{ to: e.data[0].payee, amount: e.data[0].amount }]
  : e.event === "closed" ? [{ to: e.data[0].refund_to, amount: e.data[0].amount }] : []);
const describeAction = (a) => {
  if (a.FunctionCall) {
    const { method_name, args, deposit } = a.FunctionCall;
    return `${method_name}(${Buffer.from(args, "base64").toString()})${deposit !== "0" ? ` deposit ${deposit}` : ""}`;
  }
  const [kind, body] = Object.entries(a)[0];
  return `${kind} ${body?.public_key ?? ""}`.trim();
};
const describeEvent = (e) => `${e.event}${e.data[0].request_id ? ` ${e.data[0].request_id}` : ""}${e.data[0].reason ? ` ${e.data[0].reason}` : ""}`;

console.log(`Observing ${mandate} on testnet (recorded run of ${new Date(record.recorded_at_unix * 1000).toISOString()})\n`);

// 1. The code.
const wasm = readFileSync("res/preavis.wasm");
const localHash = base58(sha256(wasm));
const rev = wasm.toString("latin1").match(/preavis\?rev=([0-9a-f]{40})/)?.[1];
const account = await query({ request_type: "view_account", account_id: mandate });
const isContract = account.code_hash !== "11111111111111111111111111111111";
check(account.code_hash === localHash && record.code_sha256 === sha256(wasm).toString("hex"),
  "deployed code hash equals sha256 of res/preavis.wasm (the reproducible build of the commit below)",
  [`chain ${account.code_hash}, local ${localHash}`, `source: NEP-330 build info names commit ${rev}; rebuild it with npm run verify:reproducible`]);

// 2. The keys.
const agentKeys = (await query({ request_type: "view_access_key_list", account_id: record.agent })).keys;
const working = agentKeys.filter((k) => k.access_key.permission.FunctionCall);
const fc = working[0]?.access_key.permission.FunctionCall;
const fullAccess = agentKeys.filter((k) => k.access_key.permission === "FullAccess").map((k) => k.public_key);
check(working.length === 1 && working[0].public_key === record.agent_public_key
  && fc.receiver_id === mandate && fc.method_names.join() === "request_spend"
  && fullAccess.join() === record.agent_offline_public_key,
  "the agent's working key is function-call only (request_spend on the mandate); its only other key is the owner's offline full-access key",
  agentKeys.map((k) => `${k.public_key} ${JSON.stringify(k.access_key.permission)}`));

// 3. Every recorded transaction, compared field by field.
console.log("\nRecorded transactions, re-read from the chain and compared with what the run recorded:");
for (const claim of record.claims.filter((c) => c.tx)) {
  const tx = await rpc("EXPERIMENTAL_tx_status", { tx_hash: claim.tx, sender_account_id: claim.signer, wait_until: "FINAL" });
  const seen = reduce(tx);
  const expected = { ...claim, transfers: expectedTransfers(claim.events ?? []) };
  const diffs = ["signer", "receiver", "actions", "tx_status", "events", "failures", "transfers"]
    .filter((field) => !same(seen[field], expected[field]))
    .map((field) => `MISMATCH ${field}: chain ${canon(seen[field]).slice(0, 240)} | recorded ${canon(expected[field]).slice(0, 240)}`);
  check(diffs.length === 0, claim.claim, [
    `${seen.signer} -> ${seen.receiver}: ${seen.actions.map(describeAction).join("; ")}`,
    `${seen.tx_status}${seen.events.length ? `; events: ${seen.events.map(describeEvent).join(", ")}` : ""}${seen.failures.length ? `; failed: ${seen.failures.join(", ")}` : ""}`,
    ...seen.transfers.map((t) => `transfer ${near(t.amount)} -> ${t.to}`),
    ...diffs,
    `https://testnet.nearblocks.io/txns/${claim.tx}`,
  ]);
}
for (const claim of record.claims.filter((c) => !c.tx)) {
  console.log(`INFO  ${claim.claim}\n      no transaction (refused before execution): ${claim.observed}`);
}

// 4. The end state.
console.log("\nEnd state, re-read from the chain:");
// An account without the contract fails every check below instead of stopping the script.
const empty = { committed: "0", held: "0", payments: 0, denials: 0, payee_committed: {}, policy: { total_cap: "0", payees: {} } };
const summary = isContract ? await view(mandate, "mandate", {}) : empty;
const attempts = isContract ? await view(mandate, "attempts", { from: 0, limit: 100 }) : [];
check(isContract, `${mandate} runs a contract`);
const end = record.end_state ?? {};
check(same(summary, end.mandate), "mandate() equals the recorded end state",
  `committed ${near(summary.committed)} of ${near(summary.policy.total_cap)}; held ${near(summary.held)}; payments ${summary.payments}, denials ${summary.denials}`);
check(same(attempts, end.attempts), `attempts() equals the ${end.attempts?.length ?? 0} recorded attempts`);

const charged = attempts.filter((a) => ["AWAITING_APPROVAL", "PENDING", "SETTLED"].includes(a.status));
const sum = (list) => list.reduce((s, a) => s + BigInt(a.amount), 0n);
const perPayee = {};
for (const a of charged) perPayee[a.payee] = (perPayee[a.payee] ?? 0n) + BigInt(a.amount);
const committedOk = BigInt(summary.committed) === sum(charged)
  && BigInt(summary.held) === sum(charged.filter((a) => a.status === "AWAITING_APPROVAL"))
  && Object.keys(perPayee).length === Object.keys(summary.payee_committed).filter((p) => summary.payee_committed[p] !== "0").length
  && Object.entries(perPayee).every(([p, v]) => BigInt(summary.payee_committed[p] ?? 0) === v);
check(committedOk, "committed, held and payee_committed equal the sums of the stored attempts that hold a charge",
  Object.entries(summary.payee_committed).map(([p, v]) => `${p}: ${near(v)} committed (attempts: ${near(perPayee[p] ?? 0n)})`));
check(BigInt(summary.committed) <= BigInt(summary.policy.total_cap)
  && Object.entries(summary.payee_committed).every(([p, v]) => BigInt(v) <= BigInt(summary.policy.payees[p] ?? 0)),
  "no cap is exceeded: committed <= total_cap; each payee <= its allowlisted cap");
check(attempts.every((a) => a.payee in summary.policy.payees || a.status === "DENIED"),
  "every attempt that was not denied names an allowlisted payee");

const balances = {};
for (const id of Object.keys(end.balances ?? {})) balances[id] = (await query({ request_type: "view_account", account_id: id })).amount;
check(Object.keys(balances).length > 0 && same(balances, end.balances),
  "the vendor, hosting, attacker and treasury balances equal the recorded ones",
  [...Object.entries(balances).map(([id, v]) => `${id}: ${near(v)}`),
   "(anyone can send NEAR to any account, so a difference here alone is not proof of a payout; the transfers above are)"]);
const settledTo = (id) => sum(attempts.filter((a) => a.payee === id && a.status === "SETTLED"));
check(settledTo(record.attacker) === 0n && !attempts.some((a) => a.payee === record.attacker && a.status !== "DENIED"),
  "the attacker account received nothing from the mandate: none of its requests got past the checks");

// 5. The lock.
const mandateKeys = (await query({ request_type: "view_access_key_list", account_id: mandate })).keys;
check(mandateKeys.length === 0,
  mandateKeys.length === 0 ? `locked: no key can sign for ${mandate}` : `NOT LOCKED: ${mandate} still has ${mandateKeys.length} access key(s)`,
  mandateKeys.length === 0
    ? "view_access_key_list returns 0 keys; the code has no method that adds a key or deploys code (no_admin_surface)"
    : mandateKeys.map((k) => `${k.public_key} ${JSON.stringify(k.access_key.permission)}`));

console.log("\nOn-chain attempts:");
console.table(attempts.map((a) => ({ request_id: a.request_id, payee: a.payee.split(".")[0],
  NEAR: Number(BigInt(a.amount)) / 1e24, status: a.status, reason: a.reason ?? "" })));

const claims = record.claims.filter((c) => c.tx).length;
console.log(failed
  ? `\nFAILED: ${failed} check(s) do not match the chain.`
  : `\nOK: ${claims} recorded transactions match the chain; end state re-read; locked: no key can sign for it; code hash ${localHash} = reproducible build of ${rev?.slice(0, 12)}.`);
process.exit(failed ? 1 : 0);
