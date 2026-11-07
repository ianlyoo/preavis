# VERIFY: claim → command → expected output

Every claim Préavis makes, the command that checks it, and what you should see. Run the commands from
a clone of this repo (prerequisites: [README §1](README.md#1-verify-everything-in-one-command)). If a
claim were false, its command would fail: each line below is printed only after the test has asserted
it on contract state, account balances or execution outcomes.

Layers: **protocol** = refused by NEAR itself (access-key rules), before any contract code runs ·
**contract** = the mandate's own checks · **settlement** = the transfer and its callback ·
**state** = balances and totals at the end · **setup** = deployment.

## A. `npm run verify` (local sandbox, no account or key needed)

Expected final lines:

```text
Preavis verify: 25 claims held, 0 broken.
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Each claim prints as `HELD <n>  [<layer>] <claim>` followed by `observed: …`. Keys, hashes and block
numbers vary per run.

| # | Claim | Layer | Expected `observed:` line |
|---:|---|---|---|
| 1 | mandate deployed and initialized with a fixed policy | setup | `mandate.test.near code sha256 3ce2d293ed41c1bf4ffb4c5cd93003665f1ed13ecc35157193f02185e1035431` |
| 2 | an owner method refuses the agent, even with a full-access key | contract | `panic OWNER_ONLY, no state change` |
| 3 | your key (the agent's working key) is a function-call key for request_spend on the mandate only | protocol | `agent keys: FunctionCall receiver=mandate methods=[request_spend] (yours) + the owner's offline full-access key` |
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
| 14 | splitting a payment does not dodge owner approval: past 10 steps the agent waits | contract | `split-a SETTLED, split-b AWAITING_APPROVAL (reserved, not paid)` |
| 15 | an approval with an altered amount is refused | contract | `panic BINDING_MISMATCH, request still held` |
| 16 | the owner's exact approval pays the held request once | settlement | `SETTLED; vendor balance +12 steps in total` |
| 17 | exceeding a payee's cap is refused | contract | `DENIED PAYEE_CAP, record #9 + event, budget unchanged` |
| 18 | exceeding the total cap is refused, held requests included | contract | `DENIED TOTAL_CAP, record #11 + event, budget unchanged` |
| 19 | a rejected request releases its reservation once | contract | `REJECTED, releases=1; a second reject panics NOT_AWAITING_APPROVAL` |
| 20 | nobody else can call the settlement callback | contract | `panic: Method on_transfer is private` |
| 21 | end state: you received nothing; only settled requests moved money | state | `attacker +0, hosting +0, vendor +12 steps = 3 settled requests; committed 12 of 40` |
| 22 | replaying the settlement callback changes nothing | settlement | `two self-calls of on_transfer on the FAILED request: releases still 1, budget unchanged` |
| 23 | two requests in flight at once cannot exceed the total cap | contract | `one SETTLED, one DENIED TOTAL_CAP at block 151 while the first transfer was unsettled (callback at block 153); vendor +4 steps once` |
| 24 | if the settlement callback fails, the charge stays frozen and visible; no refund, no resend | settlement | `test-only build (feature inject-callback-failure): vendor +3 steps once, record PENDING, committed 3, retry DENIED REPLAY` |
| 25 | firing the agent takes one key deletion; its next request is refused | protocol | `owner deleted the working key; next request: RPC error InvalidAccessKeyError(AccessKeyNotFound)` |

Amounts are in steps of 0.1 NEAR. The policy is vendor ≤ 30 steps, hosting ≤ 30, ghost ≤ 10,
total ≤ 40, and the agent alone may commit 10.

Claims 1–21 also run on testnet (section D). Claims 22–25 run only in the sandbox: 22 uses the
mandate's own deployer key, 23 and 24 deploy extra mandates (24 a test-only build), and 25 deletes the
agent's key.

## B. Same command, two more tests

| Claim | Command | Expected output |
|---|---|---|
| The mandate has no method that deploys code, adds or deletes keys, or edits the policy; its only outgoing action is a transfer to the request's payee | `cargo test --locked --test verify no_admin_surface -- --nocapture` | `exported methods: ["approve", "attempts", "contract_source_metadata", "get_request", "mandate", "new", "on_transfer", "reject", "request_spend"]; the only outgoing action is a transfer to the request's payee` |
| The committed `res/preavis.wasm` is exactly what this source builds to | `cargo test --locked --test verify prebuilt_matches_source -- --nocapture` | `res/preavis.wasm is byte-identical to this source's build: sha256 3ce2d293ed41c1bf4ffb4c5cd93003665f1ed13ecc35157193f02185e1035431` |
| The same, by hand | `npm run build-wasm && sha256sum target/wasm32-unknown-unknown/release/preavis.wasm && cat res/preavis.wasm.sha256` | the same hash twice: `3ce2d293ed41c1bf4ffb4c5cd93003665f1ed13ecc35157193f02185e1035431` |

`no_admin_surface` reads the export section of the compiled wasm. `contract_source_metadata` is the
read-only NEP-330 view that near-sdk adds to every contract. The source check also rejects the
promise actions `deploy_contract`, `add_full_access_key`, `add_access_key`, `delete_key`,
`delete_account`, `create_account`, `function_call` and `stake`.

## C. `npm run attack-local` (your own attacks)

| Claim | Command | Expected output |
|---|---|---|
| With the agent's key or your own account, you cannot get paid, overpay a payee or exceed the total | `npm run attack-local` (edit `attack/inputs.json` first, if you like) | `You made the agent pay you: 0 yoctoNEAR.` then `Invariants held: …`; `test result: ok. 1 passed` |

An attack that breaks an invariant makes this command fail with `INVARIANT BROKEN: [...]`.

## D. `npm run observe-testnet` (read-only, Node only, no key)

| Claim | Expected output |
|---|---|
| The deployed testnet code is `res/preavis.wasm` | `OK    deployed code hash equals sha256 of res/preavis.wasm` (`chain 56g4mK248s34xLPdkfxNMb2X63T57fxJcnNP4N2uLQ72`) |
| The live agent's working key is function-call only, and its only other key is the owner's offline full-access key | `OK    the agent's working key is function-call only (request_spend on the mandate); its only other key is the owner's offline full-access key` |
| The mandate is not locked yet (v0.8) | `INFO  mandate access keys: 1 (the deployer key; NOT LOCKED yet: …)` |
| Each of the 18 recorded testnet transactions has the recorded outcome and events | one `OK` line per transaction with its nearblocks link, ending `Every recorded claim matches the chain.` |

The run behind `testnet/receipts.json` executed claims 1–21 on testnet on 2026-11-07 at 04:18 UTC
(13:18 KST): `21 claims held, 0 broken` ([testnet/run.txt](testnet/run.txt)). Claims 4 and 5 are
protocol refusals with no transaction to link: the RPC rejects them before inclusion, as the log shows.

## E. `npm run execute-testnet` (optional, uses the testnet faucet)

| Claim | Command | Expected output |
|---|---|---|
| Claims 1–21 hold on a fresh testnet deployment you create | `npm run execute-testnet` | 21 `HELD` lines with nearblocks links, then `Preavis testnet: 21 claims held, 0 broken.` |

Prerequisites and 429 handling: [README §5](README.md#5-run-it-on-testnet-yourself-optional).
