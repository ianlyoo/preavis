# Journey

Dated decisions behind Préavis. Times are KST; times marked ≈ are reconstructed, the others are
commit or log times. The earlier spike (contract sketch, sandbox tests, the testnet toolchain) is not
in this repository's history; v0.8 was rebuilt from it in a fresh repo.

## 2026-11-07

- **≈12:40, start.** Scope: a Rust mandate contract the agent must ask before paying, a
  `npm run verify` that a stranger can run, attack and observe commands, one recorded testnet run, and
  honest documents. The pitch in one line: *"You hold our agent's key. Try to make it pay you."*
- **≈12:45, working name.** The project started as KeyDare; a search of the NEAR and agent-payments
  space found no collision.
- **≈12:50, one contract file, no admin surface.** No upgrade, add-key or policy-edit method. A policy
  change means a new mandate. `no_admin_surface` enforces the export list and the absence of deploy and
  key actions in the source.
- **≈12:50, denials are records, not panics.** A refused request returns normally, is stored and emits an
  NEP-297 event, so a judge can see every attempt on chain. Wrong callers still panic: they are not the
  agent, so they get no record.
- **≈12:52, charge before transfer.** The budget is charged in the same receipt as the check; a private
  callback settles or releases exactly once. If the callback itself fails, the charge stays frozen and
  visible. Refunding or resending without knowing the outcome could double-pay.
- **≈12:53, reproducible wasm.** Cargo `trim-paths` is unstable on Rust 1.99.1, and the wasm embedded
  absolute registry paths. `scripts/build-wasm.sh` remaps `$PWD` and `$CARGO_HOME` (order matters: the
  last matching prefix wins). The hash now matches across checkouts, cargo homes and a clean container.
- **12:57, attacks are data.** `attack/inputs.json` can be edited by a judge without touching Rust; each
  attack is classified as stopped by protocol, contract, denied, held or PAID, and the money invariants
  are checked after all of them.
- **≈13:00, seeded keys found.** `near-workspaces` 0.24 gives sub-accounts created without `.keys()` a key
  from a public seed (`"subaccount.seed"`), and `dev_create_account` derives keys from the account id.
  Anyone could sign for such accounts on testnet. Every account here now gets `SecretKey::from_random`.
- **≈13:00, renamed to Préavis.** Identifiers, crate and commands became `preavis`; repository
  `ianlyoo/preavis`. Old commits keep the KeyDare name.
- **13:02, public testnet RPC rate limit.** `archival-rpc.testnet.near.org` returned 429 after three
  claims. Execute switched to `https://test.rpc.fastnear.com`; observe tries FastNEAR's archival and
  regular endpoints first and backs off on 429. `PREAVIS_TESTNET_RPC` overrides both.
- **13:14, first recorded testnet run** with receipts and a key-free `observe-testnet`.
- **≈13:16, the agent's full-access key stays with the owner.** v0.8 first deleted it, which left no way
  to fire the agent: a function-call key cannot delete itself. Now the owner keeps it offline, and
  claim 25 shows firing the agent is one key deletion. The working key handed to "you" stays
  function-call only.
- **13:18, final recorded run** on `mandate.preavis-1794025012.testnet`: 21 of 21 claims held.
- **≈13:25, documents.** README with expected output pasted, VERIFY.md generated from real output,
  THREAT_MODEL.md with what is not stopped.

## 2026-11-07, v0.9 fix round

Two independent outside reviews of v0.8 found real gaps. The fixes, in order:

- **≈13:40, verify failed on arm64.** The committed wasm did not match an arm64 build of the same
  source. Cause: Cargo hashes `rustc -vV`, which names the host triple, into crate metadata, and that
  changes symbol names and code. Decision: build the committed wasm in one pinned `linux/amd64` Rust
  image (by digest), with NEP-330 build info naming the source commit, and split the checks.
  `npm run verify` deploys the committed bytes on any host and checks the inputs are unchanged since
  that commit; `npm run verify:reproducible` (Docker) proves byte identity against the committed
  file and the deployed code hash. `strip = true` cut the wasm from 393 KB to 232 KB.
- **≈13:45, storage growth was unbounded.** v0.8 stored every attempt, so a hostile key could fill
  the mandate's storage. Decision: a policy limit on payments and on stored denials; denials past the
  limit are counted and emitted but not stored. `new` requires the balance to cover the total cap
  plus storage for every slot, so the mandate can always settle or reject what it holds. Request IDs
  that reserved budget are kept forever, so replay protection never expires. Measured worst record:
  399 bytes against 500 reserved.
- **≈13:50, one treasury feature, no discretion.** `close()` sends the free remainder only to
  `refund_to`, fixed at init; held requests stay payable. Every request names an invoice reference
  that the owner's approval must repeat. Per-period caps and NEP-141 were left out.
- **14:02, attack-local measured nothing.** It printed "you received 0" from a balance difference.
  It now reads back every receipt of the attacks and sums the transfers to the attacker, with a
  positive control on the payees.
- **14:31, observe-testnet asserted too little.** It printed outcomes it did not compare. The record
  now holds, per claim, signer, receiver, actions, status, events, failures and implied transfers;
  the observer compares them all, re-reads the end state, and fails unless the mandate has no key.
  A negative control feeds it four altered records and requires four failures.
- **14:31, an agent in the loop.** `npm run agent`: an LLM reads three invoices, one with a prompt
  injection, and calls `request_spend` with the function-call key. Model access is a command
  (`claude -p`, a local model, any OpenAI-compatible endpoint). The default replays a recording made
  with a free local model (Qwen2.5-7B-Instruct on CPU through llama.cpp) and says so. That model
  declined the injection; a model-independent step then sends the injected payment anyway, and the
  contract refuses it (`UNLISTED`).
- **14:34, the lock.** A fresh testnet mandate ran the 24 on-chain claims; claim 24 deleted its last
  access key (`9cgVTZQRz1xTiTihLZY5cU1VyqBdjxxZYypbHJiieEgg`). The agent account's full-access key
  stays offline with the owner, as the switch for firing the agent. No testnet secret key is
  published.

## 2026-11-07, v1.0: the second judge round's fixes

- **Windows, not lifetimes.** The agent's own budget, its request slots and the stored denials are
  now per period of `period_blocks`. A flood exhausts one window (`DENIED EXHAUSTED`, visible as
  `exhausted` and `resets_at` in `mandate()`), and the next one starts fresh. Held requests reserve
  nothing; an approval is checked against the caps when it happens. `expires_at` lets anyone close
  the mandate to `refund_to` once it passes, which also answers "the owner lost their key".
- **No admin path, from the bytes.** Transfers and the callback go through `env::promise_*`
  directly. The wasm now imports 22 host functions, all on an allowlist, and no deploy, key or
  function-call action. `no_admin_surface` checks the committed wasm; `observe-testnet` reads the
  deployed code back with `view_code` and checks the same list. The v0.9 mandate fails that check.
- **A real owner step.** `npm run agent` and `npm run owner` are separate programs over a local node
  that keeps its state on disk. The owner sees the invoice bytes, hash, payee and amount before
  signing. The inbox gained a split payment, a changed payment account and an allowlisted
  wrong-purpose invoice that is paid; the run prints proposed actions next to measured transfers.
  The model is pinned (`scripts/local-model.sh`) and the recording was made again with it.
- **The wording.** You control the agent's signing in the sandbox; the key printed is its public
  half.
- **arm64.** `CARGO_BUILD_JOBS` goes into the build container (1 under emulation); the byte
  comparison stays the same.
- **A challenge mandate, prepared but not published.** `c1.preavis-challenge-1794034196.testnet`,
  locked, with per-day windows, a gas allowance sized for the judging window, a timed rotation
  (deploy 26 s, retire 5 s) and an observer counter "attempts so far / paid to attackers". The key
  stayed in the ignored `.credentials/` ([challenge.md](challenge.md)).
- **15:49, the lock.** The v1.0 testnet run (24 claims) ended by deleting the mandate's last key
  (`2zs58UkW3s1DqVde7zm9wMWAs5RT8MJ9sB9AQkxcd5oF`).

## 2026-11-07, v1.0.1: the challenge retired

- **An independent review of the challenge design** found that one holder of a shared key can take
  every window's request slots at no budget cost, and that no code change to a locked mandate with
  one shared key prevents it. It found no way to be paid outside the allowlist.
- **17:07, retired.** The key was not published. `npm run challenge-retire` deleted it and closed
  the mandate (6.304011 NEAR back to `refund_to`). `observe-testnet` now checks the retired state.
  Links: [challenge.md](challenge.md).
- **The wording.** The hook now says where you get the agent's key: in a local NEAR sandbox.

## Not done yet

The video, the post and the submission form.
