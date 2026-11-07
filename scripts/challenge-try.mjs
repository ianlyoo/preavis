// One request to the challenge mandate, signed with its function-call key. The c1 mandate was never
// published and is retired (docs/challenge.md): its key is deleted, so this script has no target now.
// No dependencies (Node >= 18): the transaction is serialized (borsh) and signed (ed25519) here.
//
//   PREAVIS_CHALLENGE_KEY='ed25519:…' npm run challenge -- <payee> <amount in NEAR> [request_id]
//   PREAVIS_CHALLENGE_KEY='ed25519:…' npm run challenge -- --args '<request_spend JSON>' [--gas <Tgas>]
//
// The key comes from the environment, never from the command line. The mandate, the agent account
// and the key's public half come from testnet/challenge.json (PREAVIS_CHALLENGE_RECORD overrides).
// Prints what the contract answered and every transfer the mandate sent; the live counter is
// `npm run observe-testnet`.
import { createPrivateKey, randomBytes, sign } from "node:crypto";
import { readFileSync } from "node:fs";
import { base58, query, rpc, sha256 } from "./near-rpc.mjs";

const ALPHABET = "123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
const unbase58 = (text) => {
  let n = 0n;
  for (const c of text) {
    const i = ALPHABET.indexOf(c);
    if (i < 0) throw new Error("not base58");
    n = n * 58n + BigInt(i);
  }
  let hex = n.toString(16);
  if (hex.length % 2) hex = `0${hex}`;
  const zeros = text.length - text.replace(/^1+/, "").length;
  return Buffer.concat([Buffer.alloc(zeros), Buffer.from(n === 0n ? "" : hex, "hex")]);
};

// Borsh, just what a FunctionCall transaction needs.
const u32 = (n) => { const b = Buffer.alloc(4); b.writeUInt32LE(n); return b; };
const u64 = (n) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(n)); return b; };
const u128 = (n) => { const b = Buffer.alloc(16); b.writeBigUInt64LE(BigInt(n) & (2n ** 64n - 1n)); b.writeBigUInt64LE(BigInt(n) >> 64n, 8); return b; };
const str = (s) => { const b = Buffer.from(s, "utf8"); return Buffer.concat([u32(b.length), b]); };
const bytes = (b) => Buffer.concat([u32(b.length), b]);

const usage = () => {
  console.error(`usage: PREAVIS_CHALLENGE_KEY='ed25519:…' npm run challenge -- <payee> <amount in NEAR> [request_id]
       PREAVIS_CHALLENGE_KEY='ed25519:…' npm run challenge -- --args '<request_spend JSON>' [--gas <Tgas>]`);
  process.exit(2);
};
const record = JSON.parse(readFileSync(process.env.PREAVIS_CHALLENGE_RECORD ?? "testnet/challenge.json", "utf8"));
const secret = process.env.PREAVIS_CHALLENGE_KEY;
if (!secret?.startsWith("ed25519:")) usage();
const raw = unbase58(secret.slice(8));
if (raw.length !== 64) throw new Error("PREAVIS_CHALLENGE_KEY: expected an ed25519 secret key (64 bytes, base58)");
const publicKey = raw.subarray(32);
if (`ed25519:${base58(publicKey)}` !== record.agent_public_key) {
  throw new Error(`this key's public half is not the challenge key ${record.agent_public_key}`);
}
const privateKey = createPrivateKey({
  key: Buffer.concat([Buffer.from("302e020100300506032b657004220420", "hex"), raw.subarray(0, 32)]),
  format: "der", type: "pkcs8",
});

// The request.
const argv = process.argv.slice(2);
let args, gas = 100n;
if (argv[0] === "--args") {
  if (!argv[1]) usage();
  args = JSON.parse(argv[1]);
  const g = argv.indexOf("--gas");
  if (g > 0) gas = BigInt(argv[g + 1]);
} else {
  const [payee, amount, id] = argv;
  if (!payee || !/^\d+(\.\d{1,24})?$/.test(amount ?? "")) usage();
  const [whole, frac = ""] = amount.split(".");
  args = {
    request_id: id ?? `try-${randomBytes(4).toString("hex")}`,
    payee,
    amount: (BigInt(whole) * 10n ** 24n + BigInt(frac.padEnd(24, "0"))).toString(),
    policy_version: 1,
    invoice: "challenge",
  };
}

const access = await query({ request_type: "view_access_key", account_id: record.agent, public_key: record.agent_public_key });
const head = (await rpc("block", { finality: "final" })).header;
const tx = Buffer.concat([
  str(record.agent),
  Buffer.from([0]), publicKey,
  u64(BigInt(access.nonce) + 1n),
  str(record.mandate),
  unbase58(head.hash),
  u32(1),
  Buffer.from([2]), str("request_spend"), bytes(Buffer.from(JSON.stringify(args))), u64(gas * 10n ** 12n), u128(0),
]);
const signature = sign(null, sha256(tx), privateKey);
const signed = Buffer.concat([tx, Buffer.from([0]), signature]);

console.log(`${record.agent} -> ${record.mandate}: request_spend(${JSON.stringify(args)}), ${gas} Tgas`);
let result;
try {
  result = await rpc("send_tx", { signed_tx_base64: signed.toString("base64"), wait_until: "FINAL" });
} catch (error) {
  console.log(`refused before execution: ${error.message}`);
  process.exit(1);
}
const outcomes = [result.transaction_outcome, ...result.receipts_outcome];
const failure = outcomes.map((o) => o.outcome.status).find((s) => s.Failure);
if (failure) {
  const text = JSON.stringify(failure);
  console.log(`contract panicked: ${text.split("panicked: ")[1]?.split(/["\\]/)[0] ?? text.slice(0, 200)}`);
} else if ("SuccessValue" in result.status) {
  const attempt = JSON.parse(Buffer.from(result.status.SuccessValue, "base64").toString() || "null");
  console.log(`contract answered: ${attempt?.status}${attempt?.reason ? ` ${attempt.reason}` : ""}`);
}
const tx_hash = result.transaction.hash;
const detail = await rpc("EXPERIMENTAL_tx_status", { tx_hash, sender_account_id: record.agent, wait_until: "FINAL" });
const transfers = detail.receipts.filter((r) => r.predecessor_id === record.mandate)
  .flatMap((r) => r.receipt.Action.actions.filter((a) => a.Transfer).map((a) => `${(Number(BigInt(a.Transfer.deposit)) / 1e24).toFixed(6)} NEAR -> ${r.receiver_id}`));
console.log(`transfers from the mandate: ${transfers.length ? transfers.join(", ") : "none"}`);
console.log(`https://testnet.nearblocks.io/txns/${tx_hash}`);
console.log("The live counter: npm run observe-testnet");
