# The standing challenge mandate (prepared, not published)

A second locked testnet mandate meant to be attacked by anyone: its agent's function-call key would
be published, and anyone holding it may call `request_spend` as often as they like. The question
is whether they can make the mandate pay anyone outside its two allowlisted payees, or wedge it.

**Status: deployed and locked; the key is NOT published.** Its secret half exists only in the
ignored `.credentials/testnet/challenge/c1/` on our build host. Publishing it is the owner's
decision, after a gate: an attack worker holds the key for at least 6 hours on testnet and two
reviewers attack this design. Any way found to pay anyone outside the mandate, or to wedge it,
keeps it private. The README section for players is drafted on the branch `challenge-key`, with
the key as a placeholder.

| | |
|---|---|
| mandate | [`c1.preavis-challenge-1794034196.testnet`](https://testnet.nearblocks.io/address/c1.preavis-challenge-1794034196.testnet), locked (0 access keys), code = `res/preavis.wasm` |
| agent | `c1-agent.preavis-challenge-1794034196.testnet` |
| the key to publish | `ed25519:G2n9MjoKRNacK9ubYGdLQP4Kyjr1xYgv584k6X4igj1z` (public half): function-call, `request_spend` on the mandate only, allowance 8 NEAR |
| allowlisted payees | `c1-vendor…` and `c1-hosting…`, at most 1 NEAR each, 1.2 NEAR in total |
| without the owner | at most 0.05 NEAR per day; above that a request waits for the owner, who will not approve anything during the challenge |
| per day (144,000 blocks) | at most 10 requests that pass the checks (paid or held) and 40 stored denials |
| expires | block 279,381,902, about 2026-11-27 03:40 UTC; then anyone may `close()` it, which pays only `refund_to` (`c1-treasury…`) |
| public record | [`testnet/challenge.json`](../testnet/challenge.json); live state: `npm run observe-testnet` |

## How a player tries

One command, Node only, no NEAR CLI and no account of their own:

```sh
PREAVIS_CHALLENGE_KEY='<the published key>' npm run challenge -- <payee> <amount in NEAR>
PREAVIS_CHALLENGE_KEY='<the published key>' npm run challenge -- --args '<any request_spend JSON>' --gas <Tgas>
```

[`scripts/challenge-try.mjs`](../scripts/challenge-try.mjs) builds and signs the transaction itself
(borsh, ed25519 from Node's `crypto`). It refuses a key whose public half is not the one in the
record, then prints what the contract answered and every transfer the mandate sent. Tested against
the retired `t1` mandate with a temporary key: `contract panicked: CLOSED`, `transfers from the
mandate: none` (transaction `BsfYqkpCymmbGHonKKAgDbid2LhabKBBnedvxJgzPFwJ`). The key is read from the
environment, never from the command line.

## Exhaustion is per day, and visible

Every limit that a flood can use up resets with the block height, so a flood exhausts only the
current window:

- **Request slots.** Each period of `period_blocks` = 144,000 blocks (24.6 h at the 616 ms per
  block we measured since deployment) takes at most `max_payments` = 10 requests that pass the
  checks. Past that, every request is denied `EXHAUSTED` until the period's last block; the first
  block of the next period starts a fresh window. Claim 30 of `npm run verify` checks this at the
  edge itself: requests landing before `resets_at` are `EXHAUSTED`, the first one after settles.
- **Stored denials.** At most `max_denials` = 40 denials are stored per period; the rest are only
  counted (`denials_not_stored`). Each still emits its NEP-297 event.
- **The agent's own budget.** 0.05 NEAR per period without the owner. A held request reserves
  nothing (its approval is checked against the caps at that time, claim 32), so a pile of held
  requests blocks nothing.
- **Storage.** `new` refused to start unless the balance covered 500 bytes for every slot in every
  period until `expires_at`: (10 + 40) × 20 periods = 1,000 records. A flood cannot run the mandate
  out of storage.
- **Caps.** 20 periods × 0.05 NEAR = 1.0 NEAR, at most each payee's cap (1 NEAR) and below the
  total (1.2 NEAR). So even an attacker who spends every day's budget on the allowlisted payees,
  in policy, cannot exhaust a cap before the mandate expires.

The state is explicit. `mandate()` returns `period`, `period_start`, `resets_at`,
`period_payments`, `period_denials`, `period_committed`, `exhausted` and `expired`, computed at the
current block height, so a view call shows a new window even before anyone sends a request in it.
`npm run observe-testnet` prints it with the reset as a time:

```text
OK    challenge: today's window 0: 0/10 payments, 1/40 denials stored, 0.000000 NEAR of 0.050000 NEAR committed
      the window resets at block 276740925 (about 2026-11-08 07:29 UTC); a flood exhausts only this window
      expires at block 279381902 (about 2026-11-27 03:42 UTC); after that anyone may close() it, which refunds c1-treasury.preavis-challenge-1794034196.testnet
```

When exhausted, the first line ends `; EXHAUSTED` and the second reads `EXHAUSTED: no more requests
are accepted until block <h> (about <time>); then a fresh window opens`.

## The counter

`npm run observe-testnet` ends its challenge section with

```text
  attempts so far: 0 / paid to attackers: 0
```

- **attempts so far** = stored attempts + `denials_not_stored` − 1 (our calibration call at
  deployment). These are the requests the contract processed. A call that panics (bad arguments,
  `EXPIRED`) or that NEAR refuses before execution leaves no record and is not counted; the
  allowance line next to it shows those too, as allowance used.
- **paid to attackers** is computed twice, independently:
  1. from the stored attempts: any `SETTLED`, `PENDING` or `AWAITING_APPROVAL` record whose payee
     is not allowlisted;
  2. from the balance: the mandate's balance may fall only by what the stored attempts say was
     paid to the allowlist (it rises with the contract's share of the gas the calls burn). Any
     shortfall counts as paid to attackers.

  The check fails (exit 1) unless both are zero. The negative control's sixth fixture raises the
  recorded start balance by 1 NEAR and the observer fails on it.

## The key's gas allowance, sized for the judging window

A function-call key spends its account's NEAR on gas, up to its allowance. Here that is 8 NEAR,
and the agent account holds 8.1 NEAR. Measured at deployment, with the key itself, on one denied
`request_spend` at the most gas a caller can attach (300 Tgas; transaction
`vh4Ya4rMKzr5G6DnbaidBqFgJUYdUWf7A4vJbcyteRt`):

- **in flight:** 0.300920 NEAR is charged against the allowance while the call runs (the prepaid
  gas at a pessimistic price);
- **after the refund:** 0.000221 NEAR stays spent. The refund lands a few blocks later. A call burns
  about the same gas whatever is attached.

The math:

- 8 NEAR / 0.000221 NEAR ≈ **36,000 calls** before the allowance is gone.
- From deployment (2026-11-07) to expiry (11-27) is 20 days, about 1,800 calls a day. For the
  judging window alone (11-15 to 11-25, 10 days) it is 3,600 calls a day, one every 24 seconds
  around the clock. At most 10 calls a day can pass the checks; they also run a transfer and a
  callback, which costs more gas but stays bounded.
- 8 NEAR / 0.300920 NEAR ≈ **26 max-gas calls in flight at once.** A 27th concurrent call is
  refused (`NotEnoughAllowance`) until refunds land a few blocks later; nothing is lost.

What this does not cover: a deliberate flood. At 10 transactions a second, 36,000 calls last
about an hour. That spends our gas, not the mandate's money. The mandate's state and balance are
unaffected, and its caps and windows still hold. But the key stops working for everyone, and the
remedy is a rotation (below). This is the "gas griefing" the challenge rules put out of scope; it
is a liveness cost for us, not a way to get paid. Players sharing one key also share its nonce: if
two players send at the same moment, one may see `InvalidNonce` and must resend. near-cli fetches
the current nonce on each call.

## No shared state with `npm run verify`

The challenge mandate has its own owner account (`preavis-challenge-1794034196.testnet`), its own
subaccounts and keys, its own credential directory and its own record (`testnet/challenge.json`).
Nothing in `npm run verify`, `attack-local`, `agent` or `owner` reads that record or touches
testnet: they run in a local sandbox created for the run. Whatever happens to the challenge
(flooded, exhausted, allowance spent, closed), `npm run verify` is unaffected.

`npm run observe-testnet` reads the challenge in a separate section after the recorded run. To
skip it, run `node scripts/observe-testnet.mjs testnet/receipts.json none`.

## Key rotation

When to rotate: the allowance is spent, the key must be withdrawn, or a design flaw needs a new
mandate. Since the policy is fixed and the mandate locked, a rotation is always a new mandate with a
new agent and a new key. Steps, from the repository root on the host that holds `.credentials/`:

1. **Deploy the next challenge** under a new label (fresh agent sub-account, new function-call
   key, new locked mandate):
   ```sh
   o=preavis-challenge-1794034196.testnet
   PREAVIS_CHALLENGE_LABEL=c2 PREAVIS_OWNER_CREDENTIALS=.credentials/testnet/$o/$o.json \
     PREAVIS_CHALLENGE_RECORD=target/challenge-c2.json npm run challenge-deploy
   ```
   About 10 testnet NEAR is needed: the faucet gives that to a new owner if
   `PREAVIS_OWNER_CREDENTIALS` is unset, and `PREAVIS_FUNDERS=<credential files>` tops up an
   existing one.
2. **Retire the old one**: the old agent's offline full-access key deletes the published key (from
   then on nobody can call the old mandate), and the owner closes it (the balance minus storage
   goes to its `refund_to`):
   ```sh
   PREAVIS_OWNER_CREDENTIALS=.credentials/testnet/$o/$o.json npm run challenge-retire
   ```
3. **Regenerate the links**: `cp target/challenge-c2.json testnet/challenge.json`, then
   `npm run observe-testnet`. Replace the mandate, agent and key in the README challenge section
   and in this file, then commit and push. Hand the new key to players through the same channel as
   before.

Measured on testnet with a throwaway label `t1` (2026-11-07): deploy and lock **26 s** (31 s with
`cargo`), retire **5 s**. With the observer run (13 s) and the README edit and push, a rotation
takes **about 3 minutes**, plus a faucet request if a fresh owner is used. The retired mandate stays
on chain, closed; its record stays in git history.
