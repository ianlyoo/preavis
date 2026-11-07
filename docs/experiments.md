# Experiments

What was tried, what happened. Raw logs stayed on the build machine; the final run is in
`testnet/run.txt` and `testnet/receipts.json`.

## Testnet runs (`npm run execute-testnet`)

| Run | Owner | Outcome | Cause / change |
|---|---|---|---|
| 1 | `keydare-1794023961.testnet` | **failed** after claim 3 of 21 | `archival-rpc.testnet.near.org`: "this client has exceeded the rate limit" (HTTP 429). Switched to FastNEAR. |
| 2 | `keydare-1794024392.testnet` | **failed** before deploy (3 s) | the rename missed the then-untracked `tests/testnet.rs`, which still read `res/keydare.wasm` |
| 3 | `keydare-1794024392.testnet` (reused via `PREAVIS_OWNER_CREDENTIALS`) | 21/21 held, 104 s | first full run |
| 4 | `preavis-1794024516.testnet` | 21/21 held, 106 s | fresh faucet owner under the new name |
| 5 | `preavis-1794024709.testnet` | 21/21 held, 107 s | rerun after script fixes |
| 6 | `preavis-1794025012.testnet` | 21/21 held, 107 s | **recorded run**: the agent's full-access key kept offline, firing claim added |

All accounts are testnet, funded only by the faucet helper. Keys are in the ignored `.credentials/`.

## Mutation tests (does `npm run verify` notice a broken contract?)

| Mutation in `src/lib.rs` | Result |
|---|---|
| remove the `TOTAL_CAP` check | verify fails at the total-cap claim |
| release the charge twice in the failed-transfer callback | verify fails at the release-once claim |

First attempt: both mutations were hidden because the prebuilt-hash check failed first and stopped the
run. The hash comparison moved into its own test (`prebuilt_matches_source`), so a mutation now fails
the claim it breaks.

## Stranger dry run

Fresh `git clone` of commit `394b98b` in an `ubuntu:24.04` container, with only the apt packages and
rustup that the README lists: `npm run verify` green (24 claims at that commit) in 1 min 41 s,
downloads and compilation included. The prebuilt wasm hash matched the container's own build.

## RPC probes (2026-11-07)

| Endpoint | Result |
|---|---|
| `https://archival-rpc.testnet.near.org` | 429 under the test's request rate |
| `https://test.rpc.fastnear.com` | used by runs 2–6 without a rate-limit error |
| `https://archival-rpc.testnet.fastnear.com` | worked for `observe-testnet` (`tx` lookups of the recorded run) |

## Storage per attempt

Testnet mandate after the recorded run: storage 396,358 bytes, of which 393,202 are code, so about
3.1 KB for the state and 12 attempts, roughly 250 bytes (0.0025 NEAR of stake) per attempt. Used in
THREAT_MODEL.md, "Not stopped" 4.
