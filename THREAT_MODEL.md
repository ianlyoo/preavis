# Threat model

Préavis limits what the holder of an AI agent's key can make the agent pay. This file lists who holds
which authority, which layer enforces each limit, and, just as important, what Préavis does **not**
stop. Version: v0.8, testnet, **not locked yet**.

## What is protected

The mandate account's balance (the agent's budget) and the policy fixed at initialization:

- payee allowlist with a lifetime cap per payee;
- a total cap covering held, in-flight and settled requests;
- the amount the agent may commit on its own (`approval_threshold`); beyond it, every request waits for the owner;
- a policy version that every request must name.

## Who holds what

| Holder | Key / role | Can | Cannot |
|---|---|---|---|
| **You** (the agent's working key) | function-call key on the agent account: receiver = mandate, method = `request_spend`, no deposit, 0.25 NEAR gas allowance | ask for payments; get them only if the policy allows | pay an unlisted account; exceed a cap; reuse or alter a request ID; claim another policy version; attach a deposit; call any other method; get paid twice by a replay or by two requests at once |
| **Owner** | `owner` account named at init | `approve` a held request with its exact arguments; `reject` it | request spends (`AGENT_ONLY`); pay anyone outside the allowlist; exceed caps; edit the policy; call the callback |
| **Owner, offline** | the agent account's full-access key | fire the agent (delete its working key); add a new working key | anything the mandate's checks refuse: signing as the agent is still bounded by the policy |
| **Deployer, before the lock** | the mandate account's own full-access key | **anything**: redeploy code, move the whole balance, add keys | (nothing; see "Not stopped") |
| **Anyone else** | any NEAR account | read every view (`mandate`, `attempts`, `get_request`) | call `request_spend` (`AGENT_ONLY`), `approve` or `reject` (`OWNER_ONLY`), `on_transfer` (private) or `new` (private, and only once) |
| **Payees** | their own accounts | receive transfers | nothing more: a NEAR transfer runs no payee code |

The sub-account names (`agent.<owner>`, `mandate.<owner>`) are only labels. On NEAR a parent account has
no control over a sub-account's keys.

## Which layer enforces what

| Layer | Enforces | Evidence |
|---|---|---|
| **Protocol** (NEAR access keys) | the working key calls only `request_spend` on the mandate and cannot attach a deposit | the RPC rejects the transaction before any contract runs: `MethodNameMismatch`, `DepositWithFunctionCall` (VERIFY 4, 5) |
| **Contract** (`src/lib.rs`) | caller checks on every mutating method; allowlist, caps, the agent's own limit, policy version, single-use request IDs bound to actor, payee, amount, policy version and ID | normal-returning `DENIED <reason>` records with NEP-297 events, or a panic for wrong callers and wrong approvals (VERIFY 2, 6–9, 11, 12, 14, 15, 17–20, 23) |
| **Settlement** | charge before the transfer leaves; a private callback reads the transfer result and settles, or releases the charge exactly once | `SETTLED` with an exact balance change; `FAILED` with `releases=1`; replayed callbacks change nothing (VERIFY 10, 13, 16, 22, 24) |

Request binding: a request ID can be used once. Resubmitting it is `REPLAY` if every bound field
matches and `ALTERED` otherwise. The owner's `approve` must repeat payee, amount and policy version
exactly (`BINDING_MISMATCH`), so an approval cannot pay something other than what the agent asked for.

## Failure semantics

| Event | Result | Why |
|---|---|---|
| Transfer fails (the payee account does not exist) | `FAILED`, charge released once | NEAR transfers fail only when the receiver does not exist; the callback sees `PromiseError::Failed` |
| The callback itself fails (out of gas, a bug) | the record stays `PENDING`, the charge stays frozen; the transfer may already have happened | not knowing the outcome, the contract neither refunds (which could double-spend the budget) nor resends (which could double-pay). The frozen amount is visible in `attempts` and in `mandate().committed`. Shown with a test-only build (VERIFY 24) |
| Two requests in the same block | the second sees the first's charge | the charge is taken in the same receipt as the check, before the transfer (VERIFY 23) |
| Owner rejects twice | the second call panics `NOT_AWAITING_APPROVAL` | release happens only from `AWAITING_APPROVAL` |

## Not stopped

1. **A wrong-purpose payment to an allowlisted payee.** Within its caps, the agent can pay an
   allowlisted vendor for something you did not want. Préavis bounds how much can go where; it does not
   judge why. `attack-local` input #15 shows this payment going through.
2. **A compromised owner key.** Whoever holds it can approve every held request, which means paying
   allowlisted payees up to their caps and the total. With the agent's offline full-access key they can
   also sign as the agent, still inside the policy. They cannot pay an unlisted account, raise a cap or
   change the policy.
3. **The deployer, before the lock.** Until the mandate account's full-access key is deleted, its
   holder can redeploy any code and take the whole balance. **In v0.8 that key still exists**
   (`observe-testnet` prints `NOT LOCKED yet`). The lock is the v0.9 step. After it, `list-keys` returns
   nothing, and `no_admin_surface` shows the code has no path to deploy code or add keys. A locked
   contract can still act through its own code, so both checks are needed. Locking also strands any
   leftover balance, deliberately: a new policy means a new mandate.
4. **Storage griefing by your key.** Each recorded attempt, denials included, adds roughly 250 bytes of
   state. On NEAR that costs about 0.0025 NEAR of the mandate's balance as storage stake (measured on the
   testnet mandate: about 3.1 KB for 12 attempts). One agent transaction used about 0.00035 NEAR of the
   0.25 NEAR gas allowance, so the key could make about 700 calls and lock up to about 1.75 NEAR. On our
   testnet mandate that is more than its free balance (about 1.03 NEAR after the 3.96 NEAR its code and state
   already lock). Once the free balance runs out, the mandate can no longer record attempts and every request
   fails, so this is a denial of service, not a payout. Mitigations: the owner fires the agent; fund a
   storage reserve; a later version could charge the agent for storage or cap stored denials.
5. **Trusting the RPC.** `observe-testnet` believes the RPC endpoint's answers. To remove that trust,
   point `PREAVIS_TESTNET_RPC` at a node you run.
6. **A bad policy.** If the owner allowlists an attacker's account, the attacker is a payee. Read the
   policy with the `mandate` view before trusting a mandate.
7. **Implicit accounts.** A transfer to a 64-hex implicit account that does not exist yet creates it,
   so the transfer succeeds. Only named accounts that do not exist make a transfer fail.

## Why on-chain

The checks run in the contract that holds the money, not on the agent operator's server. Anyone,
including a payee or a judge, can read the policy and every attempt from the chain, and nobody can skip
the checks, the operator included, once the mandate is locked.
