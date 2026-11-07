# The challenge mandate: a design note (never published, retired)

**Status: not published; retired on 2026-11-07.** Its key was never handed to anyone. The key
was then deleted from the agent account on chain, and the mandate is closed.

## What it was meant to be

The idea was a second locked mandate on NEAR testnet whose agent function-call key would be
published. Anyone holding the key could call `request_spend` as often as they liked. The question
was whether anyone could make the mandate pay an account outside its two allowlisted payees, or
wedge it.

| | |
|---|---|
| mandate | [`c1.preavis-challenge-1794034196.testnet`](https://testnet.nearblocks.io/address/c1.preavis-challenge-1794034196.testnet), locked (0 access keys), code = `res/preavis.wasm` |
| agent | `c1-agent.preavis-challenge-1794034196.testnet`, function-call key `ed25519:G2n9MjoKRNacK9ubYGdLQP4Kyjr1xYgv584k6X4igj1z` (`request_spend` on the mandate only) |
| allowlisted payees | `c1-vendor…` and `c1-hosting…`, at most 1 NEAR each, 1.2 NEAR in total |
| without the owner | at most 0.05 NEAR per window; above that, a request waits for the owner |
| per window (144,000 blocks, about a day) | at most 10 requests that pass the checks (paid or held), and 40 stored denials |
| public record | [`testnet/challenge.json`](../testnet/challenge.json) |

## What an independent review found

**With one shared key, one holder can take every window's 10 request slots at no budget cost.** At
the start of each window, the holder sends 10 requests to an allowlisted payee, each above the
per-window limit. Each request passes the checks and is held for the owner, who approves nothing.
A held request reserves no budget, but it does use one of the window's slots. Every other
player's in-policy request is then refused `EXHAUSTED` until the window resets, and the holder
repeats this in the next window. That takes about 10 transactions a day.

No code change to a locked mandate with a single shared key prevents this:

- The limits are first come, first served by design.
- Raising the slot count only raises the number of transactions the holder needs.
- Requiring a minimum amount turns the grab into payments to our own payees, which costs the
  holder nothing.

A fair challenge would need per-player keys, or a relayer that rate-limits each player. That is a
different design.

The review also found that the shared key's gas allowance could be spent in about 230 calls with
very large arguments, not the tens of thousands of calls that our measurement with small
arguments suggested.

Neither finding makes the mandate pay anyone outside its allowlist. Out-of-policy requests are
refused before the slot check, and the contract's per-window limits are what `npm run verify`
claims 29 and 30 check. But a challenge that one player can block for everyone else is not a fair
challenge. So the key was not published, and the mandate was retired.

## Retired on 2026-11-07

`npm run challenge-retire` did two things:

1. The agent's offline full-access key deleted the function-call key:
   [`95y8ARUx4CjZ6HkTBuLWk5x6xP68UZ6zJQ6G3ktUYKPA`](https://testnet.nearblocks.io/txns/95y8ARUx4CjZ6HkTBuLWk5x6xP68UZ6zJQ6G3ktUYKPA).
2. The owner closed the mandate:
   [`F9QHYYWiJRXtvGTiAspDkybLGQLzp9zJi4qxoCWLoDa5`](https://testnet.nearblocks.io/txns/F9QHYYWiJRXtvGTiAspDkybLGQLzp9zJi4qxoCWLoDa5).
   The close sent 6.304011 NEAR to `refund_to` (`c1-treasury…`) and nothing to anyone else. The
   rest of the balance is the storage stake.

Before the retirement, the contract had recorded no request besides our own calibration call, and
nothing had been paid.

`npm run observe-testnet` checks the retired state on every run:

- the function-call key is gone;
- the deletion and the close are re-read from the chain;
- the mandate reports `closed`;
- its balance is explained by the refund alone.

It ends:

```text
  retired, never published; attempts: 0 / paid to attackers: 0
```

## What is left in the repository

- [`tests/challenge.rs`](../tests/challenge.rs): `npm run challenge-deploy` and
  `npm run challenge-retire`.
- [`scripts/challenge-try.mjs`](../scripts/challenge-try.mjs): it would have built and signed one
  `request_spend` with the published key, read from the environment.
- The record in [`testnet/challenge.json`](../testnet/challenge.json).

They stay as the record of what was built and how it ended. Nothing in `npm run verify`,
`attack-local`, `agent` or `owner` uses them.
