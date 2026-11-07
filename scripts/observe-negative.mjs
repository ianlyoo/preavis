// Negative control for observe-testnet: each fixture is testnet/receipts.json with one recorded fact
// changed. The observer must exit non-zero on every one, and print the check that caught it.
import { execFileSync } from "node:child_process";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

const source = process.argv[2] ?? "testnet/receipts.json";
const record = JSON.parse(readFileSync(source, "utf8"));
const withTx = record.claims.filter((c) => c.tx);
const denial = withTx.find((c) => (c.events ?? []).some((e) => e.data?.[0]?.reason));
const other = withTx.find((c) => c.tx !== denial.tx && c.signer === denial.signer && c.events?.length);

// The v0.9 mandate: locked too, but its code (high-level promises) imports deploy_contract,
// add_key_* and function_call, which v1.0 removed.
const V09_MANDATE = "mandate.preavis-1794029520.testnet";

// Each fixture: [label, the line the observer must print, change to the run record, change to the
// challenge record (none: the challenge section is skipped)].
const fixtures = [
  ["substituted transaction: a denial claim points at another transaction by the same signer", /MISMATCH (actions|events)/, (r) => {
    r.claims.find((c) => c.claim === denial.claim).tx = other.tx;
  }],
  ["mismatched event: the recorded denial reason is changed", /MISMATCH events/, (r) => {
    const event = r.claims.find((c) => c.claim === denial.claim).events.find((e) => e.data?.[0]?.reason);
    event.data[0].reason = event.data[0].reason === "TOTAL_CAP" ? "UNLISTED" : "TOTAL_CAP";
  }],
  ["altered end state: the record claims the attacker was paid", /FAIL  the vendor, hosting, attacker and treasury balances/, (r) => {
    const id = r.attacker;
    r.end_state.balances[id] = (BigInt(r.end_state.balances[id]) + 10n ** 24n).toString();
  }],
  ["not locked: the record names an account that still has a key as the mandate", /FAIL  NOT LOCKED/, (r) => {
    r.mandate = r.owner;
  }],
  ["admin imports: the record names the v0.9 mandate, whose deployed code can deploy code and add keys", /NOT ALLOWED: env\.promise_batch_action_(deploy_contract|add_key)/, (r) => {
    r.mandate = V09_MANDATE;
  }],
  ["challenge payout: the challenge record's start balance is 1 NEAR higher, so 1 NEAR left without a record", /FAIL  challenge: paid to attackers/, () => {}, (c) => {
    c.mandate_start_balance = (BigInt(c.mandate_start_balance) + 10n ** 24n).toString();
  }],
];
const challengeSource = "testnet/challenge.json";

const dir = mkdtempSync(join(tmpdir(), "preavis-negative-"));
let wrong = 0;
for (const [i, [label, expect, mutate, mutateChallenge]] of fixtures.entries()) {
  const copy = structuredClone(record);
  mutate(copy);
  const file = join(dir, `fixture-${i + 1}.json`);
  writeFileSync(file, JSON.stringify(copy, null, 2));
  let challengeFile = "none";
  if (mutateChallenge) {
    const c = JSON.parse(readFileSync(challengeSource, "utf8"));
    mutateChallenge(c);
    challengeFile = join(dir, `fixture-${i + 1}-challenge.json`);
    writeFileSync(challengeFile, JSON.stringify(c, null, 2));
  }
  let code = 0, out = "";
  try {
    out = execFileSync("node", ["scripts/observe-testnet.mjs", file, challengeFile], { encoding: "utf8", stdio: ["ignore", "pipe", "pipe"] });
  } catch (error) {
    code = error.status ?? 1;
    out = `${error.stdout ?? ""}${error.stderr ?? ""}`;
  }
  const reasons = out.split("\n").filter((l) => expect.test(l)).slice(0, 2).map((l) => l.trim().slice(0, 150));
  const ok = code !== 0 && reasons.length > 0;
  if (!ok) wrong++;
  console.log(`${ok ? "OK  " : "FAIL"}  fixture ${i + 1}, ${label}: observer exit ${code}${reasons.length ? ", caught by:" : ", NOT for the expected reason"}`);
  for (const r of reasons) console.log(`      ${r}`);
}
console.log(wrong ? `\n${wrong} fixture(s) were NOT caught.` : `\nAll ${fixtures.length} altered records were rejected (non-zero exit).`);
process.exit(wrong ? 1 : 0);
