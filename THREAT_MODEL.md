# Threat model

Préavis limits what the holder of an AI agent's key can make the agent pay. This file lists who holds
which authority, which layer enforces each limit, and, just as important, what Préavis does **not**
stop. Version: v1.0.1, testnet; the recorded run's mandate is **locked** (no access key). The challenge mandate is also locked, was never published, and is closed ([docs/challenge.md](docs/challenge.md)).

## What is protected

The mandate account's balance (the agent's budget) and the policy fixed at initialization:

- payee allowlist with a lifetime cap per payee;
- a total cap covering in-flight and settled payments; a held request reserves nothing, and its
  approval is checked against every cap at that time;
- per period of `period_blocks` blocks: the amount the agent may commit on its own
  (`approval_threshold`; beyond it, every request waits for the owner), the requests that pass the
  checks (`max_payments`; past it, `DENIED EXHAUSTED` until `resets_at`) and the denials stored
  (`max_denials`; past it, only counted). The storage for every slot of every period until
  `expires_at` is reserved at init;
- a policy version that every request must name;
- `expires_at`: from that block on nothing is requested or approved (`EXPIRED`), and anyone may
  `close` the mandate;
- `refund_to`, the only account `close` can pay.

## Who holds what

| Holder | Key / role | Can | Cannot |
|---|---|---|---|
| **You** (signing as the agent: in `npm run verify` its working key is generated on your machine and signs every agent call) | function-call key on the agent account: receiver = mandate, method = `request_spend`, no deposit, a gas allowance (0.25 NEAR on the recorded run) | ask for payments; get them only if the policy allows | pay an unlisted account; exceed a cap; reuse or alter a request ID; claim another policy version; attach a deposit; call any other method; get paid twice by a replay or by two requests at once |
| **Owner** | `owner` account named at init | `approve` a held request with its exact arguments, invoice included, if it still fits the caps; `reject` it; `close` the mandate, which sends everything but the storage stake to `refund_to` | request spends (`AGENT_ONLY`); pay anyone outside the allowlist or other than `refund_to`; exceed caps; edit the policy; call the callback |
| **Owner, offline** | the agent account's full-access key | fire the agent (delete its working key); add a new working key | anything the mandate's checks refuse: signing as the agent is still bounded by the policy |
| **Deployer, before the lock** | the mandate account's own full-access key, **deleted on testnet** (transaction `2zs58UkW3s1DqVde7zm9wMWAs5RT8MJ9sB9AQkxcd5oF`; the challenge: `2eSEVB5QgXvXzvTspuxtfCPyFrZ2XEnJsLpedVww6XPQ`) | before deletion: **anything**: redeploy code, move the whole balance, add keys | after deletion: nothing; no key exists (see "Not stopped" 3) |
| **Anyone else** | any NEAR account | read every view (`mandate`, `attempts`, `get_request`, `contract_source_metadata`); after `expires_at`, `close` the mandate, which pays only `refund_to` | call `request_spend` (`AGENT_ONLY`), `approve`, `reject` or, before `expires_at`, `close` (`OWNER_ONLY`), `on_transfer` (private) or `new` (private, and only once) |
| **Payees** | their own accounts | receive transfers | nothing more: a NEAR transfer runs no payee code |

The sub-account names (`agent.<owner>`, `mandate.<owner>`) are only labels. On NEAR a parent account has
no control over a sub-account's keys.

## Which layer enforces what

| Layer | Enforces | Evidence |
|---|---|---|
| **Protocol** (NEAR access keys) | the working key calls only `request_spend` on the mandate and cannot attach a deposit | the RPC rejects the transaction before any contract runs: `MethodNameMismatch`, `DepositWithFunctionCall` (VERIFY 4, 5) |
| **Contract** (`src/lib.rs`) | caller checks on every mutating method; allowlist, caps, the agent's own limit, policy version, payment and denial limits, single-use request IDs bound to actor, payee, amount, policy version and invoice | normal-returning `DENIED <reason>` records with NEP-297 events, or a panic for wrong callers and wrong approvals (VERIFY 2, 6–9, 11, 12, 14–16, 18–21, 26, 27, 29, 30, 32–34) |
| **Settlement** | charge before the transfer leaves; a private callback reads the transfer result and settles, or releases the charge exactly once | `SETTLED` with an exact balance change; `FAILED` with `releases=1`; replayed callbacks change nothing (VERIFY 10, 13, 17, 23, 25, 28, 31) |
| **No key** (the lock) | nobody can sign for the mandate account, so only its code moves its balance | `view_access_key_list` returns nothing; `observe-testnet` fails otherwise (VERIFY 24, section D) |
| **Code** (what the wasm imports) | the code cannot deploy code, add or delete keys, call another contract, stake or delete an account: it does not import those host functions | `no_admin_surface` on the committed wasm; `observe-testnet` on the deployed code (`view_code`) against the same allowlist (VERIFY B, D) |

Request binding: a request ID that reserved budget can be used once, forever: its record is kept for
the life of the mandate. Resubmitting it is `REPLAY` if every bound field matches and `ALTERED`
otherwise. A denied ID reserved nothing and may be used again. Every request names an invoice
reference (up to 64 characters, e.g. the invoice's sha256). The owner's `approve` must repeat payee,
amount, policy version and invoice exactly (`BINDING_MISMATCH`), so an approval cannot pay something
other than what the agent asked for, or for another invoice.

## Failure semantics

| Event | Result | Why |
|---|---|---|
| Transfer fails (the payee account does not exist) | `FAILED`, charge released once | NEAR transfers fail only when the receiver does not exist; the callback sees `PromiseError::Failed` |
| The callback itself fails (out of gas, a bug) | the record stays `PENDING`, the charge stays frozen; the transfer may already have happened | not knowing the outcome, the contract neither refunds (which could double-spend the budget) nor resends (which could double-pay). The frozen amount is visible in `attempts` and in `mandate().committed`. Shown with a test-only build (VERIFY 28) |
| Two requests in the same block | the second sees the first's charge | the charge is taken in the same receipt as the check, before the transfer (VERIFY 27) |
| Owner rejects twice | the second call panics `NOT_AWAITING_APPROVAL` | release happens only from `AWAITING_APPROVAL` |
| Owner closes with requests held | everything but the storage stake goes to `refund_to`; held requests can no longer be approved (`CLOSED`), only rejected; new requests panic `CLOSED` | a hold reserves nothing, so there is nothing to keep back (VERIFY 34) |
| A held request is approved after other payments | approved only if it still fits the payee cap, the total cap and the period's slots; otherwise the approval panics with the reason (e.g. `PayeeCap`) and the request stays held | holds reserve nothing (VERIFY 32) |
| The period ends | the period's budget, slots and stored denials start fresh at `resets_at`; a failed transfer gives its share back to the period it was charged in | VERIFY 30, 31 |
| `expires_at` passes | `request_spend` and `approve` panic `EXPIRED`; anyone may `close`, which pays only `refund_to` | VERIFY 33 |

## Not stopped

1. **A wrong-purpose payment to an allowlisted payee.** Within its caps, the agent can pay an
   allowlisted vendor for something you did not want. Préavis bounds how much can go where; it does not
   judge why. `attack-local` input #15 shows this payment going through.
2. **A compromised owner key.** Whoever holds it can approve every held request, which means paying
   allowlisted payees up to their caps and the total. With the agent's offline full-access key they can
   also sign as the agent, still inside the policy. They cannot pay an unlisted account, raise a cap or
   change the policy.
3. **The deployer, before the lock.** Until the mandate account's last access key is deleted, its
   holder can redeploy any code and take the whole balance. On the v1.0 testnet mandates that key is
   **deleted** (VERIFY 24, transaction `2zs58UkW3s1DqVde7zm9wMWAs5RT8MJ9sB9AQkxcd5oF`), and
   `observe-testnet` fails if any key exists. The code's imports show it has no path to deploy code or
   add keys: checked on the committed wasm (`no_admin_surface`) and on the deployed bytes
   (`observe-testnet`). A locked contract can still act through its own code, so both checks are
   needed. What stays possible: `close` sends everything but the storage stake to `refund_to`, fixed
   at init.
   A new policy means a new mandate. For your own mandate, the lock is up to you: until you delete
   the key, this risk is yours.
4. **Exhausting today's window.** Your key can use up the period's `max_payments` with small
   in-policy payments to allowlisted payees or with requests held for the owner, after which new
   requests are `DENIED EXHAUSTED` until `resets_at` (shown by `mandate()` and `observe-testnet`).
   That is a denial of service bounded to one period, not a payout: every such payment is still
   inside the caps, and the next period starts fresh (VERIFY 29, 30). Denials past `max_denials` are
   counted but not stored, so they cannot grow storage. The storage for every slot of every period is
   reserved at init, so the mandate can always settle or reject what it holds.
   The holder can do this again at the start of every period, so if many players shared one key,
   one of them could block the others every day. That is why the challenge mandate's shared key
   was never published ([docs/challenge.md](docs/challenge.md)); a public challenge would need
   per-player keys or a rate-limited relayer.
5. **Spending the key's gas allowance.** Every call costs the agent account gas, up to the key's
   allowance. Calls with very large arguments spend it fastest: an independent review estimated
   that an 8 NEAR allowance lasts about 230 such calls ([docs/challenge.md](docs/challenge.md)). That costs gas, not the
   mandate's money; when the allowance is gone the key stops working until the owner adds a new one
   or rotates the mandate.
6. **A lost owner key.** Nobody can approve held requests or close early any more. Nothing is
   paid wrongly and nothing is stuck for good: at `expires_at` anyone may call `close`, which sends
   the balance (minus storage) to `refund_to`. Until then the agent keeps working within its own
   per-period budget. Pick `refund_to` and `expires_at` with this in mind.
7. **Trusting the RPC.** `observe-testnet` believes the RPC endpoint's answers. To remove that trust,
   point `PREAVIS_TESTNET_RPC` at a node you run.
8. **A bad policy.** If the owner allowlists an attacker's account, the attacker is a payee. Read the
   policy with the `mandate` view before trusting a mandate.
9. **Implicit accounts.** A transfer to a 64-hex implicit account that does not exist yet creates it,
   so the transfer succeeds. Only named accounts that do not exist make a transfer fail.
10. **The agent's choices inside the policy.** A prompt-injected model can propose anything. The
   contract refuses what is outside the policy (`npm run agent`: the injected payment and the changed
   payment account are `DENIED UNLISTED`), but a model fooled into paying an allowlisted vendor's
   invoice for something nobody ordered, within its own limit, is paid: `npm run agent` shows it
   (inv-2215, 0.3 NEAR of consulting to the allowlisted host). Above the agent's limit it is held,
   and `npm run owner -- show` prints the invoice bytes behind the hash before the owner signs.
11. **The callback's own failure.** If `on_transfer` itself fails, the charge stays frozen and visible
   (VERIFY 28). There is no method to unfreeze it; that amount is lost to the budget, not paid twice.

## Why on-chain

The checks run in the contract that holds the money, not on the agent operator's server. Anyone,
including a payee or a judge, can read the policy and every attempt from the chain, and nobody can skip
the checks, the operator included, once the mandate is locked.
