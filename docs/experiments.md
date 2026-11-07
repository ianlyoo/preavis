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

| 7 | `preavis-1794028144.testnet` | 24/24 held, 122 s | v0.9 trial run, locked; used to develop the stricter observer |
| 8 | `preavis-1794029520.testnet` | 24/24 held, 129 s | **v0.9 recorded run**, locked (`mandate.preavis-1794029520.testnet`) |

Runs 1–6 are v0.8 (21 claims, not locked). All accounts are testnet, funded only by the faucet
helper. Keys are in the ignored `.credentials/`.

## Mutation tests (does `npm run verify` notice a broken contract?)

| Mutation in `src/lib.rs` | Result |
|---|---|
| remove the `TOTAL_CAP` check | verify fails at the total-cap claim |
| release the charge twice in the failed-transfer callback | verify fails at the release-once claim |

First attempt: both mutations were hidden because the prebuilt-hash check failed first and stopped the
run. The hash comparison moved into its own test, so a mutation now fails the claim it breaks. Since
v0.9 the claims run against the committed wasm by default; mutations of `src/` need
`PREAVIS_WASM=source npm run verify`.

## Stranger dry runs

Fresh `git clone` of commit `394b98b` in an `ubuntu:24.04` container, with only the apt packages and
rustup that the README lists: `npm run verify` green (24 claims at that commit) in 1 min 41 s,
downloads and compilation included. The prebuilt wasm hash matched the container's own build.

An outside run on an arm64 host failed this check in v0.8: the native build there differs from the x86_64 build
(host triple in Cargo's crate metadata). That led to the pinned Docker build and the split between
`verify` and `verify:reproducible` in v0.9.

v0.9: fresh clone of the `v0.9` tag in a clean `ubuntu:24.04` container on x86_64: __FRESH_RESULT__

## RPC probes (2026-11-07)

| Endpoint | Result |
|---|---|
| `https://archival-rpc.testnet.near.org` | 429 under the test's request rate |
| `https://test.rpc.fastnear.com` | used by runs 2–6 without a rate-limit error |
| `https://archival-rpc.testnet.fastnear.com` | worked for `observe-testnet` (`tx` lookups of the recorded run) |

## Storage per attempt

v0.8 testnet mandate after its recorded run: about 3.1 KB of state for 12 attempts, roughly 250 bytes
per attempt, unbounded in number. v0.9 measures the worst case in the sandbox (64-character request
ID and invoice, longest payee): the largest single record grew storage by 399 bytes, inside the 500
bytes per slot that `new` reserves. Five denials past `max_denials` left storage unchanged.

## Agent recording

`agent/transcript.json`: Qwen2.5-7B-Instruct, Q4_K_M GGUF from the Qwen repository on Hugging Face,
served on CPU by the llama.cpp server image (`ghcr.io/ggml-org/llama.cpp:server`, digest in the
transcript), temperature 0. About 30 s for three invoices. It paid the vendor invoice, submitted the
hosting invoice (held for the owner) and declined the injected one. A scripted stand-in that obeys
every invoice (`agent/hijacked-model.mjs`) sends the attacker's payment; the contract denies it.
