# Préavis

**You hold our agent's key. Try to make it pay you.**

*You control the agent. You cannot exceed its authority.*

Préavis is a spending mandate for an AI agent on NEAR. A contract account holds the agent's budget
under a fixed policy: which payees, how much each, how much in total, and how much the agent may
spend before the owner has to approve. The agent's working key is a NEAR function-call key that can
call one method, `request_spend`, and nothing else. We give you that key. Every claim below is
checked by a command you can run, and a false claim would fail that command.

*Préavis* is French for "advance notice": we announce the heist and hand you the key.

Status: **v0.8, NEAR testnet only.** The live mandate is **not locked yet**: its deployer key still
exists (see [THREAT_MODEL.md](THREAT_MODEL.md)). Deleting that key is the v0.9 step.

## 1. Verify everything in one command

Prerequisites: Linux, macOS or WSL with

- a C toolchain, `pkg-config` and OpenSSL headers. On Debian/Ubuntu:
  `sudo apt-get install -y build-essential pkg-config libssl-dev curl git nodejs npm`
  (on macOS, the Xcode command line tools and `brew install pkg-config openssl node`);
- Rust via rustup: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal`
  then `. "$HOME/.cargo/env"`. The repo's `rust-toolchain.toml` makes rustup fetch the pinned
  Rust 1.99.1 and the wasm target on first use;
- Node.js 18 or newer (only `npm run` and `observe-testnet` use it; there are no npm dependencies).

```sh
git clone https://github.com/ianlyoo/preavis.git
cd preavis
npm run verify
```

`npm run verify` is `cargo test --locked -- --nocapture --test-threads=1`. It builds the contract,
starts a fresh local NEAR sandbox, generates the agent's key on your machine, and asserts every
scenario on contract state, balances and execution outcomes. You need no account, secret or paid API.
The first run downloads Rust 1.99.1, the crates and the NEAR sandbox binary (nearcore 2.14.0), so it
needs a network connection. In a fresh Ubuntu 24.04 container `npm run verify` took 1 min 41 s, crate downloads and compilation included.

Expected output (abridged; keys and block numbers differ per run):

```text
test prebuilt_matches_source ... res/preavis.wasm is byte-identical to this source's build: sha256 3ce2d293ed41c1bf4ffb4c5cd93003665f1ed13ecc35157193f02185e1035431
test no_admin_surface ... exported methods: ["approve", "attempts", "contract_source_metadata", "get_request", "mandate", "new", "on_transfer", "reject", "request_spend"]; the only outgoing action is a transfer to the request's payee

  this is now your agent: agent.test.near
  your key: ed25519:Aw5r5RhGheUmyjnQKxFVz2UcFjuci2WWBnNPM1KgMi7A (generated on this machine)
  it can call mandate.test.near.request_spend and nothing else.
  the agent's full-access key is the owner's, offline; it only exists to fire the agent.

HELD  4  [protocol] the agent's key cannot attach a deposit
         observed: RPC error InvalidAccessKeyError(DepositWithFunctionCall), no record
HELD  7  [contract] paying an unlisted account (you) is refused
         observed: DENIED UNLISTED, record #0 + event, budget unchanged
HELD 13  [settlement] a failed transfer releases its charge exactly once
         observed: transfer receipt AccountDoesNotExist; FAILED, releases=1, budget back to 4 steps
HELD 23  [contract] two requests in flight at once cannot exceed the total cap
         observed: one SETTLED, one DENIED TOTAL_CAP at block 151 while the first transfer was unsettled (callback at block 153); vendor +4 steps once
...
Preavis verify: 25 claims held, 0 broken.
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

The full list of 25 claims, each with its command and expected line, is in [VERIFY.md](VERIFY.md).

## 2. Do X → see Y

All of these run inside `npm run verify`. Holding the agent's key, you:

| Do | See | Refused by |
|---|---|---|
| pay yourself (an unlisted account) | `DENIED UNLISTED`, recorded, NEP-297 event, budget unchanged | contract |
| pay an allowlisted vendor more than its cap | `DENIED PAYEE_CAP` | contract |
| pay more than the total, counting requests still waiting for approval | `DENIED TOTAL_CAP` | contract |
| split one big payment into small ones | past the agent's own 1 NEAR, each request waits for the owner (`AWAITING_APPROVAL`), reserved but not paid | contract |
| replay a request, or reuse its ID with other arguments | `DENIED REPLAY` / `DENIED ALTERED` | contract |
| claim another policy version | `DENIED POLICY_VERSION` | contract |
| attach a deposit | RPC error `DepositWithFunctionCall`; no transaction | protocol (access key) |
| call `approve`, `reject` or anything but `request_spend` | RPC error `MethodNameMismatch` | protocol (access key) |
| call the settlement callback from outside | `Method on_transfer is private` | contract |
| send two requests at once whose sum exceeds the total | one settles, the other `DENIED TOTAL_CAP` | contract (charged before the transfer leaves) |

And you see the paths that do move money:

| Do | See |
|---|---|
| an in-policy purchase | `SETTLED` only after the callback checks the transfer result; the vendor gains exactly the amount |
| a payment to an allowlisted account that does not exist | the transfer fails, the record is `FAILED` and the charge is released exactly once (`releases=1`), also after callback replays |
| the owner approves a held request with an altered amount | `BINDING_MISMATCH`, still held |
| the owner approves it exactly | paid once, `SETTLED` |
| the owner rejects it, twice | `REJECTED`, released once; the second reject panics `NOT_AWAITING_APPROVAL` |
| the settlement callback itself fails (test-only build) | the transfer happened once, the record stays `PENDING`, the charge stays frozen, nothing is refunded or resent |
| the owner fires the agent | one key deletion; the agent's next request fails with `AccessKeyNotFound` |

Denials return normally. They are not panics, so each one leaves a committed record and an event you
can read back.

## 3. Attack it yourself

```sh
npm run attack-local
```

This runs the attacks in [`attack/inputs.json`](attack/inputs.json) against a fresh sandbox mandate.
Each attack is signed either with the agent's key or with your own full-access account. Edit the
file, add your own attacks, and run it again (or point `PREAVIS_ATTACKS=/path/to/file.json` at
another file). Placeholders such as `$attacker`, `$vendor` and `"$near:1.5"` are explained in the
file. The run fails only if you broke an invariant: you received anything, a payee received more
than its settled records or its cap, the committed total exceeds the cap, or a charge was released
twice.

Expected output with the shipped inputs:

```text
  #  as       method         outcome
  0  agent    request_spend  denied, recorded: UNLISTED
     pay me
  3  agent    request_spend  denied, recorded: PAYEE_CAP
     overflow: u128::MAX
  6  agent    request_spend  held for the owner; reserved, not paid
     a big purchase from the vendor
  8  agent    request_spend  denied, recorded: ALTERED
     reuse its ID to pay me
  9  agent    approve        refused by the protocol: MethodNameMismatch
     approve it yourself with the agent key
 10  attacker approve        refused by the contract: OWNER_ONLY
     approve it from your own account
 15  agent    request_spend  PAID (inside the mandate)
     NOT stopped by design: an allowlisted vendor paid for the wrong purpose (THREAT_MODEL.md)
...
You made the agent pay you: 0 yoctoNEAR.
Invariants held: payees received exactly what settled records say and never more than their caps;
committed 2700000000000000000000000 <= total cap 4000000000000000000000000; no charge released twice.
```

## 4. Check the live testnet mandate, without any key

```sh
npm run observe-testnet
```

This is read-only and uses only Node. It reads [`testnet/receipts.json`](testnet/receipts.json), the
record of our one end-to-end testnet run, then re-reads from the chain the deployed code hash, the
key lists, the status and events of each recorded transaction, and the mandate's on-chain attempts.

```text
Observing mandate.preavis-1794025012.testnet on testnet (recorded run of 2026-11-07T04:18:39.000Z)

OK    deployed code hash equals sha256 of res/preavis.wasm
      chain 56g4mK248s34xLPdkfxNMb2X63T57fxJcnNP4N2uLQ72, local 56g4mK248s34xLPdkfxNMb2X63T57fxJcnNP4N2uLQ72 (hex 3ce2d293ed41c1bf4ffb4c5cd93003665f1ed13ecc35157193f02185e1035431)
OK    the agent's working key is function-call only (request_spend on the mandate); its only other key is the owner's offline full-access key
INFO  mandate access keys: 1 (the deployer key; NOT LOCKED yet: the lock deletes it, see THREAT_MODEL.md)
OK    paying an unlisted account (you) is refused
      success; events: denied UNLISTED; https://testnet.nearblocks.io/txns/...
OK    a failed transfer releases its charge exactly once
      success; events: pending, failed; AccountDoesNotExist; https://testnet.nearblocks.io/txns/...
...
Every recorded claim matches the chain.
```

**If you get HTTP 429 (rate limit):** the script already retries with backoff and falls back across
three public endpoints. If all of them fail, wait a minute or pass another testnet RPC, for example
`PREAVIS_TESTNET_RPC=https://rpc.testnet.near.org npm run observe-testnet`. The recorded run itself is
partial evidence; the local `npm run verify` is the primary check.

The human-readable log of that run, with a nearblocks link per transaction, is
[`testnet/run.txt`](testnet/run.txt).

## 5. Run it on testnet yourself (optional)

```sh
npm run execute-testnet
```

This creates a fresh testnet account through the NEAR testnet faucet helper
(`helper.testnet.near.org`). It gets about 10 testnet NEAR and a random key saved under the ignored
`.credentials/testnet/`, never printed. The run deploys the committed `res/preavis.wasm`, runs the 21
on-chain claims (about 2 minutes, about 5.7 testnet NEAR) and writes `target/preavis-testnet-run.json`.
Testnet NEAR has no value; nothing here touches mainnet.

- If the faucet helper is down, create a testnet account with
  [near-cli-rs](https://github.com/near/near-cli-rs) (`near account create-account sponsor-by-faucet-service ...`)
  and point `PREAVIS_OWNER_CREDENTIALS` at a credential file holding `account_id`, `public_key` and `secret_key`.
- The default RPC is `https://test.rpc.fastnear.com`. On HTTP 429, rerun with
  `PREAVIS_TESTNET_RPC=<another testnet RPC>`. The run creates fresh subaccounts, so after a failed run
  start again with a fresh owner rather than resuming.

## How it works

```
 you (agent key: function-call, request_spend only)
   │
   ▼
 mandate contract (holds the budget; policy fixed at init)
   ├─ check: request ID unused · policy version · payee allowlisted · amount > 0
   │         · payee cap · total cap (held + in-flight + settled) · agent's own limit
   ├─ denied  → record + NEP-297 event, return normally, nothing charged
   ├─ above the agent's own limit → charge (reserve), record AWAITING_APPROVAL, wait for the owner
   └─ allowed → charge first, then transfer ──► payee
                                   └─ private callback on_transfer:
                                        success → SETTLED
                                        failure → FAILED, release once
                                        callback fails → stays PENDING, charge frozen (visible)
 owner: approve(request, exact arguments) / reject(request). Nothing else.
```

The policy in every demo mandate: vendor ≤ 3 NEAR, hosting ≤ 3 NEAR, ghost (allowlisted, never
created) ≤ 1 NEAR, total ≤ 4 NEAR, and the agent alone may commit 1 NEAR. On testnet every amount is
1/1000 of that.

There is no method that deploys code, adds keys or edits the policy. `no_admin_surface` checks the
compiled wasm's export list and the source for that. A new policy means a new mandate.

The contract is under 400 lines of Rust in [`src/lib.rs`](src/lib.rs). The scenarios are in
[`tests/common/mod.rs`](tests/common/mod.rs) (shared by sandbox and testnet) and
[`tests/verify.rs`](tests/verify.rs).

## Reproducible build

- `rust-toolchain.toml` pins Rust 1.99.1. `Cargo.lock` pins near-sdk 5.30.1, near-workspaces 0.24.0
  and near-sandbox 0.4.0.
- `res/preavis.wasm` is the prebuilt contract; `res/preavis.wasm.sha256` is its hash.
- Rebuild it with `npm run build-wasm` (that is `scripts/build-wasm.sh`) and compare:
  `sha256sum target/wasm32-unknown-unknown/release/preavis.wasm res/preavis.wasm`.
  The script remaps local paths (`--remap-path-prefix`), so the bytes do not depend on where your
  checkout or Cargo registry live. The `prebuilt_matches_source` test asserts this on every
  `npm run verify`; it passed in a fresh Ubuntu 24.04 container too.
- The deployed testnet code hash is the base58 form of the same sha256 (`observe-testnet` checks it).

## Repository map

| Path | What |
|---|---|
| `src/lib.rs` | the mandate contract |
| `tests/verify.rs`, `tests/common/mod.rs` | `npm run verify`: 25 claims, plus `no_admin_surface` and `prebuilt_matches_source` |
| `tests/attack.rs`, `attack/inputs.json` | `npm run attack-local` |
| `tests/testnet.rs` | `npm run execute-testnet` |
| `scripts/observe-testnet.mjs` | `npm run observe-testnet` (read-only, Node only) |
| `testnet/receipts.json`, `testnet/run.txt` | the recorded testnet run |
| `res/` | prebuilt wasm and its sha256 |
| [VERIFY.md](VERIFY.md) | claim → command → expected output |
| [THREAT_MODEL.md](THREAT_MODEL.md) | who can do what, and what Préavis does not stop |
| `docs/` | journey (dated decisions), experiments, lessons |

## Disclosure: AI-generated code

AI coding agents (Anthropic's Claude, through Claude Code) wrote most of the code, tests and
documentation in this repository, from a design a human chose. That is also why every claim here comes with a command: do not take
the author's word for it, run it.

## Built with

near-sdk-rs 5.30.1, near-workspaces-rs 0.24.0, near-sandbox 0.4.0 (nearcore 2.14.0) and NEAR testnet,
plus FastNEAR's public testnet RPC and nearblocks for links.
Built at NEAR HACKATHON "Build Beyond Boundaries" (2026).
