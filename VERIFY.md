# VERIFY: claim → command → expected output

Every claim Préavis makes, the command that checks it, and what you should see. Run the commands from
a clone of this repo (prerequisites: [README §1](README.md#1-verify-everything-in-one-command)). If a
claim were false, its command would fail: each line below is printed only after the test has asserted
it on contract state, account balances or execution outcomes.

Layers: **protocol** = refused by NEAR itself (access-key rules), before any contract code runs ·
**contract** = the mandate's own checks · **settlement** = the transfer and its callback ·
**state** = balances and totals at the end · **setup** = deployment.

## What is proven where

| Command | Needs | Proves |
|---|---|---|
| `npm run verify` | Rust (rustup), Node, network for the first download. No Docker. Linux, macOS or WSL, x86_64 or arm64 (where it was run: [README §1](README.md#1-verify-everything-in-one-command)) | the 35 claims below, run against the committed `res/preavis.wasm` (the bytes deployed on testnet); that the wasm exports no admin method; that it names the source commit it was built from and the contract's inputs have not changed since; plus `attack-local`, the recorded agent run and 64 random sequences of the invariant tests (section H) |
| `npm run verify:fuzz` | as `npm run verify` | the invariant tests over 2,048 random sequences with a random seed (section H) |
| `npm run verify:reproducible` | Docker (pulls the pinned `linux/amd64` Rust image; an arm64 host needs amd64 emulation) | byte identity: rebuilding that commit gives exactly `res/preavis.wasm`, its recorded sha256, and the code hash deployed on testnet |
| `npm run observe-testnet` | Node only, no key | the recorded testnet run: every transaction, event, failure and transfer, the end state, that the mandate is locked, and that the deployed code imports only allowlisted host functions; the challenge mandate (never published, retired): locked, key deleted, closed with its remainder sent only to `refund_to`, and `paid to attackers: 0` |
| `npm run agent`, `npm run owner` | Rust, Node | the agent and the owner as separate programs over a local node that survives restarts (section F) |

## A. `npm run verify` (local sandbox, no account or key needed)

Expected final lines of the main test:

```text
Preavis verify: 35 claims held, 0 broken.
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

`npm run verify` is `cargo test --locked`, so it also runs `tests/agent.rs` (section F, ending
`test result: ok. 2 passed`), `tests/attack.rs` (section C), `tests/fuzz.rs` (section H) and
`tests/limits.rs` (THREAT_MODEL "Not stopped" 12), each ending `test result: ok. 1 passed`. Each claim prints as
`HELD <n>  [<layer>] <claim>` followed by `observed: …`. Keys, hashes, amounts in yocto and block
numbers vary per run. With `PREAVIS_WASM=source` the claims run against a native build of `src/`
instead of the committed wasm (only claim 1's hash changes).

| # | Claim | Layer | Expected `observed:` line |
|---:|---|---|---|
| 1 | mandate deployed and initialized with a fixed policy | setup | `mandate.test.near code sha256 f5ca5dca0dbbd0e03b14606203af743b3b74840851b9e3da39c197d581fa5a6f` |
| 2 | an owner method refuses the agent, even with a full-access key | contract | `panic OWNER_ONLY, no state change` |
| 3 | the agent's working key (generated on this machine) is a function-call key for request_spend on the mandate only | protocol | `agent keys: FunctionCall receiver=mandate methods=[request_spend] (the working key) + the owner's offline full-access key` |
| 4 | the agent's key cannot attach a deposit | protocol | `RPC error InvalidAccessKeyError(DepositWithFunctionCall), no record` |
| 5 | the agent's key cannot call any other method (approve) | protocol | `RPC error InvalidAccessKeyError(MethodNameMismatch)` |
| 6 | only the agent can request a spend; not even the owner | contract | `panic AGENT_ONLY, no record` |
| 7 | paying an unlisted account (you) is refused | contract | `DENIED UNLISTED, record #0 + event, budget unchanged` |
| 8 | a request under another policy version is refused | contract | `DENIED POLICY_VERSION, record #1 + event, budget unchanged` |
| 9 | a zero-amount request is refused | contract | `DENIED ZERO_AMOUNT, record #2 + event, budget unchanged` |
| 10 | an in-policy purchase settles | settlement | `SETTLED after checking the transfer result; vendor balance +4 steps exactly` |
| 11 | replaying a settled request is refused | contract | `DENIED REPLAY, record #4 + event, budget unchanged` |
| 12 | reusing a request ID with altered arguments is refused | contract | `DENIED ALTERED, record #5 + event, budget unchanged` |
| 13 | a failed transfer releases its charge exactly once | settlement | `transfer receipt AccountDoesNotExist; FAILED, releases=1, budget back to 4 steps` |
| 14 | splitting a payment does not dodge owner approval: past 10 steps in a period the agent waits | contract | `split-a SETTLED, split-b AWAITING_APPROVAL (held, not charged, not paid)` |
| 15 | an approval with an altered amount is refused | contract | `panic BINDING_MISMATCH, request still held` |
| 16 | an approval for a different invoice is refused | contract | `panic BINDING_MISMATCH, request still held` |
| 17 | the owner's exact approval pays the held request once | settlement | `SETTLED; vendor balance +12 steps in total` |
| 18 | exceeding a payee's cap is refused | contract | `DENIED PAYEE_CAP, record #9 + event, budget unchanged` |
| 19 | exceeding the total cap is refused | contract | `DENIED TOTAL_CAP, record #10 + event, budget unchanged` |
| 20 | a rejected request is closed once and nothing is paid | contract | `REJECTED, held back to 0, committed unchanged; a second reject panics NOT_AWAITING_APPROVAL` |
| 21 | nobody else can call the settlement callback | contract | `panic: Method on_transfer is private` |
| 22 | end state: you received nothing; only settled requests moved money | state | `attacker +0, hosting +0, vendor +12 steps = 3 settled requests; committed 12 of 40` |
| 23 | replaying the settlement callback changes nothing | settlement | `two self-calls of on_transfer on the FAILED request: releases still 1, budget unchanged` |
| 24 | locked: the mandate account has no access key, so nobody can redeploy it or move its balance outside the contract | protocol | `deleted the deployer's full-access key; view_access_key_list: 0 keys` |
| 25 | after the lock the contract still works: an in-policy request settles | settlement | `SETTLED; vendor balance +1 step exactly` |
| 26 | the mandate can't be initialized twice: owner, agent, refund_to and policy stay as set | contract | `new() again from the mandate's own key: panic "The contract has already been initialized"; from any other account: panic "Method new is private"` |
| 27 | two requests in flight at once cannot exceed the total cap | contract | `one SETTLED, one DENIED TOTAL_CAP at block 162 while the first transfer was unsettled (callback at block 164); vendor +4 steps once` |
| 28 | if the settlement callback fails, the charge stays frozen and visible; no refund, no resend | settlement | `test-only build (feature inject-callback-failure): vendor +3 steps once, record PENDING, committed 3, retry DENIED REPLAY` |
| 29 | hostile requests can't grow storage without bound, and the held request still settles | contract | `limits per period 3 payments + 3 stored denials; the 4th request DENIED EXHAUSTED, mandate() shows exhausted until resets_at <height>; 5 more denials only counted, storage unchanged at <n> bytes; largest record +407 bytes <= 500 reserved; underfunded init panics BALANCE_BELOW_CAP_AND_STORAGE` |
| 30 | the agent's own budget and request slots are per period: exhausted until the period's last block, fresh from the next one's first | contract | `period of 40 blocks, 3 steps and 4 requests each; period 0: p0-a SETTLED, p0-b AWAITING_APPROVAL, p0-c SETTLED, p0-d AWAITING_APPROVAL; requests landing before block 305: 4, all DENIED EXHAUSTED; edge-4 landing at block 307 SETTLED in period 1` |
| 31 | a failed transfer gives its share of the period's budget back | settlement | `period 1: 1 step used, a 2-step transfer to the never-created account FAILED, period budget back to 1 step; the next 2-step request settles without the owner` |
| 32 | a held request reserves nothing: its approval is checked against the caps as they are then | contract | `vendor cap 6 steps, 4 settled; approving p0-b (2) brings vendor to 6 and pays; approving p0-d (1) panics PayeeCap, p0-d stays AWAITING_APPROVAL` |
| 33 | after expires_at no new spend is accepted or approved, and anyone can close the mandate, to refund_to only | contract | `past block 370: request and approve panic EXPIRED; before it a stranger's close panicked OWNER_ONLY, after it the stranger's close sent <amount> yocto (balance - storage stake) to refund_to and nothing to the stranger` |
| 34 | closing sends the remainder only to refund_to; afterwards nothing is approved or requested | contract | `refund_to +<amount> yocto = balance - storage stake; approving the held keep-1 panics CLOSED, rejecting it works; next request panics CLOSED` |
| 35 | firing the agent takes one key deletion; its next request is refused | protocol | `owner deleted the working key; next request: RPC error InvalidAccessKeyError(AccessKeyNotFound)` |

Amounts are in steps of 0.1 NEAR. The policy is vendor ≤ 30 steps, hosting ≤ 30, ghost ≤ 10,
total ≤ 40, and per period of 1,008,000 blocks (about a week) the agent alone may commit 10 steps,
make at most 15 payments and leave at most 15 stored denials; the mandate expires after six
periods. Every request names an invoice reference, which the owner's approval must repeat (claim 16).

Claims 1–22, 24 and 25 also run on testnet (section D). Claims 23 and 26–35 run only in the sandbox:
23 calls the callback with the mandate's own key, 26 re-initializes, 27–34 deploy extra mandates (28
a test-only build; 30–33 fast-forward the sandbox's block height to a period edge and past expiry),
and 35 deletes the agent's key.

## B. Same command, two static tests

| Claim | Command | Expected output |
|---|---|---|
| The mandate has no method that deploys code, adds or deletes keys, or edits the policy; it sends money only to a request's payee and, on `close`, to `refund_to` | `cargo test --locked --test verify no_admin_surface -- --nocapture` | `exported methods: ["approve", "attempts", "close", "contract_source_metadata", "get_request", "mandate", "new", "on_transfer", "reject", "request_spend"]; 22 imported host functions, all in the allowlist (no deploy, key, account, stake or cross-contract call); the only outgoing receipts: a transfer to the request's payee then on_transfer on itself, and on close a transfer to refund_to` |
| The committed `res/preavis.wasm` was built from a named commit, and the contract's inputs are unchanged since | `cargo test --locked --test verify prebuilt_is_from_this_source -- --nocapture` | `res/preavis.wasm (sha256 f5ca5dca0dbbd0e03b14606203af743b3b74840851b9e3da39c197d581fa5a6f) was built from commit 91161947518540d81027bb1f09ebe8b2381fddfc; the contract's inputs are unchanged since. Byte identity: npm run verify:reproducible (Docker)` |

`no_admin_surface` reads the export and the import sections of the wasm. The contract sends money
with the low-level host functions (`promise_batch_create`, `promise_batch_action_transfer`,
`promise_then`, `promise_return`), so its wasm imports exactly 22 host functions, every one on the
allowlist `ALLOWED_IMPORTS` in `tests/verify.rs`. A host function the code does not import cannot be
called by it: there is no `promise_batch_action_deploy_contract`, no `add_key_*`, no `delete_key`, no
`function_call`, no `stake`, no `create_account` or `delete_account` in the bytes. `observe-testnet`
reads the deployed code back from the chain (`view_code`) and applies the same allowlist.
`contract_source_metadata` is the read-only NEP-330 view; it returns the build environment, command
and source commit. The source check counts two `promise_batch_create`, two transfers and one
`promise_then`.

`prebuilt_is_from_this_source` does not rebuild: a native build differs between x86_64 and arm64
hosts (Cargo hashes the host triple into symbol names). Byte identity is section E.

## C. `npm run attack-local` (your own attacks)

| Claim | Command | Expected output |
|---|---|---|
| With the agent's key or your own account, you cannot get paid, overpay a payee or exceed the total | `npm run attack-local` (edit `attack/inputs.json` first, if you like) | `You made the agent pay you: 0 yoctoNEAR.` then `(measured: the transfers to attacker.test.near in all 32 receipts of your attacks, read back from the node)` and `Invariants held: …`; `test result: ok. 1 passed` |

The number is measured: the test reads back every receipt your attacks produced and sums the
transfers to your account. An attack that breaks an invariant makes this command fail with
`INVARIANT BROKEN: [...]`.

## D. `npm run observe-testnet` (read-only, Node only, no key)

| Claim | Expected output |
|---|---|
| The deployed testnet code is `res/preavis.wasm` | `OK    deployed code hash equals sha256 of res/preavis.wasm …` (`chain HYTrsLM8vmnBYBEo236nCxSY1ouRpUdFvF6jB7QgzkhU`), naming source commit `91161947518540d81027bb1f09ebe8b2381fddfc` |
| The deployed code, read back from the chain, imports only allowlisted host functions | `OK    mandate.preavis-1794034063.testnet: the deployed code imports only allowlisted host functions (view_code, 22 imports)` |
| The live agent's working key is function-call only; its only other key is the owner's offline full-access key | `OK    the agent's working key is function-call only …` |
| Each of the 21 recorded transactions matches: signer, receiver, method and arguments, deposit, success or failure, every event (with its reason), every failure, every transfer the mandate sent | one `OK` line per transaction with its decoded call, events, transfers and nearblocks link |
| Claims 4, 5 and 22 have no transaction | `INFO` lines: 4 and 5 `no transaction: the network refused to accept it, so nothing reached the chain`; 22 `a statement about the end state, re-checked from the chain below` |
| The end state matches: `mandate()`, all 13 attempts, `committed` and `payee_committed` equal the settled and in-flight attempts, `held` the awaiting ones, no cap exceeded, the vendor / hosting / attacker / treasury balances | `OK` lines under `End state, re-read from the chain` |
| The window fields of `mandate()` (period, `resets_at`, the period's counters, `exhausted`, `expired`) follow the block height | `OK    the window fields (…) are consistent with the recorded state at the current height` |
| The mandate is locked | `OK    locked: no key can sign for mandate.preavis-1794034063.testnet` |
| The challenge mandate (`testnet/challenge.json`; never published, retired): same code, allowlisted imports, locked, the policy as recorded, totals equal to its attempts, no cap exceeded | `OK    challenge: …` lines |
| It is retired: its function-call key is gone, the key deletion and the close are re-read from the chain, the close sent its remainder only to `refund_to`, and `mandate()` reports closed | `OK    challenge: retired: …` lines |
| Nothing left the challenge mandate for anyone outside its allowlist | `OK    challenge: paid to attackers is 0: …`, from the stored attempts and, independently, from the balance (it may fall only by the allowlisted payments and the refund) |
| The counter | `retired, never published; attempts: 0 / paid to attackers: 0` |
| All of it | last line `OK: 21 recorded transactions match the chain; end state re-read; locked: no key can sign for it; code hash HYTrsLM8vmnBYBEo236nCxSY1ouRpUdFvF6jB7QgzkhU = reproducible build of 911619475185; imports allowlisted. Challenge: retired, never published; attempts: 0 / paid to attackers: 0.`, exit 0 |

Any mismatch prints `FAIL` with a `MISMATCH <field>: chain … | recorded …` line and exits 1; so does a
mandate that still has a key (`FAIL  NOT LOCKED: …`) or code with a host import outside the
allowlist (`NOT ALLOWED: env.<name>`).

**Negative control:** `npm run observe-testnet:negative` writes six altered copies of the records and
requires the observer to exit non-zero on each, for the expected reason:

1. a claim pointing at another transaction;
2. a changed denial reason;
3. a balance claiming the attacker was paid;
4. a mandate that still has a key;
5. the v0.9 mandate, locked too, whose code (high-level promises) imports
   `promise_batch_action_deploy_contract` and `add_key_with_full_access`;
6. a challenge record whose start balance is 1 NEAR higher, so 1 NEAR would have left without a
   record.

Expected last line: `All 6 altered records were rejected (non-zero exit).` It takes about 12 minutes,
since every fixture re-reads the chain through public RPCs that rate-limit.

The run behind `testnet/receipts.json` executed claims 1–22, 24 and 25 on testnet on 2026-11-07 at
06:49 UTC (15:49 KST): `24 claims held, 0 broken` ([testnet/run.txt](testnet/run.txt)). Claims 4 and 5
are protocol refusals with no transaction to link: the RPC rejects them before inclusion, as the log
shows. The lock is claim 24.

## E. `npm run verify:reproducible` (needs Docker)

| Claim | Expected output |
|---|---|
| Rebuilding the commit named in `res/preavis.wasm` in the pinned image gives the same bytes | `OK    rebuilt from commit 91161947518540d81027bb1f09ebe8b2381fddfc: byte-identical to res/preavis.wasm` |
| … equal to the recorded hash | `OK    equals res/preavis.wasm.sha256` (`f5ca5dca0dbbd0e03b14606203af743b3b74840851b9e3da39c197d581fa5a6f`) |
| … equal to the code deployed on testnet | `OK    equals the code deployed on testnet at mandate.preavis-1794034063.testnet` (`HYTrsLM8vmnBYBEo236nCxSY1ouRpUdFvF6jB7QgzkhU`); the challenge mandate runs the same code hash (section D) |

The image is `rust:1.99.1-slim-bookworm` pinned by its `linux/amd64` digest
(`sha256:392f221964669402bf4ae133d2a46c3ae270f64734133f4b2703d40019235c1a`, see
`scripts/build-reproducible.sh`). The build reads only `git archive` of that commit, so local changes
and paths do not matter. It embeds NEP-330 build info (environment, command, source commit).

On arm64 the same amd64 image runs under emulation (Docker Desktop and OrbStack include it), and the
comparison is the same byte comparison: there is no native-arm64 hash to compare against. Under
emulation the script passes `CARGO_BUILD_JOBS=1` into the container by default, because parallel
rustc processes under QEMU have run out of memory; set `PREAVIS_BUILD_JOBS=<n>` to change it (on
x86_64 the default is Cargo's own). Measured by the second judge round on Apple Silicon (v0.9):
`npm run verify` 190 s including downloads, `npm run verify:reproducible` 90 s, and with
`CARGO_BUILD_JOBS=1` 4 min 23 s. We have no arm64 host and did not re-run it for v1.0.

## F. `npm run agent` and `npm run owner` (an LLM agent and its owner, local sandbox)

`npm run agent` is a program, not a test: it starts a local NEAR node whose state lives in `.preavis/`
(or `$PREAVIS_OFFICE`), creates the mandate on first use, works through `agent/inbox/` once, and
stops the node. Requests above the agent's own limit stay `AWAITING_APPROVAL` on chain. `npm run
owner -- list | show <id> | approve <id> | reject <id>` restarts the node from disk and acts as the
owner. `npm run verify` runs both programs end to end in `tests/agent.rs`, restarting the node four
times.

| Claim | Expected output |
|---|---|
| The run says whether a model is live or a recording | first line `MODEL: REPLAYING A RECORDING, not a live model (agent/transcript.json, recorded on 2026-11-07).` then the recorded model, or `MODEL: live, …` |
| The in-policy invoice settles | `contract SETTLED; transferred 0.4 NEAR -> vendor.test.near` |
| The large invoice and both halves of a split payment wait for the owner | `contract AWAITING_APPROVAL; transferred nothing` for inv-2210, inv-1050-1 and inv-1050-2 |
| The injected payment and the changed payment account are refused by the contract, whatever the model does | `WHATEVER THE MODEL DOES: the agent key sends what agent/inbox/3-urgent.txt asks (3.5 NEAR -> attacker.test.near): DENIED UNLISTED; transferred nothing`, the same for `6-acme-new-account.txt` (0.3 NEAR) |
| Proposed actions vs measured transfers | the `PROPOSED BY THE MODEL vs MEASURED TRANSFERS (from each transaction's receipts)` table |
| A hold survives a restart | `npm run owner -- list` (a new process, the node restarted from disk): `3 request(s) awaiting your approval` |
| The owner sees the invoice bytes, their sha256, the payee and the amount before signing | `npm run owner -- approve inv-2210`: `payee hosting.test.near`, `amount 2.5 NEAR (2500000000000000000000000 yoctoNEAR)`, `invoice sha256 155951782b63…`, the invoice verbatim, then `contract SETTLED; transferred 2.5 NEAR -> hosting.test.near` |
| A rejection pays nothing | `npm run owner -- reject inv-1050-2`: `contract REJECTED; transferred nothing` |
| A failed transfer is reported as failed, never as paid | `tests/agent.rs` runs `npm run agent` with `PREAVIS_MODEL_CMD="node agent/ghost-model.mjs"`, a stand-in that always proposes 0.1 NEAR to the allowlisted, never-created `ghost.test.near`: `contract REFUSED AccountDoesNotExist; transfer of 0.1 NEAR -> ghost.test.near attempted, failed: nothing paid` for each invoice, `nothing (0.1 NEAR -> ghost.test.near attempted, failed)` in the table, then `Preavis agent: 7 transfer(s) to ghost.test.near attempted and failed (AccountDoesNotExist), 7 FAILED record(s), committed 0; reported as failed, not as paid.` |
| Measured result | `MEASURED by balance, equal to the settled records: vendor.test.near +0.4 NEAR, hosting.test.near +2.8 NEAR, attacker.test.near +0 NEAR.` |
| What the contract does not stop | `NOT stopped, by design: inv-2215 (consulting nobody ordered) paid 0.3 NEAR to hosting.test.near, an allowlisted payee, within the agent's own limit.` |

In the recording, the model declined both attacker invoices itself (`NOTHING SENT`). With
`PREAVIS_MODEL_CMD="node agent/hijacked-model.mjs"`, a stand-in that obeys every invoice, the agent
does send them and the contract answers `DENIED UNLISTED`. The recording's model is pinned in
`scripts/local-model.sh` (README §4).

## G. `npm run execute-testnet` (optional, uses the testnet faucet)

| Claim | Command | Expected output |
|---|---|---|
| Claims 1–22, 24 and 25 hold on a fresh testnet deployment you create, ending locked | `npm run execute-testnet` | 24 `HELD` lines with nearblocks links, then `Preavis testnet: 24 claims held, 0 broken.` |

Prerequisites and 429 handling: [README §6](README.md#6-run-it-on-testnet-yourself-optional).

## H. `npm run verify:fuzz` (invariant tests, random sequences)

Self-verification beyond the scripted claims: `tests/fuzz.rs` runs seeded random sequences of
operations, each against a fresh sandbox mandate with a random policy, and checks the invariants after
every step. Nothing touches testnet and no model is involved.

What this does and does not show. Within one mandate the steps are **sequential**: each step's
transaction runs to its final result, `on_transfer` included, before the next is sent. 64 mandates
run **in parallel**, each with its own sequence and independent of the others. So the random
sequences do not interleave operations on one mandate; two requests in flight at once on one
mandate are a separate, fixed scenario (claim 27, `tests/verify.rs:120`). The invariants below are
checked on the executions sampled, not proven for every policy, schedule or interleaving.

| Command | Sequences × steps | Seed | Expected last line |
|---|---|---|---|
| `npm run verify` (part of it) | 64 × 40 | 494 | `Preavis invariants: 64 sequences × 40 steps (2560 steps, <n> transactions), seed 494, <s> s: 0 invariant violations.` |
| `npm run verify:fuzz` | 2,048 × 40 | random, printed on the first line | `Preavis invariants: 2048 sequences × 40 steps (81920 steps, <n> transactions), seed <seed>, <s> s: 0 invariant violations.` |

`PREAVIS_FUZZ_SEED`, `PREAVIS_FUZZ_SEQUENCES` and `PREAVIS_FUZZ_STEPS` override the defaults;
`PREAVIS_FUZZ_FEATURE` selects a test-only build, as in the negative control below. A seed fixes the policies and the operations; the sandbox makes blocks in real time, so the block each step
lands in, and with it where period edges fall, varies between runs. Before the last line both modes
print how many steps ended in each outcome (e.g. `Agent request_spend → DENIED EXHAUSTED`), and the
long run prints a progress line per 64 sequences.

**Each sequence** gets a random policy: four allowlisted payees with caps, a total cap, an approval
threshold, periods of 10–30 blocks, `expires_at` 100–900 blocks out, 1–2 request slots and 0–4
stored denials per period. In one sequence of ten, amounts count in yoctoNEAR instead of 0.1 NEAR.
Then come 38 random steps and two closes: a stranger's (refused before `expires_at`), then the
owner's. The random steps are:

- `request_spend` to the allowlisted payees (one never created, so its transfer fails; one an
  implicit 64-hex account), look-alikes (`vend0r.…`, `vendorr.…`, `vendor.….x`, the upper-case
  name), sub-accounts of payees, an unlisted implicit account, `refund_to`, the owner, the agent, the
  stranger, the mandate itself and an unlisted account that exists;
- amounts of 0, 1 yocto, a few units, each cap −1/±0/+1, the payee's remaining cap ±1, the remaining
  total ±1, the agent's remaining own budget ±1, `u128::MAX`, `u128::MAX − 1` and 2¹²⁷;
- fresh, reused (altered) and replayed request IDs, empty and 65-byte IDs and invoices, wrong policy
  versions (±1, 0, `u32::MAX`), other invoices, and requests signed by the owner or a stranger;
- `approve` with the exact binding or with an altered amount, invoice, payee or version, and
  `reject`, both also signed by the agent or a stranger;
- a period crossing: wait for the next period and resend the last request denied `EXHAUSTED`;
- `close` by the owner or a stranger at a random point.

**After every step**, read from the chain:

| | Invariant | Checked against |
|---|---|---|
| I1 | every transfer out of the mandate goes to an allowlisted payee within its cap, for a request settled or failed in that step, or to `refund_to` from `close` signed by the owner or landing at or after `expires_at`; a stranger's close succeeds exactly when it lands at or after `expires_at`; no look-alike, sub-account or unlisted implicit account ever comes to exist | every transfer in the step's receipts, read back from the node; the `closed` event; `view_account` at the end |
| I2 | `committed` ≤ total cap; each payee's committed ≤ its cap; `period_committed`, and the agent's own settled amount in every period, ≤ `approval_threshold` | `mandate()` and all attempts |
| I3 | after normal callback completion (the step's transaction finished, its `on_transfer` included): `committed` and each payee's committed equal the sum of settled requests; `held` equals the sum awaiting; nothing is left `PENDING`; a failed request has `releases` 1, every other 0; vendor, hosting and the implicit payee gain exactly what settled to them, `refund_to` exactly the close refund, the unlisted account nothing | `mandate()`, all attempts, balances |
| I4 | the mandate's balance falls by exactly the settled payments plus the close refund (it may gain at most what its own receipts burnt; in the sandbox it gains nothing) | its balance before and after |
| I5 | on an open, unexpired mandate (the resend is skipped once it is closed or past `expires_at`, and a resend landing at or after `expires_at` is not counted), a request denied `EXHAUSTED` in period N, resent in a later period with a valid request ID (its own, or a fresh one if its own was taken since), is accepted (`SETTLED`, `FAILED` or `AWAITING_APPROVAL`) in a period after N whenever it still fits the payee's cap and the total cap | the returned attempt; `mandate()` read just before |

**Scope of I3.** "Nothing is left `PENDING`" holds only after normal callback completion. If the
settlement callback itself fails, the request stays `PENDING` and its charge stays frozen, by design
(claim 28: no refund, no resend); the oracle then reports I3 (or I1, see the negative control). The
general safety statement, which also holds in that case, is: `committed` = sum of `PENDING` +
`SETTLED` requests, per payee as well, within the payee caps and the total cap.

**Expiry.** `request_spend` and `approve` are refused from `expires_at` on: no new spend is accepted
or approved after expiry. A transfer already sent before it still completes, and its `on_transfer`
has no expiry check; `close` then pays the remainder to `refund_to`.

A violation stops the run with `INVARIANT VIOLATED: I<n> …`, the seed and the sequence number. The
test then replays the sequence on fresh mandates, removing one step at a time (a greedy pass, not a
guaranteed shortest sequence), prints the shorter sequence that still fails, step by step, and the
command that reproduces it, including `PREAVIS_FUZZ_FEATURE` when the run used one.

**Negative control.** `PREAVIS_FUZZ_FEATURE=inject-callback-failure` runs the test-only build
whose settlement callback always panics (claim 28). Measured on the final test code:
`PREAVIS_FUZZ_FEATURE=inject-callback-failure PREAVIS_FUZZ_SEED=494 PREAVIS_FUZZ_SEQUENCES=8 npm run
verify:fuzz` stopped at step 2 of sequence 0 with `INVARIANT VIOLATED: I1 transfer of
200000000000000000000000 yocto to ghost.s0.test.near without a request settled or failed in this
step` and shrank the sequence to its one payment request; the command exits non-zero.

**Measured long run** (2026-11-07, commit `e1a3b86`, a clean `ubuntu:24.04` container on Apple
Silicon via colima, 6 CPUs; not run on x86_64):

```text
invariant tests: 2048 sequences × 40 steps, seed 1794048948887891796
  64 of 2048 sequences: 0 invariant violations (46 s)
  …
Preavis invariants: 2048 sequences × 40 steps (81920 steps, 73178 transactions), seed 1794048948887891796, 1478 s: 0 invariant violations.
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1478.36s
```

Among those steps: 4,687 requests `SETTLED` and 1,727 `FAILED` (the never-created payee), 1,710 held
for the owner, 564 owner approvals settled and 181 failed, 605 rejections, 1,128 `DENIED EXHAUSTED`;
of 638 I5 resends, 590 were accepted, 41 no longer fitted a cap (`DENIED PAYEE_CAP` / `TOTAL_CAP`)
and 7 landed after `expires_at`. Strangers' closes: 275 after `expires_at` (paid `refund_to`), 2,184
refused `OWNER_ONLY` before it. In the same container, the 64-sequence run inside `npm run verify`
took 49 s.
