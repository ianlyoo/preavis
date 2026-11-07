# Journey

Dated decisions behind v0.8. Times are KST; times marked ≈ are reconstructed, the others are commit or log times. The earlier spike (contract sketch, sandbox tests, the
testnet toolchain) is not in this repository's history; v0.8 was rebuilt from it in a fresh repo.

## 2026-11-07

- **≈12:40, start.** Scope fixed by the hackathon plan, "Option 1": a Rust mandate contract the agent
  must ask before paying, a `npm run verify` that a stranger can run, attack and observe commands, one
  recorded testnet run, and honest documents. Judge-facing line: *"You hold our agent's key. Try to make
  it pay you."*
- **≈12:45, name check.** Searched for existing projects named KeyDare in the NEAR and agent-payments
  space; nothing collided.
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
- **≈13:00, owner delta #1180: rename to Préavis.** Identifiers, crate and commands became `preavis`;
  repository `ianlyoo/preavis`. Old commits keep the KeyDare name.
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

## Not in v0.8 (planned for 2026-11-11)

The lock (deleting the mandate account's full-access key), the video, the post and the form.
