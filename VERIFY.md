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
| `npm run verify` | Rust (rustup), Node, network for the first download. No Docker. Linux, macOS or WSL, x86_64 or arm64 (where it was run: [README §1](README.md#1-verify-everything-in-one-command)) | the 30 claims below, run against the committed `res/preavis.wasm` (the bytes deployed on testnet); that the wasm exports no admin method; that it names the source commit it was built from and the contract's inputs have not changed since; plus `attack-local` and the recorded agent run |
| `npm run verify:reproducible` | Docker (pulls the pinned `linux/amd64` Rust image; an arm64 host needs amd64 emulation) | byte identity: rebuilding that commit gives exactly `res/preavis.wasm`, its recorded sha256, and the code hash deployed on testnet |
| `npm run observe-testnet` | Node only, no key | the recorded testnet run: every transaction, event, failure and transfer, the end state, and that the mandate is locked |

## A. `npm run verify` (local sandbox, no account or key needed)

Expected final lines of the main test:

```text
Preavis verify: 30 claims held, 0 broken.
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

`npm run verify` is `cargo test --locked`, so it also runs `tests/agent.rs` (section F) and
`tests/attack.rs` (section C), each ending `test result: ok. 1 passed`. Each claim prints as
`HELD <n>  [<layer>] <claim>` followed by `observed: …`. Keys, hashes, amounts in yocto and block
numbers vary per run. With `PREAVIS_WASM=source` the claims run against a native build of `src/`
instead of the committed wasm (only claim 1's hash changes).

| # | Claim | Layer | Expected `observed:` line |
|---:|---|---|---|
| 1 | mandate deployed and initialized with a fixed policy | setup | `mandate.test.near code sha256 900a452b59841520e10327c6409b5d3e81265b6c57edd5b1f25c5109fcab6f82` |
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
| 16 | an approval for a different invoice is refused | contract | `panic BINDING_MISMATCH, request still held` |
| 17 | the owner's exact approval pays the held request once | settlement | `SETTLED; vendor balance +12 steps in total` |
| 18 | exceeding a payee's cap is refused | contract | `DENIED PAYEE_CAP, record #9 + event, budget unchanged` |
| 19 | exceeding the total cap is refused, held requests included | contract | `DENIED TOTAL_CAP, record #11 + event, budget unchanged` |
| 20 | a rejected request releases its reservation once | contract | `REJECTED, releases=1; a second reject panics NOT_AWAITING_APPROVAL` |
| 21 | nobody else can call the settlement callback | contract | `panic: Method on_transfer is private` |
| 22 | end state: you received nothing; only settled requests moved money | state | `attacker +0, hosting +0, vendor +12 steps = 3 settled requests; committed 12 of 40` |
| 23 | replaying the settlement callback changes nothing | settlement | `two self-calls of on_transfer on the FAILED request: releases still 1, budget unchanged` |
| 24 | locked: the mandate account has no access key, so nobody can redeploy it or move its balance outside the contract | protocol | `deleted the deployer's full-access key; view_access_key_list: 0 keys` |
| 25 | after the lock the contract still works: an in-policy request settles | settlement | `SETTLED; vendor balance +1 step exactly` |
| 26 | two requests in flight at once cannot exceed the total cap | contract | `one SETTLED, one DENIED TOTAL_CAP at block N while the first transfer was unsettled (callback at block N+2); vendor +4 steps once` |
| 27 | if the settlement callback fails, the charge stays frozen and visible; no refund, no resend | settlement | `test-only build (feature inject-callback-failure): vendor +3 steps once, record PENDING, committed 3, retry DENIED REPLAY` |
| 28 | hostile requests can't grow storage without bound, and the held request still settles | contract | `limits 3 payments + 3 stored denials; 5 more denials only counted, storage unchanged at <n> bytes; largest record +399 bytes <= 500 reserved; underfunded init panics BALANCE_BELOW_CAP_AND_STORAGE` |
| 29 | closing sends the remainder only to refund_to; held requests stay payable; the agent is refused | contract | `refund_to +<amount> yocto = balance - storage stake - held 4 steps; keep-1 approved after close and paid; next request panics CLOSED` |
| 30 | firing the agent takes one key deletion; its next request is refused | protocol | `owner deleted the working key; next request: RPC error InvalidAccessKeyError(AccessKeyNotFound)` |

Amounts are in steps of 0.1 NEAR. The policy is vendor ≤ 30 steps, hosting ≤ 30, ghost ≤ 10,
total ≤ 40, the agent alone may commit 10, and at most 30 payments and 30 stored denials. Every
request names an invoice reference, which the owner's approval must repeat (claim 16).

Claims 1–22, 24 and 25 also run on testnet (section D). Claims 23 and 26–30 run only in the sandbox:
23 calls the callback with the mandate's own key, 26–29 deploy extra mandates (27 a test-only build),
and 30 deletes the agent's key.

## B. Same command, two static tests

| Claim | Command | Expected output |
|---|---|---|
| The mandate has no method that deploys code, adds or deletes keys, or edits the policy; it sends money only to a request's payee and, on `close`, to `refund_to` | `cargo test --locked --test verify no_admin_surface -- --nocapture` | `exported methods: ["approve", "attempts", "close", "contract_source_metadata", "get_request", "mandate", "new", "on_transfer", "reject", "request_spend"]; the only outgoing actions are a transfer to the request's payee and, on close, to refund_to` |
| The committed `res/preavis.wasm` was built from a named commit, and the contract's inputs are unchanged since | `cargo test --locked --test verify prebuilt_is_from_this_source -- --nocapture` | `res/preavis.wasm (sha256 900a452b59841520e10327c6409b5d3e81265b6c57edd5b1f25c5109fcab6f82) was built from commit 8aee2d5742db899a08274df707cc94464d27eff2; the contract's inputs are unchanged since. Byte identity: npm run verify:reproducible (Docker)` |

`no_admin_surface` reads the export section of the wasm. `contract_source_metadata` is the read-only
NEP-330 view; it returns the build environment, command and source commit. The source check also
rejects the promise actions `deploy_contract`, `add_full_access_key`, `add_access_key`, `delete_key`,
`delete_account`, `create_account`, `function_call` and `.stake(`, and allows exactly two transfers.

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
| The deployed testnet code is `res/preavis.wasm` | `OK    deployed code hash equals sha256 of res/preavis.wasm …` (`chain AhGnuZq8k6R9X1Nf4YcYuk8WU6ZfGnqmp46GvnPJobgd`), naming source commit `8aee2d5742db899a08274df707cc94464d27eff2` |
| The live agent's working key is function-call only; its only other key is the owner's offline full-access key | `OK    the agent's working key is function-call only …` |
| Each of the 21 recorded transactions matches: signer, receiver, method and arguments, deposit, success or failure, every event (with its reason), every failure, every transfer the mandate sent | one `OK` line per transaction with its decoded call, events, transfers and nearblocks link |
| The end state matches: `mandate()`, all 13 attempts, `committed`, `held` and `payee_committed` equal the sums of the stored attempts, no cap exceeded, the vendor / hosting / attacker / treasury balances | `OK` lines under `End state, re-read from the chain` |
| The mandate is locked | `OK    locked: no key can sign for mandate.preavis-1794029520.testnet` |
| All of it | last line `OK: 21 recorded transactions match the chain; end state re-read; locked: no key can sign for it; code hash AhGnuZq8k6R9X1Nf4YcYuk8WU6ZfGnqmp46GvnPJobgd = reproducible build of 8aee2d5742db.`, exit 0 |

Any mismatch prints `FAIL` with a `MISMATCH <field>: chain … | recorded …` line and exits 1; so does a
mandate that still has a key (`FAIL  NOT LOCKED: …`).

**Negative control:** `npm run observe-testnet:negative` writes four altered copies of the record (a
claim pointing at another transaction; a changed denial reason; a balance claiming the attacker was
paid; a mandate that still has a key) and requires the observer to exit non-zero on each, for the
expected reason. Expected last line: `All 4 altered records were rejected (non-zero exit).`

The run behind `testnet/receipts.json` executed claims 1–22, 24 and 25 on testnet on 2026-11-07 at
05:34 UTC (14:34 KST): `24 claims held, 0 broken` ([testnet/run.txt](testnet/run.txt)). Claims 4 and 5
are protocol refusals with no transaction to link: the RPC rejects them before inclusion, as the log
shows. The lock is claim 24: transaction `9cgVTZQRz1xTiTihLZY5cU1VyqBdjxxZYypbHJiieEgg` deleted the
mandate's last key.

## E. `npm run verify:reproducible` (needs Docker)

| Claim | Expected output |
|---|---|
| Rebuilding the commit named in `res/preavis.wasm` in the pinned image gives the same bytes | `OK    rebuilt from commit 8aee2d5742db899a08274df707cc94464d27eff2: byte-identical to res/preavis.wasm` |
| … equal to the recorded hash | `OK    equals res/preavis.wasm.sha256` (`900a452b59841520e10327c6409b5d3e81265b6c57edd5b1f25c5109fcab6f82`) |
| … equal to the code deployed on testnet | `OK    equals the code deployed on testnet at mandate.preavis-1794029520.testnet` (`AhGnuZq8k6R9X1Nf4YcYuk8WU6ZfGnqmp46GvnPJobgd`) |

The image is `rust:1.99.1-slim-bookworm` pinned by its `linux/amd64` digest
(`sha256:392f221964669402bf4ae133d2a46c3ae270f64734133f4b2703d40019235c1a`, see
`scripts/build-reproducible.sh`). The build reads only `git archive` of that commit, so local changes
and paths do not matter. It embeds NEP-330 build info (environment, command, source commit).
Checked on x86_64. An arm64 host runs the same amd64 image under emulation (Docker Desktop and
OrbStack include it); that path has not been checked yet.

## F. `npm run agent` (an LLM agent in the loop, local sandbox)

| Claim | Expected output |
|---|---|
| The run says whether a model is live or a recording | first line `MODEL: REPLAYING A RECORDING, not a live model (agent/transcript.json, recorded on 2026-11-07).` and the recorded model, or `MODEL: live, …` |
| The in-policy invoice settles | `contract SETTLED; transferred 0.4 NEAR -> vendor.test.near` |
| The large invoice waits for the owner, who approves it bound to the invoice's hash | `contract AWAITING_APPROVAL`, then `OWNER approves inv-2210 … SETTLED; transferred 2.5 NEAR -> hosting.test.near` |
| The injected payment is refused by the contract, whatever the model does | `WHATEVER THE MODEL DOES: the agent key sends exactly what the injection asks (3.5 NEAR -> attacker.test.near): DENIED UNLISTED` |
| Measured result | `MEASURED from all 16 receipts of this run: the mandate paid hosting.test.near 2.5 NEAR, vendor.test.near 0.4 NEAR; attacker.test.near received 0.` |

In the recording, the model declined the injected invoice itself (`NOTHING SENT (the model
declined)`). With `PREAVIS_MODEL_CMD="node agent/hijacked-model.mjs"`, a stand-in that obeys every
invoice, the agent does send the attacker's payment and the contract answers `DENIED UNLISTED`.

## G. `npm run execute-testnet` (optional, uses the testnet faucet)

| Claim | Command | Expected output |
|---|---|---|
| Claims 1–22, 24 and 25 hold on a fresh testnet deployment you create, ending locked | `npm run execute-testnet` | 24 `HELD` lines with nearblocks links, then `Preavis testnet: 24 claims held, 0 broken.` |

Prerequisites and 429 handling: [README §6](README.md#6-run-it-on-testnet-yourself-optional).
