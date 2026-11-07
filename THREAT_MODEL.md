# Threat model

Préavis limits what the holder of an AI agent's key can make the agent pay. This file lists who holds
which authority, which layer enforces each limit, and, just as important, what Préavis does **not**
stop. Version: v0.9, testnet; the live mandate is **locked** (no access key).

## What is protected

The mandate account's balance (the agent's budget) and the policy fixed at initialization:

- payee allowlist with a lifetime cap per payee;
- a total cap covering held, in-flight and settled requests;
- the amount the agent may commit on its own (`approval_threshold`); beyond it, every request waits for the owner;
- a policy version that every request must name;
- a limit on payments (`max_payments`) and on stored denials (`max_denials`), with the storage for
  all of them reserved at init;
- `refund_to`, the only account `close` can pay.

## Who holds what

| Holder | Key / role | Can | Cannot |
|---|---|---|---|
| **You** (the agent's working key) | function-call key on the agent account: receiver = mandate, method = `request_spend`, no deposit, 0.25 NEAR gas allowance | ask for payments; get them only if the policy allows | pay an unlisted account; exceed a cap; reuse or alter a request ID; claim another policy version; attach a deposit; call any other method; get paid twice by a replay or by two requests at once |
| **Owner** | `owner` account named at init | `approve` a held request with its exact arguments, invoice included; `reject` it; `close` the mandate, which sends the free remainder to `refund_to` | request spends (`AGENT_ONLY`); pay anyone outside the allowlist or other than `refund_to`; exceed caps; edit the policy; call the callback |
| **Owner, offline** | the agent account's full-access key | fire the agent (delete its working key); add a new working key | anything the mandate's checks refuse: signing as the agent is still bounded by the policy |
| **Deployer, before the lock** | the mandate account's own full-access key, **deleted on testnet** (transaction `9cgVTZQRz1xTiTihLZY5cU1VyqBdjxxZYypbHJiieEgg`) | before deletion: **anything**: redeploy code, move the whole balance, add keys | after deletion: nothing; no key exists (see "Not stopped" 3) |
| **Anyone else** | any NEAR account | read every view (`mandate`, `attempts`, `get_request`, `contract_source_metadata`) | call `request_spend` (`AGENT_ONLY`), `approve`, `reject` or `close` (`OWNER_ONLY`), `on_transfer` (private) or `new` (private, and only once) |
| **Payees** | their own accounts | receive transfers | nothing more: a NEAR transfer runs no payee code |

The sub-account names (`agent.<owner>`, `mandate.<owner>`) are only labels. On NEAR a parent account has
no control over a sub-account's keys.

## Which layer enforces what

| Layer | Enforces | Evidence |
|---|---|---|
| **Protocol** (NEAR access keys) | the working key calls only `request_spend` on the mandate and cannot attach a deposit | the RPC rejects the transaction before any contract runs: `MethodNameMismatch`, `DepositWithFunctionCall` (VERIFY 4, 5) |
| **Contract** (`src/lib.rs`) | caller checks on every mutating method; allowlist, caps, the agent's own limit, policy version, payment and denial limits, single-use request IDs bound to actor, payee, amount, policy version and invoice | normal-returning `DENIED <reason>` records with NEP-297 events, or a panic for wrong callers and wrong approvals (VERIFY 2, 6–9, 11, 12, 14–16, 18–21, 26, 28, 29) |
| **Settlement** | charge before the transfer leaves; a private callback reads the transfer result and settles, or releases the charge exactly once | `SETTLED` with an exact balance change; `FAILED` with `releases=1`; replayed callbacks change nothing (VERIFY 10, 13, 17, 23, 25, 27) |
| **No key** (the lock) | nobody can sign for the mandate account, so only its code moves its balance | `view_access_key_list` returns nothing; `observe-testnet` fails otherwise (VERIFY 24, section D) |

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
| The callback itself fails (out of gas, a bug) | the record stays `PENDING`, the charge stays frozen; the transfer may already have happened | not knowing the outcome, the contract neither refunds (which could double-spend the budget) nor resends (which could double-pay). The frozen amount is visible in `attempts` and in `mandate().committed`. Shown with a test-only build (VERIFY 27) |
| Two requests in the same block | the second sees the first's charge | the charge is taken in the same receipt as the check, before the transfer (VERIFY 26) |
| Owner rejects twice | the second call panics `NOT_AWAITING_APPROVAL` | release happens only from `AWAITING_APPROVAL` |
| Owner closes with requests held | the held amount and the storage stake stay; held requests can still be approved and paid; new requests panic `CLOSED` | `close` keeps `held` back (VERIFY 29) |

## Not stopped

1. **A wrong-purpose payment to an allowlisted payee.** Within its caps, the agent can pay an
   allowlisted vendor for something you did not want. Préavis bounds how much can go where; it does not
   judge why. `attack-local` input #15 shows this payment going through.
2. **A compromised owner key.** Whoever holds it can approve every held request, which means paying
   allowlisted payees up to their caps and the total. With the agent's offline full-access key they can
   also sign as the agent, still inside the policy. They cannot pay an unlisted account, raise a cap or
   change the policy.
3. **The deployer, before the lock.** Until the mandate account's last access key is deleted, its
   holder can redeploy any code and take the whole balance. On the v0.9 testnet mandate that key is
   **deleted** (VERIFY 24, transaction `9cgVTZQRz1xTiTihLZY5cU1VyqBdjxxZYypbHJiieEgg`), and
   `observe-testnet` fails if any key exists. `no_admin_surface` shows the code has no path to deploy
   code or add keys. A locked contract can still act through its own code, so both checks are needed.
   What stays possible: the owner's `close` sends the free remainder to `refund_to`, fixed at init.
   A new policy means a new mandate. For your own mandate, the lock is up to you: until you delete
   the key, this risk is yours.
4. **Exhausting the payment slots.** Your key can use up the mandate's `max_payments` with small
   in-policy payments to allowlisted payees, after which new requests are `DENIED PAYMENT_LIMIT`.
   That is a denial of service bounded by the policy, not a payout: every such payment is still
   inside the caps. Denials past `max_denials` are counted but not stored, so they cannot grow storage
   (VERIFY 28: storage unchanged after 5 more denials). The storage for every slot is reserved at init,
   so the mandate can always settle or reject what it holds. Each call also costs your key gas from
   its 0.25 NEAR allowance. Mitigation: the owner fires the agent, and a new mandate gets a new policy.
5. **Trusting the RPC.** `observe-testnet` believes the RPC endpoint's answers. To remove that trust,
   point `PREAVIS_TESTNET_RPC` at a node you run.
6. **A bad policy.** If the owner allowlists an attacker's account, the attacker is a payee. Read the
   policy with the `mandate` view before trusting a mandate.
7. **Implicit accounts.** A transfer to a 64-hex implicit account that does not exist yet creates it,
   so the transfer succeeds. Only named accounts that do not exist make a transfer fail.

8. **The agent's choices inside the policy.** A prompt-injected model can propose anything. The
   contract refuses what is outside the policy (`npm run agent`: the injected payment to the attacker
   is `DENIED UNLISTED`), but a model fooled into paying an allowlisted vendor's fake invoice within
   its caps would be paid, or held for the owner above the agent's own limit. The invoice hash in each
   request lets the owner see which invoice a payment was for.
9. **The callback's own failure.** If `on_transfer` itself fails, the charge stays frozen and visible
   (VERIFY 27). There is no method to unfreeze it; that amount is lost to the budget, not paid twice.

## Why on-chain

The checks run in the contract that holds the money, not on the agent operator's server. Anyone,
including a payee or a judge, can read the policy and every attempt from the chain, and nobody can skip
the checks, the operator included, once the mandate is locked.
