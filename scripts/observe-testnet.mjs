// Read-only check of the recorded testnet run: no key, no signing, no dependencies (Node >= 18).
// Usage: node scripts/observe-testnet.mjs [testnet/receipts.json]
//
// For every claim the run recorded the transaction and what it expects to see in it: signer,
// receiver, actions (method, args, deposit), success or failure, the NEP-297 events, each failure
// reason and the transfers the mandate sent. This script fetches each transaction from the chain,
// reduces it the same way, and fails on any difference. Then it re-reads the end state (views and
// balances), recomputes the totals from the stored attempts, and fails unless the mandate account
// has no access key left. It also reads the deployed code itself (view_code) and fails on any
// host-function import outside the allowlist tests/verify.rs keeps (no deploy, no key, no
// function-call action exists in the code). Last, if testnet/challenge.json exists, the challenge
// mandate: its lock, keys, policy and the counter "attempts / paid to attackers"; once the record
// says it was retired, that its key is gone, it is closed and the close refunded only refund_to.
// Exit code: 0 only if every check holds.
// Usage: node scripts/observe-testnet.mjs [receipts.json] [challenge.json | none]
import { existsSync, readFileSync } from "node:fs";
import { base58, query, rpc, sha256, view } from "./near-rpc.mjs";
import { wasmImports } from "./wasm-imports.mjs";

const record = JSON.parse(readFileSync(process.argv[2] ?? "testnet/receipts.json", "utf8"));
const mandate = record.mandate;
const challengeFile = process.argv[3] ?? "testnet/challenge.json";
const challenge = challengeFile !== "none" && existsSync(challengeFile) ? JSON.parse(readFileSync(challengeFile, "utf8")) : null;
// The one allowlist, read from the test that checks the local build against it.
const allowedImports = [...readFileSync("tests/verify.rs", "utf8")
  .match(/ALLOWED_IMPORTS: \[&str; \d+\] = \[([^\]]*)\]/)[1].matchAll(/"([a-z0-9_]+)"/g)].map((m) => m[1]);

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
// The deployed code's host imports, read from the chain: every one must be on the allowlist.
const checkImports = async (account) => {
  let imports = [];
  try {
    imports = wasmImports(Buffer.from((await query({ request_type: "view_code", account_id: account })).code_base64, "base64"));
  } catch (error) {
    imports = [`(no code: ${error.message.slice(0, 80)})`];
  }
  const outside = imports.filter((i) => !(i.startsWith("env.") && allowedImports.includes(i.slice(4))));
  check(imports.length > 0 && outside.length === 0,
    `${account}: the deployed code imports only allowlisted host functions (view_code, ${imports.length} imports)`,
    outside.length
      ? outside.map((i) => `NOT ALLOWED: ${i}`)
      : "none of deploy_contract, add_key_*, delete_key, function_call, stake, delete_account: nothing in the code can add a key, redeploy, or call another contract");
};
// Live fields of mandate(): they follow the block height, so they are checked for consistency with
// the recorded state at the current height instead of compared byte for byte.
const LIVE = ["period", "period_start", "resets_at", "period_committed", "period_payments", "period_denials", "exhausted", "expired"];
const finalBlock = async () => (await rpc("block", { finality: "final" })).header;
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

await checkImports(mandate);

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
  const why = claim.layer === "protocol"
    ? "no transaction: the network refused to accept it, so nothing reached the chain"
    : "no transaction: a statement about the end state, re-checked from the chain below";
  console.log(`INFO  ${claim.claim}\n      ${why}: ${claim.observed}`);
}

// 4. The end state.
console.log("\nEnd state, re-read from the chain:");
// An account without the contract fails every check below instead of stopping the script.
const empty = { committed: "0", held: "0", payments: 0, denials: 0, payee_committed: {}, policy: { total_cap: "0", payees: {} } };
const summary = isContract ? await view(mandate, "mandate", {}) : empty;
const attempts = isContract ? await view(mandate, "attempts", { from: 0, limit: 100 }) : [];
check(isContract, `${mandate} runs a contract`);
const end = record.end_state ?? {};
const head = await finalBlock();
const strip = (m) => Object.fromEntries(Object.entries(m ?? {}).filter(([k]) => !LIVE.includes(k)));
check(same(strip(summary), strip(end.mandate)), "mandate() equals the recorded end state (fields that do not follow the block height)",
  `committed ${near(summary.committed)} of ${near(summary.policy.total_cap)}; held ${near(summary.held)}; payments ${summary.payments}, denials ${summary.denials}`);
const live = (s, rec, height) => {
  const p = s.policy;
  if (!rec || !p?.period_blocks) return ["no recorded period"];
  const start = rec.period_start - rec.period * p.period_blocks;
  const period = Math.floor((height - start) / p.period_blocks);
  const same_period = s.period === rec.period;
  const problems = [];
  // The view may be served a block or two apart from the header read here.
  if (Math.abs(s.period - period) > 1 || s.period < rec.period) problems.push(`period ${s.period}, expected ${period} at height ${height}`);
  if (s.period_start !== start + s.period * p.period_blocks || s.resets_at !== s.period_start + p.period_blocks) problems.push("period_start / resets_at");
  if (same_period ? !(s.period_committed === rec.period_committed && s.period_payments === rec.period_payments && s.period_denials === rec.period_denials)
    : !(s.period_committed === "0" && s.period_payments === 0 && s.period_denials === 0)) problems.push("period counters");
  if (s.exhausted !== (s.period_payments >= p.max_payments)) problems.push("exhausted");
  if (Math.abs(height - p.expires_at) > 2 && s.expired !== (height >= p.expires_at)) problems.push("expired");
  return problems;
};
const liveProblems = live(summary, end.mandate, head.height);
check(liveProblems.length === 0, "the window fields (period, resets_at, period counters, exhausted, expired) are consistent with the recorded state at the current height",
  [`height ${head.height}: period ${summary.period} (blocks ${summary.period_start}..${summary.resets_at}); this period: ${near(summary.period_committed ?? "0")} committed, ${summary.period_payments} payments, ${summary.period_denials} denials stored${summary.exhausted ? ", EXHAUSTED" : ""}${summary.expired ? ", EXPIRED" : ""}`,
   ...liveProblems.map((p) => `INCONSISTENT: ${p}`)]);
check(same(attempts, end.attempts), `attempts() equals the ${end.attempts?.length ?? 0} recorded attempts`);

const sum = (list) => list.reduce((s, a) => s + BigInt(a.amount), 0n);
// Charged: committed and paid (or in flight). A held request reserves nothing until it is approved.
const chargeOk = (summary, attempts) => {
  const charged = attempts.filter((a) => ["PENDING", "SETTLED"].includes(a.status));
  const perPayee = {};
  for (const a of charged) perPayee[a.payee] = (perPayee[a.payee] ?? 0n) + BigInt(a.amount);
  return {
    perPayee,
    ok: BigInt(summary.committed) === sum(charged)
      && BigInt(summary.held) === sum(attempts.filter((a) => a.status === "AWAITING_APPROVAL"))
      && Object.keys(perPayee).length === Object.keys(summary.payee_committed).filter((p) => summary.payee_committed[p] !== "0").length
      && Object.entries(perPayee).every(([p, v]) => BigInt(summary.payee_committed[p] ?? 0) === v),
  };
};
const { perPayee, ok: committedOk } = chargeOk(summary, attempts);
check(committedOk, "committed and payee_committed equal the sums of the settled and in-flight attempts; held equals the sum of those awaiting approval",
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

// 6. The challenge mandate, if one is recorded: a separate mandate, separate accounts and keys,
// nothing shared with the run above or with the local sandbox. Retired (record.retired): the
// function-call key deleted, the mandate closed, its remainder sent to refund_to only.
let counter = "";
if (challenge) {
  const c = challenge;
  const r = c.retired;
  console.log(`\nThe challenge mandate: ${c.mandate} (deployed ${new Date(c.created_at_unix * 1000).toISOString()}${r ? `; never published, retired ${new Date(r.closed_at_unix * 1000).toISOString()}` : ""})`);
  const acct = await query({ request_type: "view_account", account_id: c.mandate }).catch(() => null);
  check(acct?.code_hash === localHash && c.code_sha256 === sha256(wasm).toString("hex"),
    "challenge: deployed code hash equals sha256 of res/preavis.wasm", `chain ${acct?.code_hash}, local ${localHash}`);
  await checkImports(c.mandate);
  const keys = (await query({ request_type: "view_access_key_list", account_id: c.mandate }).catch(() => ({ keys: [{}] }))).keys;
  check(keys.length === 0, keys.length === 0 ? `challenge: locked, no key can sign for ${c.mandate}` : `challenge: NOT LOCKED: ${keys.length} key(s)`);

  const agentKeys = (await query({ request_type: "view_access_key_list", account_id: c.agent })).keys;
  const fcKeys = agentKeys.filter((k) => k.access_key.permission.FunctionCall);
  const fcp = fcKeys[0]?.access_key.permission.FunctionCall;
  const full = agentKeys.filter((k) => k.access_key.permission === "FullAccess").map((k) => k.public_key);
  if (r) {
    check(fcKeys.length === 0 && full.join() === c.agent_offline_public_key,
      `challenge: retired: the agent's function-call key ${c.agent_public_key} is gone; only the offline full-access key is left, so nobody can call request_spend`,
      agentKeys.map((k) => `${k.public_key} ${JSON.stringify(k.access_key.permission)}`));
    const status = async (h, sender) => rpc("EXPERIMENTAL_tx_status", { tx_hash: h, sender_account_id: sender, wait_until: "FINAL" }).catch(() => null);
    const fired = await status(r.key_deleted_tx, c.agent);
    const firedOk = fired && "SuccessValue" in fired.status && fired.transaction.receiver_id === c.agent
      && fired.transaction.actions.length === 1 && fired.transaction.actions[0].DeleteKey?.public_key === c.agent_public_key;
    check(firedOk, `challenge: retired: transaction ${r.key_deleted_tx} deleted that key`);
    const closed = await status(r.close_tx, c.owner);
    const sent = (closed?.receipts ?? []).filter((x) => x.predecessor_id === c.mandate && x.receiver_id !== c.mandate)
      .flatMap((x) => (x.receipt.Action?.actions ?? []).filter((a) => a.Transfer).map((a) => [x.receiver_id, a.Transfer.deposit]));
    check(closed && "SuccessValue" in closed.status && closed.transaction.signer_id === c.owner && closed.transaction.receiver_id === c.mandate
      && sent.length === 1 && sent[0][0] === c.refund_to && sent[0][1] === r.refunded_yocto,
      `challenge: retired: transaction ${r.close_tx} closed the mandate and sent ${near(r.refunded_yocto)} to ${c.refund_to}, nothing to anyone else`,
      sent.map(([to, v]) => `transfer ${near(v)} -> ${to}`));
  } else {
    check(fcKeys.length === 1 && fcKeys[0].public_key === c.agent_public_key && fcp.receiver_id === c.mandate
      && fcp.method_names.join() === "request_spend" && full.join() === c.agent_offline_public_key,
      "challenge: the agent's challenge key is function-call only (request_spend on the mandate); the only other key is the offline full-access key",
      agentKeys.map((k) => `${k.public_key} ${JSON.stringify(k.access_key.permission)}`));
  }
  const allowance = fcp?.allowance == null ? null : BigInt(fcp.allowance);
  const perCall = BigInt(c.calibration.allowance_used_yocto);
  const used = allowance === null ? 0n : BigInt(c.allowance_yocto) - allowance;

  const s = await view(c.mandate, "mandate", {}).catch(() => null);
  const all = [];
  for (let from = 0; s && from < s.attempts; from += 100) all.push(...await view(c.mandate, "attempts", { from, limit: 100 }));
  const allowlisted = new Set(c.allowlisted);
  check(s && same(s.policy, c.policy) && s.agent === c.agent && s.refund_to === c.refund_to && s.owner === c.owner,
    "challenge: the policy, agent and refund account equal the recorded ones (the code has no method that changes them)",
    s && `allowlisted: ${Object.entries(s.policy.payees).map(([p, v]) => `${p} up to ${near(v)}`).join(", ")}; total ${near(s.policy.total_cap)}; ${near(s.policy.approval_threshold)} a day without the owner`);
  if (s) {
    const { ok } = chargeOk(s, all);
    check(ok && all.length === s.attempts, "challenge: committed, held and per-payee totals equal the sums of all stored attempts",
      `${all.length} stored attempts; committed ${near(s.committed)}, held ${near(s.held)}`);
    check(BigInt(s.committed) <= BigInt(s.policy.total_cap)
      && Object.entries(s.payee_committed).every(([p, v]) => BigInt(v) <= BigInt(s.policy.payees[p] ?? 0)),
      "challenge: no cap is exceeded");
    // Paid to attackers: what left the mandate for anyone outside the allowlist. Two independent reads:
    // the stored attempts, and the balance (it may only fall by what the attempts say was paid; it
    // rises with the contract's share of the gas the calls burn).
    const toOthers = all.filter((a) => ["PENDING", "SETTLED", "AWAITING_APPROVAL"].includes(a.status) && !allowlisted.has(a.payee));
    const paidOut = sum(all.filter((a) => ["PENDING", "SETTLED"].includes(a.status)));
    const balance = BigInt(acct?.amount ?? 0);
    const refunded = r ? BigInt(r.refunded_yocto) : 0n;
    const floor = BigInt(c.mandate_start_balance) - paidOut - refunded;
    const unexplained = floor > balance ? floor - balance : 0n;
    const paidToAttackers = sum(toOthers) + unexplained;
    check(toOthers.length === 0 && unexplained === 0n,
      `challenge: paid to attackers is 0: no attempt to a non-allowlisted account got past the checks, and the balance fell by no more than the allowlisted payments${r ? " and the refund" : ""}`,
      [`balance ${near(balance)} = start ${near(c.mandate_start_balance)} - paid to allowlisted ${near(paidOut)}${r ? ` - refunded to ${c.refund_to} ${near(refunded)}` : ""} + ${near(balance - floor > 0n ? balance - floor : 0n)} gas rewards`,
       ...toOthers.map((a) => `PAID OUTSIDE THE MANDATE: ${a.request_id} ${near(a.amount)} -> ${a.payee} (${a.status})`)]);

    if (r) {
      check(s.closed === true, `challenge: retired: mandate() reports closed; nothing can be requested or approved any more`);
    } else {
      const now = await finalBlock();
      const msPerBlock = (Number(now.timestamp_nanosec) / 1e6 - c.created_at_unix * 1000) / (now.height - c.created_at_height);
      const at = (height) => new Date(Number(now.timestamp_nanosec) / 1e6 + (height - now.height) * msPerBlock).toISOString().slice(0, 16).replace("T", " ") + " UTC";
      const problems = live(s, { period_start: s.period_start - s.period * s.policy.period_blocks, period: 0 }, now.height)
        .filter((p) => !p.startsWith("period counters"));
      check(problems.length === 0 && !s.closed,
        `challenge: today's window ${s.period}: ${s.period_payments}/${s.policy.max_payments} payments, ${s.period_denials}/${s.policy.max_denials} denials stored, ${near(s.period_committed)} of ${near(s.policy.approval_threshold)} committed${s.exhausted ? "; EXHAUSTED" : ""}`,
        [s.exhausted
          ? `EXHAUSTED: no more requests are accepted until block ${s.resets_at} (about ${at(s.resets_at)}); then a fresh window opens`
          : `the window resets at block ${s.resets_at} (about ${at(s.resets_at)}); a flood exhausts only this window`,
         `expires at block ${s.policy.expires_at} (about ${at(s.policy.expires_at)}); after that anyone may close() it, which refunds ${c.refund_to}${s.expired ? "; EXPIRED" : ""}`,
         `height ${now.height}, measured ${msPerBlock.toFixed(0)} ms per block since deployment`,
         ...problems.map((p) => `INCONSISTENT: ${p}`)]);
    }
    const calls = s.attempts + s.denials_not_stored - 1;
    if (!r) check(allowance !== null, `challenge: the key's gas allowance left: ${allowance === null ? "unlimited (not expected)" : near(allowance)} of ${near(c.allowance_yocto)}`,
      `used ${near(used)} ≈ ${perCall ? (used / perCall).toString() : "?"} calls at the calibrated ${near(perCall)} net per call; while a call is in flight it holds up to ${near(c.calibration.allowance_in_flight_yocto)}`);
    counter = `${r ? "retired, never published; attempts" : "attempts so far"}: ${calls} / paid to attackers: ${paidToAttackers === 0n ? "0" : near(paidToAttackers)}`;
    console.log(`\n  ${counter}`);
    console.log(`  (requests the contract recorded since the lock, not counting the deployer's calibration call; ${s.payments} paid to the allowlist, ${s.denials} denied, ${s.denials_not_stored} denials over the daily record limit counted but not stored)`);
  }
}

const claims = record.claims.filter((c) => c.tx).length;
console.log(failed
  ? `\nFAILED: ${failed} check(s) do not match the chain.`
  : `\nOK: ${claims} recorded transactions match the chain; end state re-read; locked: no key can sign for it; code hash ${localHash} = reproducible build of ${rev?.slice(0, 12)}; imports allowlisted.${counter ? ` Challenge: ${counter}.` : ""}`);
process.exit(failed ? 1 : 0);
